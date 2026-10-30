//! NIP-47 method handlers (SPEC §8.4). M4: get_info, get_balance, get_budget,
//! pay_invoice, make_invoice, lookup_invoice.

use std::time::Duration;

use allod_core::apps::effective_methods;
use allod_core::wallet::Tx;
use allod_core::wallet::{InvoiceRequest, PayRequest};
use allod_core::{App, Wallet};
use allod_ln::LightningBackend;
use serde_json::{json, Map, Value};

use crate::error::{nwc_error_from, Code, NwcError};

pub type HandlerResult = Result<Value, NwcError>;

/// How long `pay_invoice` waits for a final result after the wallet returned PENDING.
pub const PAY_WAIT: Duration = Duration::from_secs(300);

/// NIP-47 transaction object plus the NWC-09 fields `transaction_id` and `payment_type`.
pub fn tx_json(tx: &Tx) -> Value {
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        if !v.is_null() {
            m.insert(k.into(), v);
        }
    };
    put("type", json!(tx.direction));
    put("state", json!(tx.state.to_ascii_lowercase()));
    put("transaction_id", json!(tx.public_id));
    put("payment_type", json!(tx.payment_type));
    put("invoice", json!(tx.payment_request));
    put("description", json!(tx.description));
    put("description_hash", json!(tx.description_hash));
    // The preimage proves payment: only for settled rows (an unpaid invoice's preimage
    // stays with the wallet).
    if tx.state == "SETTLED" {
        put("preimage", json!(tx.preimage));
    }
    put("payment_hash", json!(tx.payment_hash));
    put("amount", json!(tx.amount_msat));
    put("fees_paid", json!(tx.fee_msat));
    put("created_at", json!(tx.created_at));
    put("expires_at", json!(tx.expires_at));
    put("settled_at", json!(tx.settled_at));
    if let Some(meta) = tx.metadata.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok()) {
        put("metadata", meta);
    }
    Value::Object(m)
}

fn bad(msg: impl Into<String>) -> NwcError {
    NwcError::new(Code::BadRequest, msg)
}

fn opt_u64(params: &Value, key: &str) -> Result<Option<u64>, NwcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            v.as_u64().map(Some).ok_or_else(|| bad(format!("{key} must be a positive integer")))
        }
    }
}

fn opt_str<'a>(params: &'a Value, key: &str) -> Result<Option<&'a str>, NwcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad(format!("{key} must be a string"))),
    }
}

/// NWC-06 metadata: a JSON object of at most 4096 bytes (SPEC §8.8).
fn metadata(params: &Value) -> Result<Option<String>, NwcError> {
    match params.get("metadata") {
        None | Some(Value::Null) => Ok(None),
        Some(v @ Value::Object(_)) => {
            let s = v.to_string();
            if s.len() > 4096 {
                return Err(bad("metadata is limited to 4096 bytes"));
            }
            Ok(Some(s))
        }
        Some(_) => Err(bad("metadata must be a JSON object")),
    }
}

