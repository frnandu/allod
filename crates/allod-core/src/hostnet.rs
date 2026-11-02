//! Network settings of the appliance host (SPEC §2.3.10): Wi-Fi scan, connect, forget and
//! the Wi-Fi country. Allod has no access to the host's network stack; it asks the host's
//! `allod-net-api` helper over a Unix socket (`ALLOD_HOST_NETWORK_SOCKET`). One request per
//! connection: a JSON line, answered by one JSON document with `ok` and either the result
//! fields or `error`.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Replies are small (a scan lists a few dozen networks); anything larger is refused.
const MAX_REPLY: u64 = 256 * 1024;
/// `nmcli` waits up to 45 s for a connection; leave room for the helper around it.
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum HostNetError {
    /// The request itself is invalid (checked before it reaches the host).
    #[error("{0}")]
    BadRequest(String),
    /// The host tried and failed (wrong password, network out of range, ...).
    #[error("{0}")]
    Failed(String),
    /// The helper is not running or did not answer.
    #[error("the host network helper is unavailable: {0}")]
    Unavailable(String),
}

pub struct HostNetwork {
    socket: PathBuf,
}

impl HostNetwork {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        HostNetwork { socket: socket.into() }
    }

    /// Interfaces, addresses, the current Wi-Fi network and the Wi-Fi country.
    pub async fn status(&self) -> Result<Value, HostNetError> {
        self.call(json!({ "cmd": "status" })).await
    }

    /// Wi-Fi networks in range, strongest first.
    pub async fn scan(&self) -> Result<Value, HostNetError> {
        self.call(json!({ "cmd": "scan" })).await
    }

    /// Joins a Wi-Fi network and keeps it for later boots. An empty password means an
    /// open network.
    pub async fn connect(
        &self,
        ssid: &str,
        password: &str,
        hidden: bool,
    ) -> Result<Value, HostNetError> {
        check_ssid(ssid)?;
        check_password(password)?;
        self.call(json!({ "cmd": "connect", "ssid": ssid, "password": password, "hidden": hidden }))
            .await
    }

    /// Removes a saved Wi-Fi network.
    pub async fn forget(&self, ssid: &str) -> Result<Value, HostNetError> {
        check_ssid(ssid)?;
        self.call(json!({ "cmd": "forget", "ssid": ssid })).await
    }

    /// Sets the Wi-Fi regulatory country (ISO 3166 alpha-2), which decides the allowed
    /// channels.
    pub async fn set_country(&self, country: &str) -> Result<Value, HostNetError> {
        let country = country.trim().to_ascii_uppercase();
        if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(HostNetError::BadRequest(
                "enter a two-letter country code, e.g. DE or US".into(),
            ));
        }
        self.call(json!({ "cmd": "country", "country": country })).await
    }

    async fn call(&self, request: Value) -> Result<Value, HostNetError> {
        let unavailable = |e: std::io::Error| HostNetError::Unavailable(e.to_string());
        let exchange = async {
            let mut stream = UnixStream::connect(&self.socket).await.map_err(unavailable)?;
            let mut line = request.to_string();
            line.push('\n');
            stream.write_all(line.as_bytes()).await.map_err(unavailable)?;
            stream.shutdown().await.map_err(unavailable)?;
            let mut reply = Vec::new();
            stream.take(MAX_REPLY + 1).read_to_end(&mut reply).await.map_err(unavailable)?;
            Ok::<_, HostNetError>(reply)
        };
        let reply = tokio::time::timeout(TIMEOUT, exchange)
            .await
            .map_err(|_| HostNetError::Unavailable("no answer within 60 s".into()))??;
        if reply.len() as u64 > MAX_REPLY {
            return Err(HostNetError::Unavailable("reply too large".into()));
        }
        parse_reply(&reply)
    }
}

fn parse_reply(reply: &[u8]) -> Result<Value, HostNetError> {
    let value: Value = serde_json::from_slice(reply)
        .map_err(|_| HostNetError::Unavailable("the helper sent no valid JSON".into()))?;
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(value),
        Some(false) => Err(HostNetError::Failed(
            value.get("error").and_then(Value::as_str).unwrap_or("failed").to_string(),
        )),
        None => Err(HostNetError::Unavailable("the helper's reply has no \"ok\" field".into())),
    }
}

/// An SSID is 1 to 32 bytes; control characters are refused so it stays printable.
fn check_ssid(ssid: &str) -> Result<(), HostNetError> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
        return Err(HostNetError::BadRequest("enter a network name of 1 to 32 bytes".into()));
    }
    Ok(())
}

/// WPA passphrases are 8 to 63 printable ASCII characters, or 64 hex digits (a raw key).
fn check_password(password: &str) -> Result<(), HostNetError> {
    let ok = password.is_empty()
        || ((8..=63).contains(&password.len())
            && password.bytes().all(|b| (0x20..0x7f).contains(&b)))
        || (password.len() == 64 && password.bytes().all(|b| b.is_ascii_hexdigit()));
    if !ok {
        return Err(HostNetError::BadRequest(
            "a Wi-Fi password has 8 to 63 characters (or leave it empty for an open network)"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    #[test]
    fn input_checks() {
        assert!(check_ssid("home").is_ok());
        assert!(check_ssid("").is_err());
        assert!(check_ssid(&"x".repeat(33)).is_err());
        assert!(check_ssid("a\nb").is_err());
        assert!(check_password("").is_ok());
        assert!(check_password("short").is_err());
        assert!(check_password("correct horse").is_ok());
        assert!(check_password(&"a".repeat(64)).is_ok(), "64 hex digits: a raw key");
        assert!(check_password(&"z".repeat(64)).is_err());
        assert!(check_password("pässwort123").is_err(), "non-ASCII");
    }

    #[test]
    fn replies() {
        assert_eq!(parse_reply(br#"{"ok":true,"x":1}"#).unwrap()["x"], 1);
        assert!(matches!(parse_reply(br#"{"ok":false,"error":"wrong password"}"#),
            Err(HostNetError::Failed(m)) if m == "wrong password"));
        assert!(matches!(parse_reply(b"nope"), Err(HostNetError::Unavailable(_))));
    }

    #[tokio::test]
    async fn talks_to_the_helper_over_a_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            tokio::io::BufReader::new(read).read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            let reply = json!({ "ok": true, "echo": req });
            write.write_all(reply.to_string().as_bytes()).await.unwrap();
        });
        let net = HostNetwork::new(&path);
        let reply = net.connect("my net", "password123", true).await.unwrap();
        assert_eq!(
            reply["echo"],
            json!({ "cmd": "connect", "ssid": "my net", "password": "password123", "hidden": true })
        );
        server.await.unwrap();
        assert!(matches!(net.set_country("d").await, Err(HostNetError::BadRequest(_))));
        assert!(matches!(
            HostNetwork::new(dir.path().join("missing.sock")).status().await,
            Err(HostNetError::Unavailable(_))
        ));
    }
}
