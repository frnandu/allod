//! Who a payment came from (or went to), read from what the payment carries:
//! NIP-57 zap requests, LUD-18 payer data, LUD-12 comments, BOLT12 payer notes and
//! NWC-06 `recipient_data`. Nothing here is verified: it is what the payer claims.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Counterparty {
    /// Nostr pubkey (hex), e.g. the author of a zap request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Lightning address or email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// zap | lnurl | bolt12 | contact
    pub source: &'static str,
}

fn hex_pubkey(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?.trim().to_ascii_lowercase();
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

fn text(v: Option<&Value>, max: usize) -> Option<String> {
    let s = v?.as_str()?.trim();
    (!s.is_empty()).then(|| s.chars().take(max).collect())
}

/// A NIP-57 zap request (kind 9734) as JSON.
fn zap(v: &Value, outgoing: bool) -> Option<Counterparty> {
    if v.get("kind").and_then(Value::as_u64) != Some(9734) {
        return None;
    }
    // Incoming: the zapper is the event author. Outgoing: the zapped user is the `p` tag.
    let pubkey = if outgoing {
        v.get("tags")?.as_array()?.iter().find_map(|t| {
            let t = t.as_array()?;
            (t.first()?.as_str()? == "p").then(|| hex_pubkey(t.get(1))).flatten()
        })
    } else {
        hex_pubkey(v.get("pubkey"))
    };
    Some(Counterparty {
        pubkey,
        name: None,
        identifier: None,
        comment: text(v.get("content"), 280),
        source: "zap",
    })
}

/// Reads the counterparty of a transaction, if the payment says anything about it.
pub fn of(
    direction: &str,
    description: &str,
    metadata: Option<&str>,
    payer_note: Option<&str>,
) -> Option<Counterparty> {
    let outgoing = direction == "outgoing";
    let meta: Option<Value> = metadata.and_then(|m| serde_json::from_str(m).ok());
    let meta = meta.as_ref().filter(|m| m.is_object());
    let comment = meta.and_then(|m| text(m.get("comment"), 280));

    // NIP-57: in metadata (NWC-06 `nostr`), or as the invoice description itself.
    let desc_json: Option<Value> = description
        .trim_start()
        .starts_with('{')
        .then(|| serde_json::from_str(description).ok())
        .flatten();
    if let Some(mut z) = meta
        .and_then(|m| m.get("nostr"))
        .and_then(|n| zap(n, outgoing))
        .or_else(|| desc_json.as_ref().and_then(|d| zap(d, outgoing)))
    {
        z.comment = z.comment.or(comment);
        return Some(z);
    }
    if let Some(m) = meta {
        // Our own record of who we paid (set by the Send screen for a contact).
        if outgoing {
            if let Some(r) = m.get("recipient") {
                return Some(Counterparty {
                    pubkey: hex_pubkey(r.get("pubkey")),
                    name: text(r.get("name"), 100),
                    identifier: text(
                        m.get("recipient_data").and_then(|d| d.get("identifier")),
                        200,
                    ),
                    comment,
                    source: "contact",
                });
            }
            if let Some(id) = text(m.get("recipient_data").and_then(|d| d.get("identifier")), 200) {
                return Some(Counterparty {
                    pubkey: None,
                    name: None,
                    identifier: Some(id),
                    comment,
                    source: "lnurl",
                });
            }
        } else if let Some(p) = m.get("payer_data").filter(|p| p.is_object()) {
            return Some(Counterparty {
                pubkey: hex_pubkey(p.get("pubkey")),
                name: text(p.get("name"), 100),
                identifier: text(p.get("email"), 200).or_else(|| text(p.get("identifier"), 200)),
                comment,
                source: "lnurl",
            });
        }
    }
    if !outgoing {
        if let Some(note) = payer_note.map(str::trim).filter(|n| !n.is_empty()) {
            return Some(Counterparty {
                pubkey: None,
                name: None,
                identifier: None,
                comment: Some(note.chars().take(280).collect()),
                source: "bolt12",
            });
        }
    }
    comment.map(|c| Counterparty {
        pubkey: None,
        name: None,
        identifier: None,
        comment: Some(c),
        source: "lnurl",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PK: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
    const PK2: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";

    #[test]
    fn zap_in_metadata_and_description() {
        let zap_req = format!(
            r#"{{"kind":9734,"pubkey":"{PK}","content":"great post","tags":[["p","{PK2}"],["amount","21000"]]}}"#
        );
        let meta = format!(r#"{{"nostr":{zap_req}}}"#);
        let c = of("incoming", "", Some(&meta), None).unwrap();
        assert_eq!(
            (c.pubkey.as_deref(), c.comment.as_deref(), c.source),
            (Some(PK), Some("great post"), "zap")
        );
        let c = of("incoming", &zap_req, None, None).unwrap();
        assert_eq!(c.pubkey.as_deref(), Some(PK));
        // Outgoing zap: the zapped user.
        let c = of("outgoing", "", Some(&meta), None).unwrap();
        assert_eq!(c.pubkey.as_deref(), Some(PK2));
    }

    #[test]
    fn lnurl_payer_data_comment_and_notes() {
        let meta = format!(
            r#"{{"payer_data":{{"name":"Satoshi","pubkey":"{PK}","email":"s@x.org"}},"comment":"thanks"}}"#
        );
        let c = of("incoming", "", Some(&meta), None).unwrap();
        assert_eq!(c.name.as_deref(), Some("Satoshi"));
        assert_eq!(c.identifier.as_deref(), Some("s@x.org"));
        assert_eq!(c.comment.as_deref(), Some("thanks"));
        let c = of("incoming", "", None, Some("for the coffee")).unwrap();
        assert_eq!((c.comment.as_deref(), c.source), (Some("for the coffee"), "bolt12"));
        let meta = r#"{"recipient_data":{"identifier":"bob@x.org"},"comment":"hi"}"#;
        let c = of("outgoing", "", Some(meta), None).unwrap();
        assert_eq!(
            (c.identifier.as_deref(), c.comment.as_deref()),
            (Some("bob@x.org"), Some("hi"))
        );
        let meta = format!(
            r#"{{"recipient":{{"pubkey":"{PK2}","name":"Bob"}},"recipient_data":{{"identifier":"bob@x.org"}}}}"#
        );
        let c = of("outgoing", "", Some(&meta), None).unwrap();
        assert_eq!(
            (c.pubkey.as_deref(), c.name.as_deref(), c.source),
            (Some(PK2), Some("Bob"), "contact")
        );
        assert!(of("incoming", "coffee", Some("{}"), None).is_none());
        assert!(of("incoming", "{not json", Some("nonsense"), None).is_none());
        // A bad pubkey is dropped, not passed on.
        let c =
            of("incoming", "", Some(r#"{"payer_data":{"pubkey":"zz","name":"X"}}"#), None).unwrap();
        assert!(c.pubkey.is_none());
    }
}