pub async fn get_info(app: &App, node: &dyn LightningBackend, bolt12: bool) -> HandlerResult {
    let methods = effective_methods(&app.scopes, bolt12);
    let notif = app.has_scope("notifications");
    let mut out = json!({
        "methods": methods,
        "notifications": if notif { allod_core::wallet::outbox::TYPES.to_vec() } else { vec![] },
        "extensions": allod_core::apps::extensions(&methods, notif),
        "metadata": {"id": app.id, "name": app.name},
    });
    // App metadata (NWC-06) plus id and name (SPEC §8.4 #1).
    if let Value::Object(meta) = &app.metadata {
        let m = out["metadata"].as_object_mut().expect("object");
        for (k, v) in meta {
            m.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    if app.has_scope("get_info") {
        let info = node.node_info().await.map_err(|e| nwc_error_from(e.into()))?;
        let o = out.as_object_mut().unwrap();
        o.insert("alias".into(), json!(info.alias.unwrap_or_default()));
        o.insert("color".into(), json!(""));
        o.insert("pubkey".into(), json!(info.summary.pubkey));
        o.insert("network".into(), json!(info.network.map(|n| n.nip47_name()).unwrap_or("")));
        o.insert("block_height".into(), json!(info.summary.block_height));
        o.insert("block_hash".into(), json!(info.summary.block_hash));
    }
    Ok(out)
}

pub async fn get_balance(app: &App, wallet: &Wallet) -> HandlerResult {
    let id = Some(app.id);
    let balance = wallet.app_balance(id).await.map_err(nwc_error_from)?;
    Ok(json!({"balance": balance}))
}

pub async fn get_budget(app: &App, wallet: &Wallet) -> HandlerResult {
    // A connection that cannot pay has no budget to report (as in Alby Hub).
    if !app.has_scope("pay_invoice") {
        return Ok(json!({}));
    }
    match wallet.budget(app.id).await.map_err(nwc_error_from)? {
        Some(b) => Ok(serde_json::to_value(b).unwrap_or_default()),
        None => Ok(json!({})),
    }
}

pub async fn pay_invoice(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let invoice = opt_str(params, "invoice")?.ok_or_else(|| bad("invoice is required"))?;
    // NIP-47 pay_invoice takes a BOLT11 invoice only; offers and addresses go through `pay`.
    match allod_core::wallet::bip321::parse(invoice).map(|p| p.instruction) {
        Ok(allod_core::wallet::bip321::Instruction::Bolt11(_)) => {}
        _ => return Err(bad("invoice must be a BOLT11 invoice")),
    }
    let tx = wallet
        .pay(PayRequest {
            app_id: Some(app.id),
            payment: invoice.trim().to_string(),
            amount_msat: opt_u64(params, "amount")?,
            max_fee_msat: None,
            payer_note: None,
            metadata: metadata(params)?,
        })
        .await
        .map_err(nwc_error_from)?;
    let tx = wait_final(wallet, tx).await?;
    match tx.state.as_str() {
        "SETTLED" => {
            Ok(json!({"preimage": tx.preimage.unwrap_or_default(), "fees_paid": tx.fee_msat}))
        }
        "FAILED" => Err(NwcError::new(
            Code::PaymentFailed,
            tx.failure_reason.unwrap_or_else(|| "payment failed".into()),
        )),
        other => {
            Err(NwcError::new(Code::Other, format!("payment is still {}", other.to_lowercase())))
        }
    }
}

/// Waits until an outgoing payment is settled or failed.
async fn wait_final(wallet: &Wallet, tx: Tx) -> Result<Tx, NwcError> {
    if tx.state != "PENDING" {
        return Ok(tx);
    }
    let mut events = wallet.subscribe();
    let deadline = tokio::time::Instant::now() + PAY_WAIT;
    loop {
        let cur = wallet.transaction(&tx.public_id).await.map_err(nwc_error_from)?;
        if cur.state != "PENDING" {
            return Ok(cur);
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(cur);
        }
        // Wake on any wallet event, or poll every 2 s.
        let _ =
            tokio::time::timeout((deadline - now).min(Duration::from_secs(2)), events.recv()).await;
    }
}

pub async fn make_invoice(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let amount = opt_u64(params, "amount")?.ok_or_else(|| bad("amount is required"))?;
    if amount == 0 {
        return Err(bad("amount must be greater than 0"));
    }
    let description = opt_str(params, "description")?.unwrap_or_default().to_string();
    if description.len() > 640 {
        return Err(bad("description is limited to 640 characters"));
    }
    let description_hash = opt_str(params, "description_hash")?.map(str::to_ascii_lowercase);
    if let Some(h) = &description_hash {
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad("description_hash must be 32 bytes of hex"));
        }
    }
    let expiry = opt_u64(params, "expiry")?
        .map(|e| u32::try_from(e).map_err(|_| bad("expiry is too large")))
        .transpose()?;
    let tx = wallet
        .create_invoice(InvoiceRequest {
            app_id: Some(app.id),
            amount_msat: Some(amount),
            description,
            description_hash,
            expiry_secs: expiry,
            metadata: metadata(params)?,
            ..Default::default()
        })
        .await
        .map_err(nwc_error_from)?;
    Ok(tx_json(&tx))
}

