//! Sealing for backups and exports (SPEC §4.3): XChaCha20-Poly1305, stored as
//! `v1:` + base64(nonce ‖ ciphertext).
//!
//! The key normally comes from the recovery phrase ([`Vault::from_backup_key`]).
//! [`Vault::derive`] (Argon2id from a user secret) is kept for user-chosen export
//! passwords.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, Zeroizing};

use crate::KeyError;

/// Prefix of every encrypted value.
pub const ENCRYPTED_PREFIX: &str = "v1:";
const NONCE_LEN: usize = 24;
const CHECK_PLAINTEXT: &[u8] = b"allod-vault-check-v1";

/// Argon2id cost parameters. Stored next to the salt so they can be raised later
/// without breaking existing installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultParams {
    /// Memory in KiB.
    pub memory_kib: u32,
    /// Iterations.
    pub iterations: u32,
    /// Parallelism.
    pub parallelism: u32,
}

impl VaultParams {
    /// SPEC §4.3 defaults: 64 MiB, 3 iterations, 1 lane.
    pub const DEFAULT: VaultParams =
        VaultParams { memory_kib: 64 * 1024, iterations: 3, parallelism: 1 };

    /// Serialized form for the `config` table, e.g. `argon2id:m=65536,t=3,p=1`.
    pub fn encode(&self) -> String {
        format!("argon2id:m={},t={},p={}", self.memory_kib, self.iterations, self.parallelism)
    }

    /// Parses [`VaultParams::encode`] output.
    pub fn decode(s: &str) -> Result<Self, KeyError> {
        let rest = s.strip_prefix("argon2id:").ok_or(KeyError::Format)?;
        let mut params = VaultParams { memory_kib: 0, iterations: 0, parallelism: 0 };
        for part in rest.split(',') {
            let (k, v) = part.split_once('=').ok_or(KeyError::Format)?;
            let v: u32 = v.parse().map_err(|_| KeyError::Format)?;
            match k {
                "m" => params.memory_kib = v,
                "t" => params.iterations = v,
                "p" => params.parallelism = v,
                _ => return Err(KeyError::Format),
            }
        }
        if params.memory_kib == 0 || params.iterations == 0 || params.parallelism == 0 {
            return Err(KeyError::Format);
        }
        Ok(params)
    }
}

/// An unlocked vault: holds the derived encryption key in memory (zeroed on drop).
pub struct Vault {
    cipher: XChaCha20Poly1305,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vault(unlocked)")
    }
}

impl Vault {
    /// Uses a 32-byte key directly.
    pub fn from_key(key: &[u8; 32]) -> Self {
        Vault { cipher: XChaCha20Poly1305::new(key.into()) }
    }

