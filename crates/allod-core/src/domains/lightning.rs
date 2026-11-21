//! Lightning through a gateway (SPEC §12.7.2): other nodes reach this node's Lightning
//! port at a gateway's public IP, so the home IP is never published in gossip.
//!
//! The gateway passes one public TCP port over its WireGuard tunnel to the home end,
//! which hands it to ldk-server's port 9735. Allod keeps three files in step with the
//! owner's choice:
//!
//! - `{tunnel_dir}/lightning`: the home tunnel forwards Lightning connections while it
//!   exists (`images/tunnel/tunnel.sh`);
//! - `{tunnel_dir}/routes/<tunnel id>.lightning`: the public port each gateway should
//!   pass on (empty: none). Gateways fetch it over the tunnel every minute, so turning
//!   it on or off needs nothing on the server once its setup script has run;
//! - `{handoff_dir}/lightning-announce.json`: the address ldk-server announces
//!   (`gateway IPv4:port`); its entrypoint restarts the node when it changes.
//!
//! Only inbound connections go through the gateway: the connections this node opens to
//! peers still leave from the home IP.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{now, store, DomainError, DomainService, Gateway};

/// `config` key of the owner's choice.
pub const CONFIG_KEY: &str = "lightning.gateway";
/// ldk-server's Lightning port, where the home tunnel hands connections to.
pub const NODE_PORT: u16 = 9735;
/// Written into the handoff directory for ldk-server's entrypoint.
pub const ANNOUNCE_FILE: &str = "lightning-announce.json";
/// Marker in the tunnel directory: the home tunnel forwards Lightning connections.
pub const TUNNEL_FILE: &str = "lightning";
/// How long the reachability check waits to connect, and then for the node to hang up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const HANGUP_WAIT: Duration = Duration::from_secs(3);

/// The owner's choice: Lightning connections arrive at `gateway_id`'s IPv4, on `port`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightningGateway {
    pub gateway_id: i64,
    pub port: u16,
}

/// What the last reachability check found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reach {
    /// `reachable`, `no_node` (the gateway forwards, but the node did not take it),
    /// `unreachable` (nothing answered at the address) or `waiting` (no address yet).
    pub state: &'static str,
    pub detail: String,
    pub checked_at: i64,
    /// The address that was checked.
    pub address: Option<String>,
}

/// Ports a gateway already uses (SSH, HTTPS), so Lightning cannot have them.
fn check_port(port: u16) -> Result<(), DomainError> {
    match port {
        0 => Err(DomainError::BadRequest("enter a port between 1 and 65535".into())),
        22 | 443 => Err(DomainError::BadRequest(format!(
            "port {port} is used by the gateway itself ({}); pick another, like 9735",
            if port == 22 { "SSH" } else { "HTTPS" }
        ))),
        _ => Ok(()),
    }
}

/// The address peers use: `ipv4:port`.
fn address(g: &Gateway, port: u16) -> Option<String> {
    g.ipv4.as_ref().map(|ip| format!("{ip}:{port}"))
}

/// Connects to `addr` the way a peer would. A node behind the tunnel waits silently for
/// the Noise handshake; when the home tunnel accepts but cannot reach ldk-server it
/// hangs up at once.
async fn probe(addr: &str) -> (&'static str, String) {
    use tokio::io::AsyncReadExt;
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr)).await;
    let mut stream = match conn {
        Err(_) => {
            return (
                "unreachable",
                format!(
                    "no answer from {addr}: a firewall at the server or its provider may drop the port, \
                     or the gateway is offline"
                ),
            )
        }
        Ok(Err(e)) => {
            return (
                "unreachable",
                format!(
                    "{addr} refused the connection ({e}): the gateway does not pass the port on yet. \
                     It picks the change up within a minute; a gateway set up with an older Allod \
                     needs its setup script run once more"
                ),
            )
        }
        Ok(Ok(s)) => s,
    };
    let mut buf = [0u8; 64];
    match tokio::time::timeout(HANGUP_WAIT, stream.read(&mut buf)).await {
        Ok(Ok(0)) | Ok(Err(_)) => (
            "no_node",
            format!(
                "{addr} answers, but the connection ends at once: the Lightning node is not running, \
                 or this installation does not run the tunnel service"
            ),
        ),
        // Silence (the node waits for the peer's first handshake message), or data.
        _ => ("reachable", format!("peers can connect at {addr}")),
    }
}

