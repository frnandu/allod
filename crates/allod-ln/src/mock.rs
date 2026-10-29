//! In-memory backend for tests (SPEC §16.2): scripted payments, events and channels.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use crate::backend::{BackendError, BackendResult, LightningBackend};
use crate::types::*;
use crate::NodeInfo;
use allod_types::{Network, NodeSummary};

/// How the mock answers outgoing payments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendBehaviour {
    /// Succeed at once with this routing fee.
    Succeed { fee_msat: u64 },
    /// Fail at once.
    Fail,
    /// Stay pending until [`MockBackend::complete`] is called.
    Hang,
    /// Submit the payment, then lose the RPC response.
    SubmitThenTimeout,
}

#[derive(Default)]
struct State {
    payments: HashMap<String, LnPayment>,
    invoices: HashMap<String, (CreatedInvoice, Option<u64>, String)>, // hash -> (invoice, amount, preimage)
    hold_invoices: HashMap<String, String>,                           // hash -> invoice
    offers: HashMap<String, (CreatedOffer, Option<u64>)>,
    channels: Vec<Channel>,
    peers: Vec<Peer>,
    counter: u64,
    claimed: Vec<(String, String)>,
    failed: Vec<String>,
}

/// Mock Lightning backend.
pub struct MockBackend {
    state: Mutex<State>,
    events: broadcast::Sender<LnEvent>,
    pub send: Mutex<SendBehaviour>,
    pub height: Mutex<u32>,
    pub lightning_sat: Mutex<u64>,
    /// Whether the mock node supports BOLT12 (default true).
    pub bolt12: std::sync::atomic::AtomicBool,
    /// `max_fee_msat` of the latest send (BOLT11 or BOLT12).
    pub last_max_fee: Mutex<Option<u64>>,
    pub network: Network,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

impl MockBackend {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(256);
        MockBackend {
            state: Mutex::new(State::default()),
            events,
            send: Mutex::new(SendBehaviour::Succeed { fee_msat: 1_000 }),
            height: Mutex::new(100),
            lightning_sat: Mutex::new(1_000_000),
            last_max_fee: Mutex::new(None),
            bolt12: std::sync::atomic::AtomicBool::new(true),
            network: Network::Regtest,
        }
    }

    fn next_id(&self) -> String {
        let mut s = self.state.lock().unwrap();
        s.counter += 1;
        format!("{:064x}", s.counter)
    }

    pub fn emit(&self, ev: LnEvent) {
        let _ = self.events.send(ev);
    }

    /// Simulates a payer paying one of our BOLT11 invoices (auto-claimed).
    pub fn pay_invoice(&self, payment_hash: &str, amount_msat: u64) -> LnPayment {
        let preimage = self.state.lock().unwrap().invoices.get(payment_hash).map(|i| i.2.clone());
        let p = LnPayment {
            id: payment_hash.to_string(),
            kind: LnPaymentKind::Bolt11 { payment_hash: payment_hash.into(), preimage },
            direction: Direction::Inbound,
            status: LnStatus::Succeeded,
            amount_msat: Some(amount_msat),
            fee_msat: None,
            updated_at: 1,
        };
        self.state.lock().unwrap().payments.insert(p.id.clone(), p.clone());
        self.emit(LnEvent::PaymentReceived { payment: p.clone() });
        p
    }

    /// Simulates a payer locking in an HTLC for one of our hold invoices.
    pub fn hold_htlc(
        &self,
        payment_hash: &str,
        amount_msat: u64,
        claim_deadline: u32,
    ) -> LnPayment {
        let p = LnPayment {
            id: payment_hash.to_string(),
            kind: LnPaymentKind::Bolt11 { payment_hash: payment_hash.into(), preimage: None },
            direction: Direction::Inbound,
            status: LnStatus::Pending,
            amount_msat: Some(amount_msat),
            fee_msat: None,
            updated_at: 1,
        };
        self.state.lock().unwrap().payments.insert(p.id.clone(), p.clone());
        self.emit(LnEvent::PaymentClaimable {
            payment: p.clone(),
            claim_deadline: Some(claim_deadline),
            claimable_amount_msat: amount_msat,
        });
        p
    }

