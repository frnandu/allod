//! Email for the gateway account key through uid.ovh (SPEC §12.8): every npub has the
//! address `npub…@uid.ovh`; the uid.ovh bridge delivers incoming mail as NIP-59 gift
//! wraps (kind 1059) to the DM relays the key lists (kind 10050), around a kind 1301
//! rumor whose content is the raw RFC 822 message (nostr-mail). Allod uses it to
//! receive LNVPS's email verification without asking the owner for an address.

use std::time::Duration;

use base64::Engine as _;
use nostr::nips::nip59::UnwrappedGift;
use nostr::prelude::*;
use nostr_sdk::prelude::{Client, SignerAuthenticator};

/// The bridge's domain.
pub const MAIL_DOMAIN: &str = "uid.ovh";
/// Where the bridge delivers the key's mail (nostr-mail's recommended DM relays).
pub const DM_RELAYS: &[&str] = &["wss://relay.nmail.li", "wss://auth.nostr1.com"];
/// Where the relay lists are published, so the bridge finds them (indexers included).
const LIST_RELAYS: &[&str] = &[
    "wss://relay.nmail.li",
    "wss://purplepag.es",
    "wss://indexer.coracle.social",
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
];
/// Kind of a nostr-mail email rumor.
const EMAIL_KIND: u16 = 1301;
const TIMEOUT: Duration = Duration::from_secs(15);

/// The key's email address.
pub fn address(keys: &Keys) -> Result<String, String> {
    let npub = keys.public_key().to_bech32().map_err(|e| e.to_string())?;
    Ok(format!("{npub}@{MAIL_DOMAIN}"))
}

async fn client(keys: &Keys, relays: &[&str]) -> Client {
    let client = Client::builder().authenticator(SignerAuthenticator::new(keys.clone())).build();
    for r in relays {
        let _ = client.add_relay(*r).await;
    }
    client.connect().await;
    let _ = client.try_connect().timeout(Duration::from_secs(8)).await;
    client
}

/// Publishes the key's DM relay list (kind 10050) and relay list (kind 10002), so
/// the bridge knows where to deliver.
pub async fn publish_relay_lists(keys: &Keys) -> Result<(), String> {
    let dm = EventBuilder::new(Kind::Custom(10050), "")
        .tags(DM_RELAYS.iter().filter_map(|r| Tag::parse(["relay", r]).ok()))
        .finalize(keys)
        .map_err(|e| e.to_string())?;
    let nip65 = EventBuilder::new(Kind::RelayList, "")
        .tags(DM_RELAYS.iter().filter_map(|r| Tag::parse(["r", r]).ok()))
        .finalize(keys)
        .map_err(|e| e.to_string())?;
    let all: Vec<&str> = LIST_RELAYS.iter().chain(DM_RELAYS).copied().collect();
    let client = client(keys, &all).await;
    let mut ok = false;
    for ev in [&dm, &nip65] {
        ok |= client.send_event(ev).await.is_ok_and(|o| !o.success.is_empty());
    }
    client.disconnect().await;
    if ok {
        Ok(())
    } else {
        Err("no relay accepted the mailbox's relay lists".into())
    }
}

/// Emails delivered to the key since `since` (raw RFC 822 text).
pub async fn fetch(keys: &Keys, since: i64) -> Result<Vec<String>, String> {
    let client = client(keys, DM_RELAYS).await;
    // Gift wraps carry a randomized time up to two days in the past (NIP-59).
    let filter = Filter::new()
        .kind(Kind::GiftWrap)
        .pubkey(keys.public_key())
        .since(Timestamp::from((since - 3 * 86_400).max(0) as u64));
    let events = client.fetch_events(filter).timeout(TIMEOUT).await;
    client.disconnect().await;
    let events = events.map_err(|e| format!("cannot read the mailbox: {e}"))?;
    let mut out = Vec::new();
    for ev in events.into_iter() {
        let Ok(g) = UnwrappedGift::from_gift_wrap(keys, &ev) else { continue };
        if g.rumor.kind == Kind::Custom(EMAIL_KIND) && !g.rumor.content.is_empty() {
            out.push(g.rumor.content);
        }
    }
    Ok(out)
}

/// The text an email carries, with quoted-printable and base64 parts decoded, so a
/// link split over lines or encoded is found whole.
fn readable(mime: &str) -> String {
    let mime = mime.replace("\r\n", "\n");
    // Quoted-printable: soft line breaks and the escapes that matter in URLs.
    let mut text =
        mime.replace("=\n", "").replace("=3D", "=").replace("=3d", "=").replace("=26", "&");
    // base64 parts: blocks of base64 lines after a blank line.
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut block = String::new();
    for line in mime.lines().chain(std::iter::once("")) {
        let l = line.trim();
        if !l.is_empty()
            && l.len() >= 16
            && l.bytes().all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
        {
            block.push_str(l);
            continue;
        }
        if block.len() >= 32 {
            if let Ok(bytes) = b64.decode(block.as_bytes()) {
                text.push('\n');
                text.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        block.clear();
    }
    text
}

/// The token of an LNVPS email-verification message (`…verify-email?token=…`).
pub fn lnvps_token(mime: &str) -> Option<String> {
    let text = readable(mime);
    if !text.to_ascii_lowercase().contains("lnvps") {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("token=") {
        let start = from + i + "token=".len();
        let before = &lower[from..from + i];
        let token: String = text[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || "-_.~%".contains(*c))
            .collect();
        // Only a link about verifying the email.
        let line_start =
            before.rfind(|c: char| c.is_whitespace() || c == '"' || c == '<').map_or(0, |p| p + 1);
        let url = &lower[from + line_start..start];
        if token.len() >= 8 && (url.contains("verif") || url.contains("email")) {
            return Some(token);
        }
        from = start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let keys = Keys::parse("0000000000000000000000000000000000000000000000000000000000000001")
            .unwrap();
        let a = address(&keys).unwrap();
        assert!(a.starts_with("npub1") && a.ends_with("@uid.ovh"), "{a}");
    }

    #[test]
    fn tokens_in_plain_quoted_printable_and_base64_mail() {
        let plain = "From: LNVPS <no-reply@lnvps.net>\r\nSubject: Verify your email\r\n\r\n\
                     Click https://api.lnvps.net/api/v1/account/verify-email?token=AbC123-xyz_456 to verify.";
        assert_eq!(lnvps_token(plain).as_deref(), Some("AbC123-xyz_456"));
        let qp = "From: no-reply@lnvps.net\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n\
                  <a href=3D\"https://lnvps.net/account/verify-email?tok=\r\nen=3D9f8e7d6c5b4a\">Verify</a>";
        assert_eq!(lnvps_token(qp).as_deref(), Some("9f8e7d6c5b4a"));
        let body = base64::engine::general_purpose::STANDARD
            .encode("Please verify: https://lnvps.net/verify-email?token=ZZZZ11112222");
        let b64 = format!(
            "From: LNVPS <hi@lnvps.net>\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}\r\n"
        );
        assert_eq!(lnvps_token(&b64).as_deref(), Some("ZZZZ11112222"));
        // Not from LNVPS, or no verification link: nothing.
        assert_eq!(lnvps_token("From: x@y.z\r\n\r\nverify-email?token=12345678"), None);
        assert_eq!(
            lnvps_token("From: no-reply@lnvps.net\r\n\r\nhttps://x/?unsubscribe&token=12345678"),
            None
        );
    }
}