pub async fn lookup_invoice(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let hash = opt_str(params, "payment_hash")?;
    let invoice = opt_str(params, "invoice")?;
    if hash.is_none() && invoice.is_none() {
        return Err(bad("payment_hash or invoice is required"));
    }
    let tx = wallet
        .lookup_for_app(app.id, app.isolated, hash, invoice.map(str::trim))
        .await
        .map_err(nwc_error_from)?;
    Ok(tx_json(&tx))
}

// ---------------------------------------------------------------- M5 (NWC extensions)

fn sha256_matches(preimage: &str, hash: &str) -> bool {
    use sha2::{Digest, Sha256};
    hex::decode(preimage)
        .map(|p| hex::encode(Sha256::digest(p)) == hash.to_ascii_lowercase())
        .unwrap_or(false)
}

/// The payment type a row reports: internal rows report what was paid (SPEC §8.4 #17).
fn effective_type(tx: &Tx) -> &'static str {
    match tx.payment_type.as_str() {
        "bolt12" => "bolt12",
        "internal" if tx.offer_id.is_some() => "bolt12",
        _ => "bolt11",
    }
}

/// NWC-09 payment record (also used for NWC-05 list items): the common envelope with
/// `details`, plus the NIP-47 fields for clients that only know the legacy shape.
pub fn payment_record(tx: &Tx) -> Value {
    let mut v = tx_json(tx);
    let ptype = effective_type(tx);
    let o = v.as_object_mut().expect("object");
    o.insert("payment_type".into(), json!(ptype));
    o.insert("updated_at".into(), json!(tx.updated_at));
    if let Some(r) = &tx.failure_reason {
        o.insert("failure_reason".into(), json!(r));
    }
    // The preimage is returned only when it matches the payment hash.
    let preimage = match (&tx.preimage, &tx.payment_hash) {
        (Some(p), Some(h)) if tx.state == "SETTLED" && sha256_matches(p, h) => Some(p.clone()),
        _ => None,
    };
    if preimage.is_none() {
        o.remove("preimage");
    }
    let mut d = Map::new();
    let mut put = |k: &str, v: Value| {
        if !v.is_null() {
            d.insert(k.into(), v);
        }
    };
    put("payment_hash", json!(tx.payment_hash));
    put("invoice", json!(tx.payment_request));
    put("preimage", json!(preimage));
    if !tx.description.is_empty() {
        put("description", json!(tx.description));
    }
    if ptype == "bolt11" {
        put("description_hash", json!(tx.description_hash));
    } else {
        put("offer_id", json!(tx.offer_id));
        put("offer", json!(tx.offer));
        put("payer_proof", json!(tx.payer_proof));
        put("payer_note", json!(tx.payer_note));
        put("quantity", json!(tx.quantity));
    }
    o.insert("details".into(), Value::Object(d));
    v
}

fn opt_bool(params: &Value, key: &str) -> Result<Option<bool>, NwcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(format!("{key} must be a boolean"))),
    }
}

fn opt_i64(params: &Value, key: &str) -> Result<Option<i64>, NwcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_i64().map(Some).ok_or_else(|| bad(format!("{key} must be an integer"))),
    }
}

fn hex32(s: &str, what: &str) -> Result<String, NwcError> {
    let s = s.trim().to_ascii_lowercase();
    if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(s)
    } else {
        Err(bad(format!("{what} must be 32 bytes of hex")))
    }
}

pub async fn list_transactions(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let limit = opt_i64(params, "limit")?.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(bad("limit must be between 1 and 100"));
    }
    let q = allod_core::wallet::AppTxQuery {
        from: opt_i64(params, "from")?.unwrap_or(0),
        until: opt_i64(params, "until")?.unwrap_or(now),
        limit,
        offset: opt_i64(params, "offset")?.unwrap_or(0).max(0),
        unpaid: opt_bool(params, "unpaid")?.unwrap_or(false),
        direction: opt_str(params, "type")?.map(str::to_string),
    };
    let (rows, total) =
        wallet.list_for_app(app.id, app.isolated, &q).await.map_err(nwc_error_from)?;
    Ok(
        json!({"transactions": rows.iter().map(payment_record).collect::<Vec<_>>(), "total_count": total}),
    )
}

