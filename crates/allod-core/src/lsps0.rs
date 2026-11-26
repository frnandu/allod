//! A minimal Lightning peer that only speaks LSPS0 (bLIP-50): enough to ask an LSP for its
//! LSPS2 fee menu (`lsps2.get_info`) without a Lightning node.
//!
//! It connects with a throwaway node key, runs the BOLT 8 Noise_XK handshake, exchanges
//! `init`, answers pings and sends JSON-RPC requests as custom message 37913. Nothing is
//! opened or bought: `get_info` only returns offers.

use std::time::Duration;

use bitcoin::secp256k1::{ecdh::SharedSecret, PublicKey, Secp256k1, SecretKey};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// LSPS0 messages travel as this custom Lightning message type.
pub const LSPS_MESSAGE_TYPE: u16 = 37913;
const INIT: u16 = 16;
const ERROR: u16 = 17;
const PING: u16 = 18;
const PONG: u16 = 19;
/// Mainnet's genesis block hash, in the byte order `init`'s `networks` uses.
const MAINNET_CHAIN: &str = "6fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000";
const PROTOCOL: &[u8] = b"Noise_XK_secp256k1_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"lightning";
const MAC: usize = 16;
/// Lightning messages are at most 65535 bytes.
const MAX_MESSAGE: usize = 65_535;

