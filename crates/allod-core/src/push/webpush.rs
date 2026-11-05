//! Web Push message encryption (RFC 8291, `aes128gcm` from RFC 8188) and VAPID
//! (RFC 8292), SPEC §8.9.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;
use zeroize::Zeroizing;

/// Record size written in the `aes128gcm` header. One record holds the whole message.
pub const RECORD_SIZE: u32 = 4096;
/// Header size: salt (16), record size (4), key id length (1), key id (65).
const HEADER_LEN: usize = 86;
/// AES-GCM tag size.
const TAG_LEN: usize = 16;

/// Why a message could not be encrypted.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PushCryptoError {
    #[error("p256dh is not an uncompressed P-256 public key")]
    BadPublicKey,
    #[error("auth is not a 16-byte secret")]
    BadAuth,
    #[error("message is too large for one record")]
    TooLarge,
}

/// Decodes base64url, with or without padding (browsers send it without).
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    B64.decode(s.trim().trim_end_matches('=')).ok()
}

/// Checks a subscription's keys without encrypting anything.
pub fn check_keys(p256dh: &str, auth: &str) -> Result<(), PushCryptoError> {
    ua_public(p256dh)?;
    auth_secret(auth)?;
    Ok(())
}

fn ua_public(p256dh: &str) -> Result<(Vec<u8>, PublicKey), PushCryptoError> {
    let bytes = b64_decode(p256dh).ok_or(PushCryptoError::BadPublicKey)?;
    if bytes.len() != 65 || bytes[0] != 4 {
        return Err(PushCryptoError::BadPublicKey);
    }
    let key = PublicKey::from_sec1_bytes(&bytes).map_err(|_| PushCryptoError::BadPublicKey)?;
    Ok((bytes, key))
}

fn auth_secret(auth: &str) -> Result<Vec<u8>, PushCryptoError> {
    b64_decode(auth).filter(|a| a.len() == 16).ok_or(PushCryptoError::BadAuth)
}

/// Encrypts `plaintext` for one subscription, with a fresh salt and sender key.
pub fn encrypt(p256dh: &str, auth: &str, plaintext: &[u8]) -> Result<Vec<u8>, PushCryptoError> {
    let mut rng = rand::rngs::OsRng;
    let mut salt = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rng, &mut salt);
    let sender = SecretKey::random(&mut rng);
    encrypt_with(p256dh, auth, plaintext, &sender, &salt)
}

fn encrypt_with(
    p256dh: &str,
    auth: &str,
    plaintext: &[u8],
    sender: &SecretKey,
    salt: &[u8; 16],
) -> Result<Vec<u8>, PushCryptoError> {
    let (ua_bytes, ua_key) = ua_public(p256dh)?;
    let auth = auth_secret(auth)?;
    // One record: the plaintext, the last-record delimiter, then the tag.
    if plaintext.len() + 1 + TAG_LEN > RECORD_SIZE as usize {
        return Err(PushCryptoError::TooLarge);
    }
    let shared = p256::ecdh::diffie_hellman(sender.to_nonzero_scalar(), ua_key.as_affine());
    let as_public = sender.public_key().to_encoded_point(false);

    let mut key_info = Vec::with_capacity(14 + 65 + 65);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(&ua_bytes);
    key_info.extend_from_slice(as_public.as_bytes());
    let mut ikm = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(&auth), shared.raw_secret_bytes())
        .expand(&key_info, &mut *ikm)
        .expect("32 bytes is a valid HKDF-SHA256 length");

    let hk = Hkdf::<Sha256>::new(Some(salt), &*ikm);
    let mut cek = Zeroizing::new([0u8; 16]);
    let mut nonce = [0u8; 12];
    hk.expand(b"Content-Encoding: aes128gcm\0", &mut *cek)
        .expect("16 bytes is a valid HKDF-SHA256 length");
    hk.expand(b"Content-Encoding: nonce\0", &mut nonce)
        .expect("12 bytes is a valid HKDF-SHA256 length");

    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    record.push(2);
    let cipher = Aes128Gcm::new_from_slice(&*cek).expect("16-byte key");
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), record.as_slice())
        .map_err(|_| PushCryptoError::TooLarge)?;

    let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    out.push(65);
    out.extend_from_slice(as_public.as_bytes());
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Allod's VAPID key (P-256), derived from the recovery phrase so a restore keeps
/// every browser subscription valid.
pub struct VapidKey {
    secret: SecretKey,
}

impl VapidKey {
    /// HKDF-SHA256 over `ikm` (the secret of `m/1237'/4'/0'`), salt `allod/vapid`,
    /// info `v1` + counter byte; the first valid P-256 scalar wins.
    pub fn derive(ikm: &[u8; 32]) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(b"allod/vapid"), ikm);
        for counter in 0u8..=u8::MAX {
            let mut okm = Zeroizing::new([0u8; 32]);
            hk.expand(&[b'v', b'1', counter], &mut *okm)
                .expect("32 bytes is a valid HKDF-SHA256 length");
            if let Ok(secret) = SecretKey::from_slice(&*okm) {
                return VapidKey { secret };
            }
        }
        // Each try fails with probability about 2^-32.
        panic!("no valid P-256 scalar in 256 HKDF outputs")
    }

    /// The uncompressed public key, base64url without padding: the browser's
    /// `applicationServerKey`.
    pub fn public_key(&self) -> String {
        B64.encode(self.secret.public_key().to_encoded_point(false).as_bytes())
    }

    /// `Authorization` header value for a push to a service at `audience` (the
    /// endpoint's origin): `vapid t=<ES256 JWT>, k=<public key>`.
    pub fn authorization(&self, audience: &str, subject: &str, now: i64) -> String {
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({"aud": audience, "exp": now + 12 * 3600, "sub": subject});
        let claims = B64.encode(claims.to_string());
        let input = format!("{header}.{claims}");
        let signature: Signature = SigningKey::from(&self.secret).sign(input.as_bytes());
        format!("vapid t={input}.{}, k={}", B64.encode(signature.to_bytes()), self.public_key())
    }
}