pub async fn lookup_payment(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let tid = opt_str(params, "transaction_id")?;
    let ptype = opt_str(params, "payment_type")?;
    let lookup = params.get("lookup").filter(|v| !v.is_null());
    let top_hash = opt_str(params, "payment_hash")?;
    let top_inv = opt_str(params, "invoice")?;
    let forms = usize::from(tid.is_some())
        + usize::from(ptype.is_some() || lookup.is_some())
        + usize::from(top_hash.is_some() || top_inv.is_some());
    if forms != 1 {
        return Err(bad(
            "use exactly one of transaction_id, payment_hash/invoice, or payment_type + lookup",
        ));
    }
    let mut conds: Vec<(&str, String)> = Vec::new();
    let mut want_type: Option<&str> = None;
    if let Some(t) = tid {
        conds.push(("public_id", t.to_string()));
    } else if top_hash.is_some() || top_inv.is_some() {
        want_type = Some("bolt11");
        if let Some(h) = top_hash {
            conds.push(("payment_hash", hex32(h, "payment_hash")?));
        }
        if let Some(i) = top_inv {
            conds.push(("payment_request", i.trim().to_string()));
        }
    } else {
        let (Some(pt), Some(l)) = (ptype, lookup) else {
            return Err(bad("payment_type and lookup go together"));
        };
        if !l.is_object() {
            return Err(bad("lookup must be an object"));
        }
        let lh = opt_str(l, "payment_hash")?;
        let li = opt_str(l, "invoice")?;
        match pt {
            "bolt11" => {
                if lh.is_none() && li.is_none() {
                    return Err(bad("lookup needs payment_hash or invoice"));
                }
                if let Some(h) = lh {
                    conds.push(("payment_hash", hex32(h, "payment_hash")?));
                }
                if let Some(i) = li {
                    conds.push(("payment_request", i.trim().to_string()));
                }
            }
            "bolt12" => {
                let offer = opt_str(l, "offer_id")?;
                match (lh, li, offer) {
                    (Some(h), None, None) => {
                        conds.push(("payment_hash", hex32(h, "payment_hash")?))
                    }
                    (None, Some(i), None) => conds.push(("payment_request", i.trim().to_string())),
                    (h, None, Some(o)) => {
                        conds.push(("offer_id", hex32(o, "offer_id")?));
                        if let Some(h) = h {
                            conds.push(("payment_hash", hex32(h, "payment_hash")?));
                        }
                    }
                    _ => {
                        return Err(bad(
                            "use one of payment_hash, invoice, or offer_id (+ payment_hash)",
                        ))
                    }
                }
            }
            other => {
                return Err(NwcError::new(
                    Code::UnsupportedPaymentType,
                    format!("payment_type {other} is not supported"),
                ))
            }
        }
        want_type = Some(if pt == "bolt12" { "bolt12" } else { "bolt11" });
    }
    let mut rows =
        wallet.find_for_app(app.id, app.isolated, &conds).await.map_err(nwc_error_from)?;
    if let Some(t) = want_type {
        rows.retain(|r| effective_type(r) == t);
    }
    match rows.as_slice() {
        [] => Err(NwcError::new(Code::NotFound, "payment not found")),
        [one] => Ok(payment_record(one)),
        _ => {
            Err(NwcError::new(Code::MultipleMatches, "several payments match; use transaction_id"))
        }
    }
}

pub async fn make_hold_invoice(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let amount =
        opt_u64(params, "amount")?.filter(|a| *a > 0).ok_or_else(|| bad("amount is required"))?;
    let hash = hex32(
        opt_str(params, "payment_hash")?.ok_or_else(|| bad("payment_hash is required"))?,
        "payment_hash",
    )?;
    if params.get("min_cltv_expiry_delta").is_some_and(|v| !v.is_null()) {
        // ldk-server cannot set it (SPEC O-4); refuse rather than ignore it.
        return Err(bad("min_cltv_expiry_delta is not supported"));
    }
    let description = opt_str(params, "description")?.unwrap_or_default().to_string();
    let description_hash =
        opt_str(params, "description_hash")?.map(|h| hex32(h, "description_hash")).transpose()?;
    let expiry = opt_u64(params, "expiry")?.map(|e| u32::try_from(e).unwrap_or(u32::MAX));
    let tx = wallet
        .create_hold_invoice(
            InvoiceRequest {
                app_id: Some(app.id),
                amount_msat: Some(amount),
                description,
                description_hash,
                expiry_secs: expiry,
                metadata: metadata(params)?,
                ..Default::default()
            },
            &hash,
        )
        .await
        .map_err(nwc_error_from)?;
    Ok(tx_json(&tx))
}

