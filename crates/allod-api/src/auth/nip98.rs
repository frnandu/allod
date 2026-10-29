//! NIP-98 HTTP auth: a kind 27235 event signed by the caller, bound to the URL,
//! method and body of one request (SPEC §11.1).

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use nostr::prelude::*;
use sha2::{Digest, Sha256};

/// Maximum clock difference between the event and the server, in seconds.
pub const MAX_SKEW_SECS: i64 = 60;

/// What the event must match.
pub struct Expect<'a> {
    /// Acceptable absolute URLs (the `u` tag must equal one of them).
    pub urls: &'a [String],
    /// HTTP method, e.g. "POST".
    pub method: &'a str,
    /// Raw request body; when non-empty the `payload` tag must be its SHA-256.
    pub body: &'a [u8],
    /// Current unix time.
    pub now: i64,
}

/// Why a NIP-98 header was rejected. Messages are safe to return to clients.
#[derive(Debug, PartialEq, Eq)]
pub enum Nip98Error {
    Missing,
    Malformed(&'static str),
    BadSignature,
    WrongKind,
    Stale,
    WrongUrl,
    WrongMethod,
    WrongPayload,
}

impl std::fmt::Display for Nip98Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = match self {
            Nip98Error::Missing => "missing Nostr authorization",
            Nip98Error::Malformed(why) => why,
            Nip98Error::BadSignature => "invalid event signature",
            Nip98Error::WrongKind => "event must be kind 27235",
            Nip98Error::Stale => "event created_at is too far from the server time",
            Nip98Error::WrongUrl => "event u tag does not match this request",
            Nip98Error::WrongMethod => "event method tag does not match this request",
            Nip98Error::WrongPayload => "event payload tag does not match the request body",
        };
        f.write_str(m)
    }
}

/// Verifies `Authorization: Nostr <base64(event json)>` and returns the signer's
/// x-only public key (hex).
pub fn verify(authorization: Option<&str>, expect: &Expect<'_>) -> Result<String, Nip98Error> {
    let value = authorization.ok_or(Nip98Error::Missing)?;
    let encoded = value
        .strip_prefix("Nostr ")
        .ok_or(Nip98Error::Malformed("authorization must use the Nostr scheme"))?
        .trim();
    if encoded.len() > 16 * 1024 {
        return Err(Nip98Error::Malformed("authorization event too large"));
    }
    let json = B64.decode(encoded).map_err(|_| Nip98Error::Malformed("invalid base64"))?;
    let json = std::str::from_utf8(&json).map_err(|_| Nip98Error::Malformed("invalid utf-8"))?;
    let event = Event::from_json(json).map_err(|_| Nip98Error::Malformed("invalid event json"))?;
    event.verify().map_err(|_| Nip98Error::BadSignature)?;

    if event.kind != Kind::HttpAuth {
        return Err(Nip98Error::WrongKind);
    }
    let created = i64::try_from(event.created_at.as_secs()).unwrap_or(i64::MAX);
    if (created - expect.now).abs() > MAX_SKEW_SECS {
        return Err(Nip98Error::Stale);
    }
    let tag = |name: &str| -> Option<String> {
        event.tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.len() >= 2 && s[0] == name).then(|| s[1].clone())
        })
    };
    let url = tag("u").ok_or(Nip98Error::WrongUrl)?;
    if !expect.urls.iter().any(|u| u == &url) {
        return Err(Nip98Error::WrongUrl);
    }
    let method = tag("method").ok_or(Nip98Error::WrongMethod)?;
    if !method.eq_ignore_ascii_case(expect.method) {
        return Err(Nip98Error::WrongMethod);
    }
    if !expect.body.is_empty() {
        let want = hex::encode(Sha256::digest(expect.body));
        match tag("payload") {
            Some(p) if p.eq_ignore_ascii_case(&want) => {}
            _ => return Err(Nip98Error::WrongPayload),
        }
    }
    Ok(event.pubkey.to_hex())
}

/// Test helper: builds a signed NIP-98 header value.
#[cfg(test)]
pub fn sign_header(keys: &Keys, url: &str, method: &str, body: &[u8], created_at: i64) -> String {
    let mut tags = vec![Tag::parse(["u", url]).unwrap(), Tag::parse(["method", method]).unwrap()];
    if !body.is_empty() {
        tags.push(Tag::parse(["payload", &hex::encode(Sha256::digest(body))]).unwrap());
    }
    let event = EventBuilder::new(Kind::HttpAuth, "")
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at as u64))
        .finalize(keys)
        .unwrap();
    format!("Nostr {}", B64.encode(event.as_json()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://127.0.0.1:8029/api/auth/login";
    const NOW: i64 = 1_800_000_000;

    fn expect<'a>(urls: &'a [String], body: &'a [u8]) -> Expect<'a> {
        Expect { urls, method: "POST", body, now: NOW }
    }

    #[test]
    fn accepts_valid_event() {
        let keys = Keys::generate();
        let body = br#"{"challenge":"ab"}"#;
        let h = sign_header(&keys, URL, "POST", body, NOW - 5);
        let urls = [URL.to_string()];
        assert_eq!(verify(Some(&h), &expect(&urls, body)).unwrap(), keys.public_key().to_hex());
    }

    #[test]
    fn rejects_each_mismatch() {
        let keys = Keys::generate();
        let body = br#"{"challenge":"ab"}"#;
        let urls = [URL.to_string()];
        let e = expect(&urls, body);
        let cases = [
            (sign_header(&keys, URL, "POST", body, NOW - 120), Nip98Error::Stale),
            (sign_header(&keys, URL, "POST", body, NOW + 120), Nip98Error::Stale),
            (
                sign_header(&keys, "http://evil/api/auth/login", "POST", body, NOW),
                Nip98Error::WrongUrl,
            ),
            (sign_header(&keys, URL, "GET", body, NOW), Nip98Error::WrongMethod),
            (sign_header(&keys, URL, "POST", b"other", NOW), Nip98Error::WrongPayload),
        ];
        for (header, err) in cases {
            assert_eq!(verify(Some(&header), &e), Err(err));
        }
        assert_eq!(verify(None, &e), Err(Nip98Error::Missing));
        assert!(matches!(verify(Some("Bearer x"), &e), Err(Nip98Error::Malformed(_))));
        assert!(matches!(verify(Some("Nostr !!!"), &e), Err(Nip98Error::Malformed(_))));
    }

    #[test]
    fn rejects_tampered_event() {
        let keys = Keys::generate();
        let h = sign_header(&keys, URL, "POST", b"", NOW);
        let json =
            String::from_utf8(B64.decode(h.strip_prefix("Nostr ").unwrap()).unwrap()).unwrap();
        let tampered = json.replace(URL, "http://127.0.0.1:8029/api/auth/loginX");
        let header = format!("Nostr {}", B64.encode(tampered));
        let urls = ["http://127.0.0.1:8029/api/auth/loginX".to_string()];
        assert_eq!(verify(Some(&header), &expect(&urls, b"")), Err(Nip98Error::BadSignature));
    }

    #[test]
    fn rejects_other_kinds() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::TextNote, "hi")
            .tags([Tag::parse(["u", URL]).unwrap(), Tag::parse(["method", "POST"]).unwrap()])
            .custom_created_at(Timestamp::from(NOW as u64))
            .finalize(&keys)
            .unwrap();
        let header = format!("Nostr {}", B64.encode(event.as_json()));
        let urls = [URL.to_string()];
        assert_eq!(verify(Some(&header), &expect(&urls, b"")), Err(Nip98Error::WrongKind));
    }
}