    /// The node fails a held incoming HTLC on its own (deadline reached, or failed
    /// outside Allod).
    pub fn fail_inbound(&self, payment_hash: &str, emit: bool) {
        let p = {
            let mut st = self.state.lock().unwrap();
            let Some(p) = st.payments.get_mut(payment_hash) else { return };
            p.status = LnStatus::Failed;
            p.clone()
        };
        if emit {
            self.emit(LnEvent::PaymentFailed { payment: p, reason: Some("HtlcTimeout".into()) });
        }
    }

    /// Simulates a payer paying one of our offers.
    pub fn pay_offer(
        &self,
        offer_id: &str,
        amount_msat: u64,
        payer_note: Option<String>,
    ) -> LnPayment {
        let preimage = self.next_id();
        let hash = sha256_hex(&hex::decode(&preimage).unwrap());
        let p = LnPayment {
            id: self.next_id(),
            kind: LnPaymentKind::Bolt12Offer {
                payment_hash: Some(hash),
                preimage: Some(preimage),
                offer_id: offer_id.into(),
                payer_note,
                quantity: None,
            },
            direction: Direction::Inbound,
            status: LnStatus::Succeeded,
            amount_msat: Some(amount_msat),
            fee_msat: None,
            updated_at: 1,
        };
        self.state.lock().unwrap().payments.insert(p.id.clone(), p.clone());
        self.emit(LnEvent::PaymentReceived { payment: p.clone() });
        p
    }

    /// Completes a pending outgoing payment.
    pub fn complete(&self, payment_id: &str, success: bool, fee_msat: u64) {
        let mut s = self.state.lock().unwrap();
        let Some(p) = s.payments.get_mut(payment_id) else { return };
        if success {
            p.status = LnStatus::Succeeded;
            p.fee_msat = Some(fee_msat);
        } else {
            p.status = LnStatus::Failed;
        }
        let p = p.clone();
        drop(s);
        if success {
            let preimage = p.kind.preimage().map(str::to_string);
            self.emit(LnEvent::PaymentSuccessful {
                payment: p,
                preimage,
                bolt12_invoice: Some("lni1mock".into()),
            });
        } else {
            self.emit(LnEvent::PaymentFailed { payment: p, reason: Some("RouteNotFound".into()) });
        }
    }

    /// Hold invoices claimed through `bolt11_claim_for_id`: (payment_id, preimage).
    pub fn claimed(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().claimed.clone()
    }

    /// Hold invoices failed through `bolt11_fail_for_id`.
    pub fn failed(&self) -> Vec<String> {
        self.state.lock().unwrap().failed.clone()
    }

    pub fn add_channel(&self, ch: Channel) {
        self.state.lock().unwrap().channels.push(ch);
    }

    /// Mock invoices look like `lnbcrtmock<amount>h<hash>`.
    /// Parses a mock invoice: `lnbcrtmock<amt|any>h<hash>` (regtest), `lnbcmock…`
    /// (mainnet), and `x` instead of `h` for an expired one.
    pub fn parse_invoice(invoice: &str) -> Option<(Option<u64>, String)> {
        let rest =
            invoice.strip_prefix("lnbcrtmock").or_else(|| invoice.strip_prefix("lnbcmock"))?;
        let (amount, hash) = rest.split_once('h').or_else(|| rest.split_once('x'))?;
        let amount = if amount == "any" { None } else { Some(amount.parse().ok()?) };
        Some((amount, hash.to_string()))
    }

    /// An already expired regtest invoice.
    pub fn make_expired_invoice(amount_msat: Option<u64>, payment_hash: &str) -> String {
        Self::make_invoice(amount_msat, payment_hash).replacen('h', "x", 1)
    }

    /// A mainnet invoice (wrong network on regtest).
    pub fn make_mainnet_invoice(amount_msat: Option<u64>, payment_hash: &str) -> String {
        Self::make_invoice(amount_msat, payment_hash).replacen("lnbcrtmock", "lnbcmock", 1)
    }

    pub fn make_invoice(amount_msat: Option<u64>, payment_hash: &str) -> String {
        format!(
            "lnbcrtmock{}h{payment_hash}",
            amount_msat.map(|a| a.to_string()).unwrap_or_else(|| "any".into())
        )
    }