impl std::fmt::Debug for VapidKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VapidKey({})", self.public_key())
    }
}

/// Receiver side of [`encrypt`]: what a browser or UnifiedPush client does with the
/// body. Used by tests and developer tools. `None` when the body is not a valid
/// single-record message for these keys.
pub fn decrypt(ua_secret: &SecretKey, auth: &[u8], body: &[u8]) -> Option<Vec<u8>> {
    let id_len = *body.get(20)? as usize;
    let salt = body.get(..16)?;
    let as_public = body.get(21..21 + id_len)?;
    let ciphertext = body.get(21 + id_len..)?;
    let as_key = PublicKey::from_sec1_bytes(as_public).ok()?;
    let shared = p256::ecdh::diffie_hellman(ua_secret.to_nonzero_scalar(), as_key.as_affine());
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_secret.public_key().to_encoded_point(false).as_bytes());
    key_info.extend_from_slice(as_public);
    let mut ikm = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(auth), shared.raw_secret_bytes()).expand(&key_info, &mut *ikm).ok()?;
    let hk = Hkdf::<Sha256>::new(Some(salt), &*ikm);
    let (mut cek, mut nonce) = (Zeroizing::new([0u8; 16]), [0u8; 12]);
    hk.expand(b"Content-Encoding: aes128gcm\0", &mut *cek).ok()?;
    hk.expand(b"Content-Encoding: nonce\0", &mut nonce).ok()?;
    let mut plain = Aes128Gcm::new_from_slice(&*cek)
        .ok()?
        .decrypt(Nonce::from_slice(&nonce), ciphertext)
        .ok()?;
    while plain.last() == Some(&0) {
        plain.pop();
    }
    (plain.pop() == Some(2)).then_some(plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    fn b64(s: &str) -> Vec<u8> {
        b64_decode(&s.replace([' ', '\n'], "")).unwrap()
    }

    /// RFC 8291 Appendix A.
    #[test]
    fn rfc8291_test_vector() {
        let plaintext = b64("V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24");
        assert_eq!(plaintext, b"When I grow up, I want to be a watermelon");
        let as_private =
            SecretKey::from_slice(&b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        assert_eq!(
            B64.encode(as_private.public_key().to_encoded_point(false).as_bytes()),
            "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"
        );
        let ua_public =
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        let auth = "BTBZMqHH6r4Tts7J_aSIgg";
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();

        let out = encrypt_with(ua_public, auth, &plaintext, &as_private, &salt).unwrap();
        let expected = [
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z 9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml",
            "mlMoZIIgDll6e3vCYLocInmYWAmS6Tlz AC8wEqKK6PBru3jl7A8",
        ]
        .concat();
        let ciphertext =
            ["8pfeW0KbunFT06SuDKoJH9Ql87S1QUrd irN6GcG7sFz1y1sqLgVi1VhjVkHsUoEs", "bI_0LpXMuGvnzQ"]
                .concat();
        assert_eq!(out[..HEADER_LEN], b64(&expected)[..]);
        assert_eq!(out[HEADER_LEN..], b64(&ciphertext)[..]);
    }

    #[test]
    fn rejects_bad_keys_and_large_messages() {
        let ua = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        let auth = "BTBZMqHH6r4Tts7J_aSIgg";
        assert_eq!(check_keys(ua, auth), Ok(()));
        assert_eq!(check_keys("AAAA", auth), Err(PushCryptoError::BadPublicKey));
        assert_eq!(check_keys(ua, "AAAA"), Err(PushCryptoError::BadAuth));
        assert_eq!(encrypt(ua, auth, &[0; 4096]), Err(PushCryptoError::TooLarge));
        // Padded base64 is accepted too.
        assert_eq!(check_keys(ua, "BTBZMqHH6r4Tts7J_aSIgg=="), Ok(()));
        let a = encrypt(ua, auth, b"hi").unwrap();
        let b = encrypt(ua, auth, b"hi").unwrap();
        assert_ne!(a, b, "fresh salt and sender key per message");
        assert_eq!(a.len(), HEADER_LEN + 2 + 1 + TAG_LEN);
    }

    #[test]
    fn vapid_key_is_deterministic_and_signs_es256() {
        let k1 = VapidKey::derive(&[7; 32]);
        let k2 = VapidKey::derive(&[7; 32]);
        assert_eq!(k1.public_key(), k2.public_key());
        assert_ne!(k1.public_key(), VapidKey::derive(&[8; 32]).public_key());
        assert_eq!(b64_decode(&k1.public_key()).unwrap().len(), 65);

        let header = k1.authorization("https://push.example.net", "https://allod.example", 1000);
        let rest = header.strip_prefix("vapid t=").unwrap();
        let (jwt, key) = rest.split_once(", k=").unwrap();
        assert_eq!(key, k1.public_key());
        let mut parts = jwt.split('.');
        let (h, c, s) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        let claims: serde_json::Value = serde_json::from_slice(&b64_decode(c).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://push.example.net");
        assert_eq!(claims["exp"], 1000 + 12 * 3600);
        assert_eq!(claims["sub"], "https://allod.example");
        let verifying =
            VerifyingKey::from_sec1_bytes(&b64_decode(&k1.public_key()).unwrap()).unwrap();
        let sig = Signature::from_slice(&b64_decode(s).unwrap()).unwrap();
        verifying.verify(format!("{h}.{c}").as_bytes(), &sig).unwrap();
    }
}