pub async fn settle_hold_invoice(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let pre = opt_str(params, "preimage")?.ok_or_else(|| bad("preimage is required"))?;
    wallet.settle_hold(Some(app.id), pre).await.map_err(nwc_error_from)?;
    Ok(json!({}))
}

pub async fn cancel_hold_invoice(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let hash = opt_str(params, "payment_hash")?.ok_or_else(|| bad("payment_hash is required"))?;
    wallet.cancel_hold(Some(app.id), hash).await.map_err(nwc_error_from)?;
    Ok(json!({}))
}

pub async fn make_offer(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let amount = opt_u64(params, "amount")?;
    if amount == Some(0) {
        return Err(bad("amount must be greater than 0"));
    }
    let description = opt_str(params, "description")?.unwrap_or_default().to_string();
    if amount.is_some() && description.trim().is_empty() {
        return Err(bad("an offer with an amount needs a description"));
    }
    if opt_str(params, "issuer")?.is_some_and(|i| !i.is_empty()) {
        return Err(bad("issuer not supported"));
    }
    if opt_bool(params, "single_use")? == Some(true) {
        return Err(bad("single_use not supported"));
    }
    let o = wallet
        .create_offer(allod_core::wallet::OfferRequest {
            app_id: Some(app.id),
            amount_msat: amount,
            description,
            expires_at: opt_i64(params, "expires_at")?,
            metadata: metadata(params)?,
        })
        .await
        .map_err(nwc_error_from)?;
    let mut v = json!({
        "offer_id": o.offer_id, "offer": o.offer, "amount": o.amount_msat,
        "single_use": o.single_use, "created_at": o.created_at,
    });
    if let Some(d) = o.description.filter(|d| !d.is_empty()) {
        v["description"] = json!(d);
    }
    if let Some(e) = o.expires_at {
        v["expires_at"] = json!(e);
    }
    Ok(v)
}

pub async fn pay(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    use allod_core::wallet::bip321::{parse, Instruction};
    let payment =
        opt_str(params, "payment")?.ok_or_else(|| bad("payment is required"))?.trim().to_string();
    let parsed = parse(&payment).map_err(nwc_error_from)?;
    let bolt12_ok = wallet.capabilities().bolt12;
    let instruction = match parsed.instruction {
        Instruction::Bolt11(_) => "bolt11",
        Instruction::Bolt12(_) if bolt12_ok => "bolt12",
        Instruction::Bolt12(_) if parsed.bolt11_fallback.is_some() => "bolt11",
        Instruction::Bolt12(_) => {
            return Err(NwcError::new(
                Code::UnsupportedPaymentInstruction,
                "this wallet cannot pay BOLT12 offers and the URI has no lightning invoice",
            ))
        }
        Instruction::Onchain { .. } | Instruction::Lnurl(_) => {
            return Err(NwcError::new(
                Code::UnsupportedPaymentInstruction,
                "no lightning or lno instruction in the URI",
            ))
        }
    };
    let note = opt_str(params, "payer_note")?.filter(|n| !n.is_empty()).map(str::to_string);
    if note.is_some() && instruction == "bolt11" {
        return Err(bad("payer_note needs a BOLT12 offer"));
    }
    if note.as_ref().is_some_and(|n| n.len() > 512) {
        return Err(bad("payer_note is limited to 512 characters"));
    }
    let max_fee = opt_u64(params, "max_fee")?;
    let tx = wallet
        .pay(PayRequest {
            app_id: Some(app.id),
            payment,
            amount_msat: opt_u64(params, "amount")?,
            max_fee_msat: max_fee,
            payer_note: note,
            metadata: metadata(params)?,
        })
        .await
        .map_err(nwc_error_from)?;
    let tx = wait_final(wallet, tx).await?;
    if tx.state == "FAILED" {
        let reason = tx.failure_reason.unwrap_or_else(|| "payment failed".into());
        // LDK searches routes within `max_total_routing_fee_msat`; with a cap, finding no
        // route means none fit the budget, and nothing was sent (NWC-321).
        if max_fee.is_some() && reason.contains("RouteNotFound") {
            return Err(NwcError::new(
                Code::FeeLimitExceeded,
                format!("no route within max_fee ({} msat)", max_fee.unwrap_or_default()),
            ));
        }
        return Err(NwcError::new(Code::PaymentFailed, reason));
    }
    let mut v = json!({
        "transaction_id": tx.public_id,
        "state": tx.state.to_ascii_lowercase(),
        "instruction_type": instruction,
        "amount": tx.amount_msat,
        "fees_paid": tx.fee_msat,
        "created_at": tx.created_at,
    });
    for (k, val) in [
        ("payment_hash", json!(tx.payment_hash)),
        ("preimage", json!(tx.preimage)),
        ("payer_proof", json!(tx.payer_proof)),
        ("settled_at", json!(tx.settled_at)),
    ] {
        if !val.is_null() {
            v[k] = val;
        }
    }
    Ok(v)
}