    fn outgoing(&self, kind: LnPaymentKind, amount_msat: Option<u64>) -> String {
        let id = match &kind {
            LnPaymentKind::Bolt11 { payment_hash, .. } => payment_hash.clone(),
            _ => self.next_id(),
        };
        let behaviour = *self.send.lock().unwrap();
        let mut p = LnPayment {
            id: id.clone(),
            kind,
            direction: Direction::Outbound,
            status: LnStatus::Pending,
            amount_msat,
            fee_msat: None,
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        if let LnPaymentKind::Bolt11 { preimage, .. }
        | LnPaymentKind::Bolt12Offer { preimage, .. } = &mut p.kind
        {
            preimage.get_or_insert_with(|| format!("{:064x}", 0xfeed));
        }
        self.state.lock().unwrap().payments.insert(id.clone(), p);
        match behaviour {
            SendBehaviour::Succeed { fee_msat } => self.complete(&id, true, fee_msat),
            SendBehaviour::Fail => self.complete(&id, false, 0),
            SendBehaviour::Hang => {}
            SendBehaviour::SubmitThenTimeout => {}
        }
        id
    }
}

#[async_trait]
impl LightningBackend for MockBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            bolt12: self.bolt12.load(std::sync::atomic::Ordering::Relaxed),
            hold_invoices: true,
            splicing: true,
            force_close: true,
            payer_proof: true,
            offer_issuer: false,
            single_use_offers: false,
        }
    }
    fn subscribe(&self) -> broadcast::Receiver<LnEvent> {
        self.events.subscribe()
    }
    async fn node_info(&self) -> BackendResult<NodeInfo> {
        Ok(NodeInfo {
            summary: NodeSummary {
                pubkey: "02".to_string() + &"11".repeat(32),
                block_height: *self.height.lock().unwrap(),
                block_hash: "00".repeat(32),
            },
            network: Some(self.network),
            alias: Some("mock".into()),
            version: "mock".into(),
            listening_addresses: vec!["0.0.0.0:9735".into()],
            announcement_addresses: vec![],
            node_uris: vec![],
        })
    }
    async fn balances(&self) -> BackendResult<Balances> {
        Ok(Balances {
            onchain_total_sat: 50_000,
            onchain_spendable_sat: 40_000,
            anchor_reserve_sat: 10_000,
            lightning_total_sat: *self.lightning_sat.lock().unwrap(),
            ..Default::default()
        })
    }
    async fn list_channels(&self) -> BackendResult<Vec<Channel>> {
        Ok(self.state.lock().unwrap().channels.clone())
    }
    async fn open_channel(&self, req: OpenChannelReq) -> BackendResult<String> {
        if req.node_pubkey.len() != 66 {
            return Err(BackendError::InvalidRequest("invalid pubkey".into()));
        }
        let id = self.next_id();
        self.add_channel(Channel {
            channel_id: id.clone(),
            user_channel_id: id.clone(),
            counterparty_node_id: req.node_pubkey,
            short_channel_id: None,
            funding_txid: None,
            capacity_sat: req.amount_sat.unwrap_or(40_000),
            outbound_capacity_msat: req.amount_sat.unwrap_or(40_000) * 1000,
            inbound_capacity_msat: 0,
            is_outbound: true,
            is_channel_ready: false,
            is_usable: false,
            is_announced: req.announce,
            confirmations: Some(0),
            confirmations_required: Some(6),
            fee_base_msat: req.fee_base_msat,
            fee_ppm: req.fee_ppm,
            cltv_expiry_delta: Some(72),
            force_close_spend_delay: None,
        });
        Ok(id)
    }
    async fn close_channel(
        &self,
        ucid: &str,
        _cp: &str,
        _force: bool,
        _r: Option<String>,
    ) -> BackendResult<()> {
        let mut s = self.state.lock().unwrap();
        let before = s.channels.len();
        s.channels.retain(|c| c.user_channel_id != ucid);
        if s.channels.len() == before {
            return Err(BackendError::InvalidRequest("unknown channel".into()));
        }
        Ok(())
    }
    async fn update_channel_fees(
        &self,
        ucid: &str,
        _cp: &str,
        fees: ChannelFees,
    ) -> BackendResult<()> {
        let mut s = self.state.lock().unwrap();
        let ch = s
            .channels
            .iter_mut()
            .find(|c| c.user_channel_id == ucid)
            .ok_or_else(|| BackendError::InvalidRequest("unknown channel".into()))?;
        if fees.fee_base_msat.is_some() {
            ch.fee_base_msat = fees.fee_base_msat;
        }
        if fees.fee_ppm.is_some() {
            ch.fee_ppm = fees.fee_ppm;
        }
        Ok(())
    }
    async fn splice_in(&self, _u: &str, _c: &str, _a: u64) -> BackendResult<()> {
        Ok(())
    }
    async fn splice_out(
        &self,
        _u: &str,
        _c: &str,
        _ad: Option<String>,
        _a: u64,
    ) -> BackendResult<()> {
        Ok(())
    }
    async fn list_peers(&self) -> BackendResult<Vec<Peer>> {
        Ok(self.state.lock().unwrap().peers.clone())
    }
    async fn connect_peer(&self, node_id: &str, address: &str, persist: bool) -> BackendResult<()> {
        self.state.lock().unwrap().peers.push(Peer {
            node_id: node_id.into(),
            address: address.into(),
            is_persisted: persist,
            is_connected: true,
        });
        Ok(())
    }
    async fn disconnect_peer(&self, node_id: &str) -> BackendResult<()> {
        self.state.lock().unwrap().peers.retain(|p| p.node_id != node_id);
        Ok(())
    }
    async fn onchain_receive(&self) -> BackendResult<String> {
        Ok("bcrt1qmockaddress".into())
    }
    async fn onchain_send(
        &self,
        _a: &str,
        _amt: OnchainAmount,
        _f: Option<u64>,
    ) -> BackendResult<String> {
        Ok("ab".repeat(32))
    }
    async fn onchain_bump_fee(&self, _p: &str, _f: Option<u64>) -> BackendResult<String> {
        Ok("cd".repeat(32))
    }
    async fn sign_message(&self, m: &[u8]) -> BackendResult<String> {
        Ok(format!("sig{}", m.len()))
    }
    async fn verify_signature(&self, m: &[u8], s: &str, _p: &str) -> BackendResult<bool> {
        Ok(s == format!("sig{}", m.len()))
    }
    async fn list_forwards(
        &self,
        _p: Option<String>,
    ) -> BackendResult<(Vec<Forward>, Option<String>)> {
        Ok((vec![], None))
    }

    async fn bolt11_receive(
        &self,
        amount_msat: Option<u64>,
        _d: InvoiceDescription,
        _e: u32,
    ) -> BackendResult<CreatedInvoice> {
        let preimage = self.next_id();
        let hash = sha256_hex(&hex::decode(&preimage).unwrap());
        let created = CreatedInvoice {
            invoice: Self::make_invoice(amount_msat, &hash),
            payment_hash: hash.clone(),
        };
        // Own invoices are visible as pending inbound payments that know their preimage.
        let p = LnPayment {
            id: hash.clone(),
            kind: LnPaymentKind::Bolt11 {
                payment_hash: hash.clone(),
                preimage: Some(preimage.clone()),
            },
            direction: Direction::Inbound,
            status: LnStatus::Pending,
            amount_msat,
            fee_msat: None,
            updated_at: 1,
        };
        let mut s = self.state.lock().unwrap();
        s.payments.insert(hash.clone(), p);
        s.invoices.insert(hash, (created.clone(), amount_msat, preimage));
        Ok(created)
    }
    async fn bolt11_receive_for_hash(
        &self,
        amount_msat: Option<u64>,
        _d: InvoiceDescription,
        _e: u32,
        hash: &str,
    ) -> BackendResult<String> {
        let invoice = Self::make_invoice(amount_msat, hash);
        self.state.lock().unwrap().hold_invoices.insert(hash.into(), invoice.clone());
        Ok(invoice)
    }
    async fn bolt11_claim_for_id(
        &self,
        payment_id: &str,
        _amt: u64,
        preimage: &str,
    ) -> BackendResult<()> {
        if sha256_hex(
            &hex::decode(preimage)
                .map_err(|_| BackendError::InvalidRequest("bad preimage".into()))?,
        ) != payment_id
        {
            return Err(BackendError::InvalidRequest("preimage does not match".into()));
        }
        let mut s = self.state.lock().unwrap();
        s.claimed.push((payment_id.into(), preimage.into()));
        if let Some(p) = s.payments.get_mut(payment_id) {
            p.status = LnStatus::Succeeded;
        }
        Ok(())
    }
    async fn bolt11_fail_for_id(&self, payment_id: &str) -> BackendResult<()> {
        let mut s = self.state.lock().unwrap();
        s.failed.push(payment_id.into());
        if let Some(p) = s.payments.get_mut(payment_id) {
            p.status = LnStatus::Failed;
        }
        Ok(())
    }
    async fn bolt11_send(
        &self,
        invoice: &str,
        amount_msat: Option<u64>,
        max_fee: Option<u64>,
    ) -> BackendResult<String> {
        *self.last_max_fee.lock().unwrap() = max_fee;
        let (inv_amount, hash) = Self::parse_invoice(invoice)
            .ok_or_else(|| BackendError::InvalidRequest("bad invoice".into()))?;
        Ok(self.outgoing(
            LnPaymentKind::Bolt11 { payment_hash: hash, preimage: None },
            inv_amount.or(amount_msat),
        ))
    }
    async fn decode_invoice(&self, invoice: &str) -> BackendResult<DecodedInvoice> {
        let (amount, hash) = Self::parse_invoice(invoice)
            .ok_or_else(|| BackendError::InvalidRequest("invalid invoice".into()))?;
        Ok(DecodedInvoice {
            payment_hash: hash,
            destination: "03".to_string() + &"22".repeat(32),
            amount_msat: amount,
            description: Some("mock".into()),
            description_hash: None,
            timestamp: 1,
            expiry_secs: 3600,
            network: if invoice.starts_with("lnbcmock") { "bitcoin" } else { "regtest" }.into(),
            is_expired: invoice
                .strip_prefix("lnbcrtmock")
                .or_else(|| invoice.strip_prefix("lnbcmock"))
                .is_some_and(|r| !r.contains('h')),
        })
    }
    async fn bolt12_receive(
        &self,
        _d: &str,
        amount_msat: Option<u64>,
        _e: Option<u32>,
    ) -> BackendResult<CreatedOffer> {
        let id = self.next_id();
        let created = CreatedOffer { offer: format!("lno1mock{id}"), offer_id: id.clone() };
        self.state.lock().unwrap().offers.insert(id, (created.clone(), amount_msat));
        Ok(created)
    }
    async fn decode_offer(&self, offer: &str) -> BackendResult<DecodedOffer> {
        let id = offer
            .strip_prefix("lno1mock")
            .ok_or_else(|| BackendError::InvalidRequest("invalid offer".into()))?;
        let amount = self.state.lock().unwrap().offers.get(id).and_then(|o| o.1);
        Ok(DecodedOffer {
            offer_id: id.into(),
            description: Some("mock offer".into()),
            issuer: None,
            amount_msat: amount,
            currency: None,
            absolute_expiry: None,
            // `lno1mockmain…` is a mainnet offer, `lno1mockexp…` an expired one.
            chains: if id.starts_with("main") { vec!["bitcoin".into()] } else { vec![] },
            is_expired: id.starts_with("exp"),
        })
    }
    async fn bolt12_send(
        &self,
        offer: &str,
        amount_msat: Option<u64>,
        payer_note: Option<String>,
        max_fee: Option<u64>,
    ) -> BackendResult<String> {
        *self.last_max_fee.lock().unwrap() = max_fee;
        let id = offer
            .strip_prefix("lno1mock")
            .ok_or_else(|| BackendError::InvalidRequest("invalid offer".into()))?;
        // Every BOLT12 payment gets its own preimage and hash, as on a real node.
        let pre = self.next_id();
        let hash = sha256_hex(&hex::decode(&pre).unwrap_or_default());
        let payment_id = self.outgoing(
            LnPaymentKind::Bolt12Offer {
                payment_hash: Some(hash),
                preimage: Some(pre),
                offer_id: id.into(),
                payer_note,
                quantity: None,
            },
            amount_msat,
        );
        if *self.send.lock().unwrap() == SendBehaviour::SubmitThenTimeout {
            Err(BackendError::Timeout)
        } else {
            Ok(payment_id)
        }
    }
    async fn bolt12_payer_proof(&self, _p: &str, _pre: &str, _inv: &str) -> BackendResult<String> {
        Ok("lnp1mockproof".into())
    }
    async fn payment(&self, id: &str) -> BackendResult<Option<LnPayment>> {
        Ok(self.state.lock().unwrap().payments.get(id).cloned())
    }
    async fn list_payments(
        &self,
        _p: Option<String>,
    ) -> BackendResult<(Vec<LnPayment>, Option<String>)> {
        Ok((self.state.lock().unwrap().payments.values().cloned().collect(), None))
    }
}
