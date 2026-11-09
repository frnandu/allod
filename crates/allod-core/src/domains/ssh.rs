//! SSH to gateways Allod sets up itself (SPEC §12.8): an ed25519 key kept in the secret
//! `gateway_ssh`, and one command run with a script on its standard input.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::ChannelMsg;

/// A new ed25519 key in OpenSSH format.
pub fn new_key() -> Result<String, String> {
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
    let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
    key.to_openssh(LineEnding::LF).map(|k| k.to_string()).map_err(|e| e.to_string())
}

/// `ssh-ed25519 AAAA… allod` for a private key.
pub fn public_openssh(private: &str) -> Result<String, String> {
    let key = PrivateKey::from_openssh(private).map_err(|e| e.to_string())?;
    let mut public = key.public_key().clone();
    public.set_comment("allod");
    public.to_openssh().map_err(|e| e.to_string())
}

/// Checks the server key against the pinned fingerprint (or records it the first time).
struct Pin {
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Pin {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else { return Ok(false) };
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        *self.seen.lock().unwrap() = Some(fp.clone());
        Ok(self.expected.as_ref().is_none_or(|e| *e == fp))
    }
}

/// What a remote command did.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub exit: Option<u32>,
    /// Combined stdout and stderr, the last 4 KB.
    pub output: String,
    /// The server key's SHA-256 fingerprint (to pin next time).
    pub host_key: Option<String>,
}

/// Runs `command` on `host:22` as `user`, with `stdin` as its input.
pub async fn run(
    host: &str,
    user: &str,
    private_key: &str,
    host_key: Option<&str>,
    command: &str,
    stdin: &[u8],
) -> Result<Outcome, String> {
    let key = PrivateKey::from_openssh(private_key).map_err(|e| e.to_string())?;
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(600)),
        ..Default::default()
    });
    let seen = Arc::new(Mutex::new(None));
    let pin = Pin { expected: host_key.map(str::to_string), seen: seen.clone() };
    let connect = client::connect(config, (host, 22), pin);
    let mut session = tokio::time::timeout(Duration::from_secs(20), connect)
        .await
        .map_err(|_| "SSH: timed out connecting".to_string())?
        .map_err(|e| match &*seen.lock().unwrap() {
            Some(fp) if host_key.is_some_and(|h| h != fp) => {
                format!("SSH: the server key changed (now {fp}); not connecting")
            }
            _ => format!("SSH: {e}"),
        })?;
    let auth = session
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .map_err(|e| format!("SSH: {e}"))?;
    if !auth.success() {
        return Err(format!("SSH: the server refused the key for {user}"));
    }
    let mut channel = session.channel_open_session().await.map_err(|e| format!("SSH: {e}"))?;
    channel.exec(true, command).await.map_err(|e| format!("SSH: {e}"))?;
    channel.data(stdin).await.map_err(|e| format!("SSH: {e}"))?;
    channel.eof().await.map_err(|e| format!("SSH: {e}"))?;
    let mut exit = None;
    let mut output = Vec::new();
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { ref data } => output.extend_from_slice(data),
            ChannelMsg::ExtendedData { ref data, .. } => output.extend_from_slice(data),
            ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
            _ => {}
        }
    }
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    let start = output.len().saturating_sub(4096);
    let host_key = seen.lock().unwrap().clone();
    Ok(Outcome { exit, output: String::from_utf8_lossy(&output[start..]).into_owned(), host_key })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let k = new_key().unwrap();
        assert!(k.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
        let p = public_openssh(&k).unwrap();
        assert!(p.starts_with("ssh-ed25519 AAAA") && p.ends_with(" allod"), "{p}");
    }
}