#[derive(Debug, thiserror::Error)]
pub enum Lsps0Error {
    #[error("cannot connect: {0}")]
    Connect(String),
    #[error("timed out")]
    Timeout,
    #[error("handshake failed: {0}")]
    Handshake(&'static str),
    #[error("connection: {0}")]
    Io(#[from] std::io::Error),
    #[error("the peer sent an error: {0}")]
    Peer(String),
    #[error("the LSP answered with an error: {0}")]
    Rpc(String),
    #[error("unexpected reply: {0}")]
    Protocol(String),
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// BOLT 8's HKDF: salt `ck`, no info, 64 bytes split in two keys.
fn hkdf2(ck: &[u8; 32], ikm: &[u8]) -> ([u8; 32], [u8; 32]) {
    let mut out = [0u8; 64];
    Hkdf::<Sha256>::new(Some(ck), ikm).expand(&[], &mut out).expect("64 bytes is a valid length");
    let (a, b) = out.split_at(32);
    (a.try_into().expect("32"), b.try_into().expect("32"))
}

fn ecdh(sk: &SecretKey, pk: &PublicKey) -> [u8; 32] {
    // SHA256 of the compressed shared point, as BOLT 8 specifies.
    SharedSecret::new(pk, sk).secret_bytes()
}

fn nonce(n: u64) -> Nonce {
    let mut b = [0u8; 12];
    b[4..].copy_from_slice(&n.to_le_bytes());
    Nonce::from(b)
}

fn encrypt(k: &[u8; 32], n: u64, ad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(Key::from_slice(k))
        .encrypt(&nonce(n), Payload { msg: plaintext, aad: ad })
        .expect("encryption does not fail")
}

fn decrypt(k: &[u8; 32], n: u64, ad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, Lsps0Error> {
    ChaCha20Poly1305::new(Key::from_slice(k))
        .decrypt(&nonce(n), Payload { msg: ciphertext, aad: ad })
        .map_err(|_| Lsps0Error::Handshake("bad MAC"))
}

/// One direction of the transport: a key, its nonce and chaining key (rotated every 1000
/// nonces, BOLT 8).
struct CipherState {
    ck: [u8; 32],
    k: [u8; 32],
    n: u64,
}

impl CipherState {
    fn advance(&mut self) {
        self.n += 1;
        if self.n == 1000 {
            let (ck, k) = hkdf2(&self.ck, &self.k);
            self.ck = ck;
            self.k = k;
            self.n = 0;
        }
    }

    fn seal(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let c = encrypt(&self.k, self.n, &[], plaintext);
        self.advance();
        c
    }

    fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, Lsps0Error> {
        let p = decrypt(&self.k, self.n, &[], ciphertext)?;
        self.advance();
        Ok(p)
    }
}

/// The initiator's side of the handshake, kept apart from the socket for the spec's test
/// vectors.
struct Handshake {
    h: [u8; 32],
    ck: [u8; 32],
    e: SecretKey,
    s: SecretKey,
    temp_k2: [u8; 32],
    re: Option<PublicKey>,
}

impl Handshake {
    fn new(remote: &PublicKey, s: SecretKey, e: SecretKey) -> Self {
        let h0 = sha256(&[PROTOCOL]);
        let h = sha256(&[&h0, PROLOGUE]);
        let h = sha256(&[&h, &remote.serialize()]);
        Handshake { h, ck: h0, e, s, temp_k2: [0; 32], re: None }
    }

    fn act_one(&mut self, remote: &PublicKey) -> [u8; 50] {
        let secp = Secp256k1::signing_only();
        let epub = PublicKey::from_secret_key(&secp, &self.e).serialize();
        self.h = sha256(&[&self.h, &epub]);
        let (ck, temp_k1) = hkdf2(&self.ck, &ecdh(&self.e, remote));
        self.ck = ck;
        let c = encrypt(&temp_k1, 0, &self.h, &[]);
        self.h = sha256(&[&self.h, &c]);
        let mut out = [0u8; 50];
        out[1..34].copy_from_slice(&epub);
        out[34..].copy_from_slice(&c);
        out
    }

    fn read_act_two(&mut self, m: &[u8; 50]) -> Result<(), Lsps0Error> {
        if m[0] != 0 {
            return Err(Lsps0Error::Handshake("unknown handshake version"));
        }
        let re = PublicKey::from_slice(&m[1..34]).map_err(|_| Lsps0Error::Handshake("bad key"))?;
        self.h = sha256(&[&self.h, &re.serialize()]);
        let (ck, temp_k2) = hkdf2(&self.ck, &ecdh(&self.e, &re));
        self.ck = ck;
        decrypt(&temp_k2, 0, &self.h, &m[34..])?;
        self.h = sha256(&[&self.h, &m[34..]]);
        self.temp_k2 = temp_k2;
        self.re = Some(re);
        Ok(())
    }

    /// Act three, and the transport keys: (sending, receiving).
    fn act_three(self) -> Result<([u8; 66], CipherState, CipherState), Lsps0Error> {
        let re = self.re.ok_or(Lsps0Error::Handshake("act two missing"))?;
        let secp = Secp256k1::signing_only();
        let spub = PublicKey::from_secret_key(&secp, &self.s).serialize();
        let c = encrypt(&self.temp_k2, 1, &self.h, &spub);
        let h = sha256(&[&self.h, &c]);
        let (ck, temp_k3) = hkdf2(&self.ck, &ecdh(&self.s, &re));
        let t = encrypt(&temp_k3, 0, &h, &[]);
        let (sk, rk) = hkdf2(&ck, &[]);
        let mut out = [0u8; 66];
        out[1..50].copy_from_slice(&c);
        out[50..].copy_from_slice(&t);
        Ok((out, CipherState { ck, k: sk, n: 0 }, CipherState { ck, k: rk, n: 0 }))
    }
}

/// Feature bits, in the big-endian byte order Lightning uses.
fn feature_bytes(bits: &[usize]) -> Vec<u8> {
    let len = bits.iter().max().map_or(0, |b| b / 8 + 1);
    let mut f = vec![0u8; len];
    for b in bits {
        f[len - 1 - b / 8] |= 1 << (b % 8);
    }
    f
}

/// Our `init`: only optional (odd) features, the common ones a client offers, and the
/// mainnet chain.
fn init_message() -> Vec<u8> {
    let features = feature_bytes(&[1, 9, 13, 15, 17, 19, 23, 27, 45, 47, 51]);
    let chain = hex::decode(MAINNET_CHAIN).expect("valid hex");
    let mut m = Vec::new();
    m.extend_from_slice(&INIT.to_be_bytes());
    m.extend_from_slice(&0u16.to_be_bytes());
    m.extend_from_slice(&(features.len() as u16).to_be_bytes());
    m.extend_from_slice(&features);
    m.push(1); // networks
    m.push(chain.len() as u8);
    m.extend_from_slice(&chain);
    m
}

/// An open, handshaken connection to a Lightning peer.
pub struct Peer {
    stream: TcpStream,
    send: CipherState,
    recv: CipherState,
    next_id: u64,
}

impl Peer {
    /// Connects to `pubkey@host:port` and exchanges `init`, within `timeout`.
    pub async fn connect(lsp: &str, timeout: Duration) -> Result<Peer, Lsps0Error> {
        tokio::time::timeout(timeout, Self::connect_inner(lsp))
            .await
            .map_err(|_| Lsps0Error::Timeout)?
    }

    async fn connect_inner(lsp: &str) -> Result<Peer, Lsps0Error> {
        let (pubkey, address) = lsp
            .split_once('@')
            .ok_or_else(|| Lsps0Error::Connect("not pubkey@host:port".into()))?;
        let remote = PublicKey::from_slice(
            &hex::decode(pubkey).map_err(|_| Lsps0Error::Connect("bad pubkey".into()))?,
        )
        .map_err(|_| Lsps0Error::Connect("bad pubkey".into()))?;
        let address = address.trim_start_matches('[').replacen("]:", ":", 1);
        let mut stream =
            TcpStream::connect(address).await.map_err(|e| Lsps0Error::Connect(e.to_string()))?;
        let mut hs = Handshake::new(&remote, random_key(), random_key());
        stream.write_all(&hs.act_one(&remote)).await?;
        let mut act_two = [0u8; 50];
        stream.read_exact(&mut act_two).await?;
        hs.read_act_two(&act_two)?;
        let (act_three, send, recv) = hs.act_three()?;
        stream.write_all(&act_three).await?;
        let mut peer = Peer { stream, send, recv, next_id: 0 };
        peer.write(&init_message()).await?;
        loop {
            let m = peer.read().await?;
            match msg_type(&m) {
                INIT => return Ok(peer),
                ERROR => return Err(Lsps0Error::Peer(error_text(&m))),
                _ => peer.answer_ping(&m).await?,
            }
        }
    }

    async fn write(&mut self, m: &[u8]) -> Result<(), Lsps0Error> {
        let len = self.send.seal(&(m.len() as u16).to_be_bytes());
        let body = self.send.seal(m);
        self.stream.write_all(&[len, body].concat()).await?;
        Ok(())
    }

    async fn read(&mut self) -> Result<Vec<u8>, Lsps0Error> {
        let mut lc = [0u8; 2 + MAC];
        self.stream.read_exact(&mut lc).await?;
        let len = self.recv.open(&lc)?;
        let len = u16::from_be_bytes([len[0], len[1]]) as usize;
        if len > MAX_MESSAGE {
            return Err(Lsps0Error::Protocol("message too long".into()));
        }
        let mut c = vec![0u8; len + MAC];
        self.stream.read_exact(&mut c).await?;
        self.recv.open(&c)
    }

    /// Answers a ping; other messages (gossip, warnings) are ignored.
    async fn answer_ping(&mut self, m: &[u8]) -> Result<(), Lsps0Error> {
        if msg_type(m) == PING && m.len() >= 4 {
            let n = u16::from_be_bytes([m[2], m[3]]);
            if n < 65532 {
                let mut pong = PONG.to_be_bytes().to_vec();
                pong.extend_from_slice(&n.to_be_bytes());
                pong.resize(4 + n as usize, 0);
                self.write(&pong).await?;
            }
        }
        Ok(())
    }

    /// One LSPS0 JSON-RPC call; its `result`, or the LSP's error.
    pub async fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, Lsps0Error> {
        self.next_id += 1;
        let id = format!("allod{}", self.next_id);
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params, "id": id});
        let mut m = LSPS_MESSAGE_TYPE.to_be_bytes().to_vec();
        m.extend_from_slice(body.to_string().as_bytes());
        self.write(&m).await?;
        tokio::time::timeout(timeout, async {
            loop {
                let m = self.read().await?;
                match msg_type(&m) {
                    LSPS_MESSAGE_TYPE => {
                        let v: Value = serde_json::from_slice(&m[2..])
                            .map_err(|e| Lsps0Error::Protocol(e.to_string()))?;
                        if v["id"] != id {
                            continue;
                        }
                        if let Some(e) = v.get("error") {
                            let text =
                                e["message"].as_str().map_or_else(|| e.to_string(), str::to_string);
                            return Err(Lsps0Error::Rpc(text));
                        }
                        return Ok(v["result"].clone());
                    }
                    ERROR => return Err(Lsps0Error::Peer(error_text(&m))),
                    _ => self.answer_ping(&m).await?,
                }
            }
        })
        .await
        .map_err(|_| Lsps0Error::Timeout)?
    }
}

fn msg_type(m: &[u8]) -> u16 {
    if m.len() < 2 {
        return 0;
    }
    u16::from_be_bytes([m[0], m[1]])
}

/// The text of an `error` message (channel id, length, data).
fn error_text(m: &[u8]) -> String {
    m.get(36..).map(|d| String::from_utf8_lossy(d).into_owned()).unwrap_or_default()
}

fn random_key() -> SecretKey {
    loop {
        let mut b = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
        if let Ok(k) = SecretKey::from_slice(&b) {
            return k;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> SecretKey {
        SecretKey::from_slice(&[byte; 32]).unwrap()
    }

    /// BOLT 8, Appendix A: the initiator's test vectors.
    #[test]
    fn handshake_matches_the_spec() {
        let remote = PublicKey::from_slice(
            &hex::decode("028d7500dd4c12685d1f568b4c2b5048e8534b873319f3a8daa612b469132ec7f7")
                .unwrap(),
        )
        .unwrap();
        let mut hs = Handshake::new(&remote, key(0x11), key(0x12));
        assert_eq!(
            hex::encode(hs.act_one(&remote)),
            "00036360e856310ce5d294e8be33fc807077dc56ac80d95d9cd4ddbd21325eff73f70df6086551151f58b8afe6c195782c6a"
        );
        let act_two: [u8; 50] = hex::decode(
            "0002466d7fcae563e5cb09a0d1870bb580344804617879a14949cf22285f1bae3f276e2470b93aac583c9ef6eafca3f730ae",
        )
        .unwrap()
        .try_into()
        .unwrap();
        hs.read_act_two(&act_two).unwrap();
        let (act_three, mut send, recv) = hs.act_three().unwrap();
        assert_eq!(
            hex::encode(act_three),
            "00b9e3a702e93e3a9948c2ed6e5fd7590a6e1c3a0344cfc9d5b57357049aa22355361aa02e55a8fc28fef5bd6d71ad0c38228dc68b1c466263b47fdf31e560e139ba"
        );
        assert_eq!(
            hex::encode(send.k),
            "969ab31b4d288cedf6218839b27a3e2140827047f2c0f01bf5c04435d43511a9"
        );
        assert_eq!(
            hex::encode(recv.k),
            "bb9020b8965f4df047e07f955f3c4b88418984aadc5cdb35096b9ea8fa5c3442"
        );

        // Message encryption, with the key rotation every 1000 nonces.
        let mut outputs = std::collections::HashMap::new();
        for i in 0..1002 {
            let len = send.seal(&5u16.to_be_bytes());
            let body = send.seal(b"hello");
            if [0, 1, 500, 501, 1000, 1001].contains(&i) {
                outputs.insert(i, hex::encode([len, body].concat()));
            }
        }
        assert_eq!(
            outputs[&0],
            "cf2b30ddf0cf3f80e7c35a6e6730b59fe802473180f396d88a8fb0db8cbcf25d2f214cf9ea1d95"
        );
        assert_eq!(
            outputs[&1],
            "72887022101f0b6753e0c7de21657d35a4cb2a1f5cde2650528bbc8f837d0f0d7ad833b1a256a1"
        );
        assert_eq!(
            outputs[&500],
            "178cb9d7387190fa34db9c2d50027d21793c9bc2d40b1e14dcf30ebeeeb220f48364f7a4c68bf8"
        );
        assert_eq!(
            outputs[&501],
            "1b186c57d44eb6de4c057c49940d79bb838a145cb528d6e8fd26dbe50a60ca2c104b56b60e45bd"
        );
        assert_eq!(
            outputs[&1000],
            "4a2f3cc3b5e78ddb83dcb426d9863d9d9a723b0337c89dd0b005d89f8d3c05c52b76b29b740f09"
        );
        assert_eq!(
            outputs[&1001],
            "2ecd8c8a5629d0d02ab457a0fdd0f7b90a192cd46be5ecb6ca570bfc5e268338b1a16cf4ef2d36"
        );
    }

    #[test]
    fn features_are_big_endian() {
        assert_eq!(feature_bytes(&[0]), vec![0x01]);
        assert_eq!(feature_bytes(&[9, 1]), vec![0x02, 0x02]);
        assert_eq!(feature_bytes(&[]), Vec::<u8>::new());
    }

    /// Against a live LSP: `cargo test -p allod-core lsps0 -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn asks_a_live_lsp() {
        let lsp = std::env::var("LSPS_PROBE").unwrap_or_else(|_| {
            "0388948c5c7775a5eda3ee4a96434a270f20f5beeed7e9c99f242f21b87d658850@stablechannels.com:9735".into()
        });
        let mut p = Peer::connect(&lsp, Duration::from_secs(15)).await.unwrap();
        let protocols =
            p.request("lsps0.list_protocols", json!({}), Duration::from_secs(15)).await.unwrap();
        println!("{protocols}");
        let info = p.request("lsps2.get_info", json!({}), Duration::from_secs(15)).await.unwrap();
        println!("{info}");
        assert!(info["opening_fee_params_menu"].is_array());
    }
}