/// Writes `content` to `path` unless it already holds it.
fn write_if_changed(path: &std::path::Path, content: &str, mode: u32) -> Result<bool, String> {
    if std::fs::read_to_string(path).ok().map(|c| c.trim_end().to_string())
        == Some(content.trim_end().to_string())
    {
        return Ok(false);
    }
    allod_keys::write_file_with_mode(path, content, mode)
        .map(|_| true)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn remove_if_present(path: &std::path::Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

impl DomainService {
    /// The owner's choice, if Lightning goes through a gateway.
    pub async fn lightning_config(&self) -> Result<Option<LightningGateway>, DomainError> {
        Ok(self
            .db
            .config_get(CONFIG_KEY)
            .await?
            .and_then(|e| serde_json::from_str::<LightningGateway>(&e.value).ok()))
    }

    /// Sends Lightning connections through `gateway_id` on `port` (default 9735), or stops
    /// (`None`).
    pub async fn set_lightning_gateway(
        &self,
        gateway_id: Option<i64>,
        port: Option<u16>,
    ) -> Result<(), DomainError> {
        match gateway_id {
            None => {
                self.db.config_delete(CONFIG_KEY).await?;
            }
            Some(id) => {
                let g = store::gateway(&self.pool, id)
                    .await?
                    .ok_or_else(|| super::not_found("gateway"))?;
                if g.state == "expired" {
                    return Err(DomainError::BadRequest(format!(
                        "{} has expired; renew it or pick another gateway",
                        g.label
                    )));
                }
                let port = port.unwrap_or(NODE_PORT);
                check_port(port)?;
                let value = serde_json::to_string(&LightningGateway { gateway_id: id, port })
                    .map_err(|e| DomainError::Internal(e.to_string()))?;
                self.db.config_set(CONFIG_KEY, &value, false).await?;
            }
        }
        self.status.lock().unwrap().lightning = None;
        let gateways = store::list_gateways(&self.pool).await?;
        let mut errors = Vec::new();
        self.lightning_files(&gateways, &mut errors).await;
        if let Some(e) = errors.into_iter().next() {
            return Err(DomainError::Internal(e));
        }
        self.trigger();
        Ok(())
    }

    /// Keeps the tunnel and handoff files in step with the choice (every pass).
    pub(super) async fn lightning_files(&self, gateways: &[Gateway], errors: &mut Vec<String>) {
        let chosen = match self.lightning_config().await {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("lightning: {e}"));
                return;
            }
        };
        let gateway = chosen.as_ref().and_then(|c| gateways.iter().find(|g| g.id == c.gateway_id));
        if let Some(dir) = &self.cfg.tunnel_dir {
            let routes = dir.join("routes");
            let _ = std::fs::create_dir_all(&routes);
            // Every gateway gets its file, so one that no longer carries Lightning stops.
            for g in gateways {
                let Some(t) = self.tunnel(g.id) else { continue };
                let port = chosen
                    .as_ref()
                    .filter(|c| c.gateway_id == g.id)
                    .map(|c| c.port.to_string())
                    .unwrap_or_default();
                if let Err(e) =
                    write_if_changed(&routes.join(format!("{}.lightning", t.id)), &port, 0o644)
                {
                    errors.push(format!("lightning: {e}"));
                }
            }
            let marker = dir.join(TUNNEL_FILE);
            let r = if gateway.is_some() {
                write_if_changed(&marker, &NODE_PORT.to_string(), 0o644)
            } else {
                remove_if_present(&marker)
            };
            if let Err(e) = r {
                errors.push(format!("lightning: {e}"));
            }
        }
        if let Some(dir) = &self.cfg.handoff_dir {
            let file = dir.join(ANNOUNCE_FILE);
            let announce = chosen.as_ref().zip(gateway).and_then(|(c, g)| address(g, c.port));
            let r = match &announce {
                Some(a) => {
                    let _ = std::fs::create_dir_all(dir);
                    let body = json!({ "addresses": [a] }).to_string();
                    let r = write_if_changed(&file, &body, 0o644);
                    if r == Ok(true) {
                        tracing::info!(address = %a, "lightning: the node announces the gateway's address (ldk-server restarts)");
                    }
                    r
                }
                None => {
                    let r = remove_if_present(&file);
                    if r == Ok(true) {
                        tracing::info!("lightning: the node no longer announces a gateway address (ldk-server restarts)");
                    }
                    r
                }
            };
            if let Err(e) = r {
                errors.push(format!("lightning: {e}"));
            }
        }
    }

    /// Connects to the node through its gateway, as a peer would (each full pass).
    pub(super) async fn lightning_check(&self, gateways: &[Gateway]) {
        let Ok(Some(c)) = self.lightning_config().await else {
            self.status.lock().unwrap().lightning = None;
            return;
        };
        let addr = gateways.iter().find(|g| g.id == c.gateway_id).and_then(|g| address(g, c.port));
        let (state, detail) = match &addr {
            Some(a) => probe(a).await,
            None => ("waiting", "the gateway has no public IPv4 yet".to_string()),
        };
        let reach = Reach { state, detail, checked_at: now(), address: addr };
        let mut st = self.status.lock().unwrap();
        if st.lightning.as_ref().map(|r| r.state) != Some(reach.state) {
            tracing::info!(state = reach.state, detail = %reach.detail, "lightning through the gateway");
        }
        st.lightning = Some(reach);
    }

    /// Checks again now (the "Check now" button).
    pub async fn lightning_recheck(&self) {
        let gateways = store::list_gateways(&self.pool).await.unwrap_or_default();
        self.lightning_check(&gateways).await;
    }

    /// Settings → Lightning.
    pub async fn lightning_view(&self) -> Result<Value, DomainError> {
        let chosen = self.lightning_config().await?;
        let gateways = store::list_gateways(&self.pool).await?;
        let gateway = chosen.as_ref().and_then(|c| gateways.iter().find(|g| g.id == c.gateway_id));
        let reach = self.status.lock().unwrap().lightning.clone();
        Ok(json!({
            "gateway_id": chosen.as_ref().map(|c| c.gateway_id),
            "port": chosen.as_ref().map(|c| c.port),
            "address": chosen.as_ref().zip(gateway).and_then(|(c, g)| address(g, c.port)),
            "node_port": NODE_PORT,
            "managed": self.cfg.handoff_dir.is_some(),
            "tunnel": self.cfg.tunnel_dir.is_some(),
            "reach": reach,
            "gateways": gateways.iter().map(|g| json!({
                "id": g.id,
                "label": g.label,
                "provider": g.provider,
                "ipv4": g.ipv4,
                "mode": g.mode,
                "state": g.state,
            })).collect::<Vec<_>>(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports() {
        assert!(check_port(9735).is_ok());
        assert!(check_port(19735).is_ok());
        for p in [0, 22, 443] {
            assert!(check_port(p).is_err(), "{p}");
        }
    }

    #[tokio::test]
    async fn probe_tells_a_node_from_a_hangup() {
        // A node: accepts and waits for the handshake.
        let node = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let node_addr = node.local_addr().unwrap().to_string();
        let hold = tokio::spawn(async move {
            let (s, _) = node.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
            drop(s);
        });
        assert_eq!(probe(&node_addr).await.0, "reachable");
        hold.abort();

        // The home tunnel without a node behind it: accepts, then hangs up.
        let tunnel = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel_addr = tunnel.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (s, _) = tunnel.accept().await.unwrap();
            drop(s);
        });
        assert_eq!(probe(&tunnel_addr).await.0, "no_node");

        // Nothing listens.
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let free_addr = free.local_addr().unwrap().to_string();
        drop(free);
        assert_eq!(probe(&free_addr).await.0, "unreachable");
    }
}