pub async fn receive(app: &App, wallet: &Wallet, params: &Value) -> HandlerResult {
    let amount = opt_u64(params, "amount")?;
    if amount == Some(0) {
        return Err(bad("amount must be greater than 0"));
    }
    let description = opt_str(params, "description")?.unwrap_or_default().to_string();
    let jit = opt_bool(params, "jit")?.unwrap_or(false);
    let max_lsp_fee_msat = opt_u64(params, "max_lsp_fee_msat")?;
    if !jit && max_lsp_fee_msat.is_some() {
        return Err(bad("max_lsp_fee_msat requires jit"));
    }
    let (uri, tx) = wallet
        .receive(InvoiceRequest {
            app_id: Some(app.id),
            amount_msat: amount,
            description,
            metadata: metadata(params)?,
            jit,
            max_lsp_fee_msat,
            ..Default::default()
        })
        .await
        .map_err(nwc_error_from)?;
    let mut v = json!({"bip321": uri});
    if let Some(tx) = tx {
        v["transaction_id"] = json!(tx.public_id);
    }
    Ok(v)
}

pub async fn sign_message(node: &dyn LightningBackend, params: &Value) -> HandlerResult {
    let message = opt_str(params, "message")?.ok_or_else(|| bad("message is required"))?;
    let signature =
        node.sign_message(message.as_bytes()).await.map_err(|e| nwc_error_from(e.into()))?;
    Ok(json!({"message": message, "signature": signature}))
}

/// `multi_pay_invoice`: up to 50 invoices, 8 at a time. Returns `(d tag, result)` pairs.
pub async fn multi_pay_invoice(
    app: &App,
    wallet: &Wallet,
    node: &dyn LightningBackend,
    params: &Value,
) -> Result<Vec<(String, HandlerResult)>, NwcError> {
    use futures_util::stream::{self, StreamExt};
    let items = params
        .get("invoices")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("invoices is required"))?;
    if items.is_empty() || items.len() > 50 {
        return Err(bad("invoices must have 1 to 50 entries"));
    }
    let mut jobs = Vec::new();
    for it in items {
        let inv =
            opt_str(it, "invoice")?.ok_or_else(|| bad("each entry needs an invoice"))?.to_string();
        let d = match opt_str(it, "id")? {
            Some(id) => id.to_string(),
            None => node
                .decode_invoice(&inv)
                .await
                .map(|d| d.payment_hash)
                .unwrap_or_else(|_| inv.clone()),
        };
        let mut p = json!({"invoice": inv});
        if let Some(a) = it.get("amount") {
            p["amount"] = a.clone();
        }
        if let Some(m) = it.get("metadata") {
            p["metadata"] = m.clone();
        }
        jobs.push((d, p));
    }
    Ok(stream::iter(jobs)
        .map(|(d, p)| async move { (d, pay_invoice(app, wallet, &p).await) })
        .buffer_unordered(8)
        .collect()
        .await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_limits() {
        assert!(metadata(&json!({"metadata": {"a": 1}})).unwrap().is_some());
        assert!(metadata(&json!({"metadata": "x"})).is_err());
        let big = "x".repeat(5000);
        assert!(metadata(&json!({"metadata": {"a": big}})).is_err());
        assert!(metadata(&json!({})).unwrap().is_none());
    }
}