    /// Backup vault: the key is SHA-256 of the backup key pair's secret
    /// (`m/1237'/3'/0'`, SPEC §4.2), domain-separated.
    pub fn from_backup_key(backup: &crate::KeyPair) -> Self {
        use sha2::{Digest, Sha256};
        let secret = backup.expose_secret_bytes();
        let mut hasher = Sha256::new();
        hasher.update(b"allod-backup-v1");
        hasher.update(&secret[..]);
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&hasher.finalize());
        Self::from_key(&key)
    }

    /// Derives the key from a user-chosen secret with Argon2id. `salt` must be 16 random bytes stored with
    /// the vault (see [`Vault::new_salt`]).
    pub fn derive(secret: &str, salt: &[u8], params: VaultParams) -> Result<Self, KeyError> {
        let argon = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            Params::new(params.memory_kib, params.iterations, params.parallelism, Some(32))
                .map_err(|e| KeyError::Kdf(e.to_string()))?,
        );
        let mut key = Zeroizing::new([0u8; 32]);
        argon
            .hash_password_into(secret.as_bytes(), salt, &mut key[..])
            .map_err(|e| KeyError::Kdf(e.to_string()))?;
        let cipher = XChaCha20Poly1305::new_from_slice(&key[..]).map_err(|_| KeyError::Format)?;
        Ok(Vault { cipher })
    }

    /// 16 random bytes for a new vault.
    pub fn new_salt() -> [u8; 16] {
        use chacha20poly1305::aead::rand_core::RngCore;
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        salt
    }

    /// Encrypts a value; output is `v1:` + base64(nonce ‖ ciphertext).
    pub fn seal(&self, plaintext: &[u8]) -> Result<String, KeyError> {
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut ct = self.cipher.encrypt(&nonce, plaintext).map_err(|_| KeyError::Format)?;
        let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);
        ct.zeroize();
        Ok(format!("{ENCRYPTED_PREFIX}{}", B64.encode(blob)))
    }

    /// Decrypts a value produced by [`Vault::seal`].
    pub fn open(&self, sealed: &str) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        let encoded = sealed.strip_prefix(ENCRYPTED_PREFIX).ok_or(KeyError::Format)?;
        let blob = B64.decode(encoded).map_err(|_| KeyError::Format)?;
        if blob.len() < NONCE_LEN + 16 {
            return Err(KeyError::Format);
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let plaintext =
            self.cipher.decrypt(XNonce::from_slice(nonce), ct).map_err(|_| KeyError::Decrypt)?;
        Ok(Zeroizing::new(plaintext))
    }

    /// Decrypts a value that must be UTF-8 text.
    pub fn open_string(&self, sealed: &str) -> Result<Zeroizing<String>, KeyError> {
        let bytes = self.open(sealed)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| KeyError::Format)?;
        Ok(Zeroizing::new(text.to_string()))
    }

    /// A sealed check value, used to verify a user secret later.
    pub fn check_value(&self) -> Result<String, KeyError> {
        self.seal(CHECK_PLAINTEXT)
    }

    /// True when `sealed_check` was produced by a vault with the same key.
    pub fn verify(&self, sealed_check: &str) -> bool {
        matches!(self.open(sealed_check), Ok(p) if p.as_slice() == CHECK_PLAINTEXT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cheap parameters keep the tests fast; the KDF itself is Argon2's job.
    const FAST: VaultParams = VaultParams { memory_kib: 8, iterations: 1, parallelism: 1 };

    #[test]
    fn round_trip_and_random_nonces() {
        let salt = Vault::new_salt();
        let v = Vault::derive("correct horse battery staple", &salt, FAST).unwrap();
        let a = v.seal(b"secret").unwrap();
        let b = v.seal(b"secret").unwrap();
        assert!(a.starts_with(ENCRYPTED_PREFIX));
        assert_ne!(a, b, "nonces must differ");
        assert_eq!(v.open(&a).unwrap().as_slice(), b"secret");
        assert_eq!(v.open_string(&b).unwrap().as_str(), "secret");
    }

    #[test]
    fn wrong_unlock_code_and_tampering_fail() {
        let salt = Vault::new_salt();
        let v = Vault::derive("one unlock code", &salt, FAST).unwrap();
        let sealed = v.seal(b"secret").unwrap();
        let other = Vault::derive("another unlock code", &salt, FAST).unwrap();
        assert!(matches!(other.open(&sealed), Err(KeyError::Decrypt)));

        let mut blob = B64.decode(sealed.strip_prefix(ENCRYPTED_PREFIX).unwrap()).unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 1;
        let tampered = format!("{ENCRYPTED_PREFIX}{}", B64.encode(blob));
        assert!(matches!(v.open(&tampered), Err(KeyError::Decrypt)));
        assert!(matches!(v.open("v0:abc"), Err(KeyError::Format)));
        assert!(matches!(v.open("v1:AAAA"), Err(KeyError::Format)));
    }

    #[test]
    fn same_unlock_code_and_salt_give_same_key() {
        let salt = Vault::new_salt();
        let sealed = Vault::derive("pw", &salt, FAST).unwrap().seal(b"x").unwrap();
        assert_eq!(
            Vault::derive("pw", &salt, FAST).unwrap().open(&sealed).unwrap().as_slice(),
            b"x"
        );
        let other_salt = Vault::new_salt();
        assert!(Vault::derive("pw", &other_salt, FAST).unwrap().open(&sealed).is_err());
    }

    #[test]
    fn check_value_verifies_unlock_code() {
        let salt = Vault::new_salt();
        let v = Vault::derive("pw", &salt, FAST).unwrap();
        let check = v.check_value().unwrap();
        assert!(v.verify(&check));
        assert!(!Vault::derive("pw2", &salt, FAST).unwrap().verify(&check));
        assert!(!v.verify("garbage"));
    }

    #[test]
    fn params_encode_decode() {
        assert_eq!(VaultParams::DEFAULT.encode(), "argon2id:m=65536,t=3,p=1");
        assert_eq!(VaultParams::decode("argon2id:m=65536,t=3,p=1").unwrap(), VaultParams::DEFAULT);
        assert!(VaultParams::decode("scrypt:n=1").is_err());
        assert!(VaultParams::decode("argon2id:m=0,t=3,p=1").is_err());
        assert!(VaultParams::decode("argon2id:m=1,t=1,p=1,x=2").is_err());
    }

    #[test]
    fn backup_vault_is_deterministic_per_phrase() {
        let phrase = crate::RecoveryPhrase::generate().unwrap();
        let keys = crate::NostrKeys::from_phrase(&phrase).unwrap();
        let a = Vault::from_backup_key(&keys.backup_key().unwrap());
        let sealed = a.seal(b"db snapshot").unwrap();
        let b = Vault::from_backup_key(&keys.backup_key().unwrap());
        assert_eq!(b.open(&sealed).unwrap().as_slice(), b"db snapshot");
        let other =
            crate::NostrKeys::from_phrase(&crate::RecoveryPhrase::generate().unwrap()).unwrap();
        assert!(Vault::from_backup_key(&other.backup_key().unwrap()).open(&sealed).is_err());
    }

    #[test]
    fn default_params_work() {
        // One real derivation with SPEC parameters (64 MiB, t=3).
        let salt = Vault::new_salt();
        let v = Vault::derive("pw", &salt, VaultParams::DEFAULT).unwrap();
        assert!(v.verify(&v.check_value().unwrap()));
    }
}
