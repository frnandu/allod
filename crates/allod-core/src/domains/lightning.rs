//! Lightning connectivity (SPEC §12.7.2): how other nodes open connections to this one.
//!
//! The owner picks a clearnet path — none, a gateway, or the host's own public address —
//! and whether the node is also reachable at an onion address over Tor. No path at all
//! makes the node private: it still connects out and opens channels, but nobody can
//! connect to it.
//!
//! Allod keeps these files in step with the choice:
//!
//! - `{handoff_dir}/lightning-announce.json`: what ldk-server announces and where it
//!   listens (`listen`: `loopback` private, `internal` only inside the stack — Tor and
//!   the gateway tunnel —, `all` also on the port Compose publishes on the host). Its
//!   entrypoint restarts the node when the file changes; without the file the stack's
//!   own settings apply (`LDK_ANNOUNCE`, both listeners).
//! - `{tunnel_dir}/lightning` and `{tunnel_dir}/routes/<tunnel id>.lightning`: through
//!   a gateway, the home tunnel forwards Lightning connections and the gateway passes its
//!   public port on (gateways fetch the file over the tunnel every minute).
//! - `{tor_dir}/lightning`: the tor-proxy service publishes an onion service for the
//!   node while it exists, and writes the onion's host name to `{tor_dir}/lightning.onion`
//!   (kept when Tor is turned off, so the address stays the same next time).
//!
//! Only inbound connections take these paths: the connections this node opens to clearnet
//! peers still leave from the home IP.

use std::net::IpAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{now, store, DomainError, DomainService, Gateway};

/// `config` key of the owner's choice.
pub const CONFIG_KEY: &str = "lightning.connectivity";
/// The earlier setting (a gateway only), read as "clearnet through that gateway".
pub const LEGACY_CONFIG_KEY: &str = "lightning.gateway";
/// ldk-server's Lightning port inside the stack (tunnel and onion service targets), and
/// the onion service's port.
pub const NODE_PORT: u16 = 9735;
/// Written into the handoff directory for ldk-server's entrypoint.
pub const ANNOUNCE_FILE: &str = "lightning-announce.json";
/// Marker in the tunnel directory: the home tunnel forwards Lightning connections.
pub const TUNNEL_FILE: &str = "lightning";
/// Marker in the Tor directory: tor-proxy publishes the node's onion service.
pub const TOR_FILE: &str = "lightning";
/// Written by tor-proxy: the onion service's host name (`<56 chars>.onion`).
pub const ONION_FILE: &str = "lightning.onion";
/// How long the reachability check waits to connect, and then for the node to hang up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const TOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const HANGUP_WAIT: Duration = Duration::from_secs(3);

/// The clearnet path peers use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Clearnet {
    /// No clearnet address: private, or Tor only.
    #[default]
    None,
    /// A gateway's public IPv4: the home IP stays hidden.
    Gateway,
    /// The host's own public address (a router port forward): the home IP is published.
    Host,
}

/// The owner's choice. The gateway and host fields are kept while another path is
/// chosen, so switching back needs no typing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connectivity {
    #[serde(default)]
    pub tor: bool,
    #[serde(default)]
    pub clearnet: Clearnet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_id: Option<i64>,
    /// Public port on the gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_port: Option<u16>,
    /// Public IP or DNS name of the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Public port on the host (the router's forward).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_port: Option<u16>,
}

/// Lightning goes through `gateway_id`'s IPv4, on `port`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightningGateway {
    pub gateway_id: i64,
    pub port: u16,
}

/// What the last reachability check of one path found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reach {
    /// `reachable`; `no_node` (something forwards, but the node did not take it);
    /// `unreachable` (nothing answered at the address); `waiting` (no address yet);
    /// `unchecked` (published, but this installation cannot check it).
    pub state: &'static str,
    pub detail: String,
    pub checked_at: i64,
    /// The address that was checked.
    pub address: Option<String>,
}

/// The last checks, per path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LightningReach {
    pub clearnet: Option<Reach>,
    pub tor: Option<Reach>,
}

/// Ports a gateway already uses (SSH, HTTPS), so Lightning cannot have them.
fn check_gateway_port(port: u16) -> Result<(), DomainError> {
    match port {
        0 => Err(DomainError::BadRequest("enter a port between 1 and 65535".into())),
        22 | 443 => Err(DomainError::BadRequest(format!(
            "port {port} is used by the gateway itself ({}); pick another, like 9735",
            if port == 22 { "SSH" } else { "HTTPS" }
        ))),
        _ => Ok(()),
    }
}

/// A public IP address or DNS name other nodes can reach; returned trimmed and lowercase.
fn check_host(host: &str) -> Result<String, DomainError> {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    let bad = |why: &str| Err(DomainError::BadRequest(format!("{h}: {why}")));
    if h.is_empty() {
        return Err(DomainError::BadRequest(
            "enter the public IP address or DNS name of this machine".into(),
        ));
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        let local = match ip {
            IpAddr::V4(v4) => {
                let [a, b, ..] = v4.octets();
                v4.is_private()
                    || v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
                    || (a == 100 && (64..128).contains(&b))
            }
            IpAddr::V6(v6) => {
                let s = v6.segments()[0];
                v6.is_loopback()
                    || v6.is_unspecified()
                    || (s & 0xfe00) == 0xfc00
                    || (s & 0xffc0) == 0xfe80
            }
        };
        if local {
            return bad(
                "a local network address; other nodes need the public one (your router's IP)",
            );
        }
        return Ok(ip.to_string());
    }
    if h.ends_with(".onion") {
        return bad("for an onion address, turn on Tor instead");
    }
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if h.len() > 253 || !h.contains('.') || !h.split('.').all(label_ok) {
        return bad("not an IP address or a DNS name");
    }
    if [".local", ".lan", ".home.arpa", ".internal", ".localhost"].iter().any(|s| h.ends_with(s)) {
        return bad("a local name; other nodes need a public one");
    }
    Ok(h)
}

/// `host:port`, with brackets around an IPv6 address.
fn join(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// A gateway's address for peers: `ipv4:port`.
fn gateway_address(g: &Gateway, port: u16) -> Option<String> {
    g.ipv4.as_ref().map(|ip| join(ip, port))
}

/// The onion host name tor-proxy wrote, if it looks like one.
fn valid_onion(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    let name = s.strip_suffix(".onion")?;
    (name.len() == 56 && name.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7')))
        .then(|| s.clone())
}

/// Services that answer with the address a request comes from. Asked from this machine,
/// so the answer is the IP its outgoing connections use. Cloudflare's needs no DNS lookup.
const IP_ECHO: [&str; 2] = ["https://1.1.1.1/cdn-cgi/trace", "https://api.ipify.org"];

/// The address in an echo service's answer: a `ip=` line (Cloudflare's trace) or the
/// whole body (ipify).
fn parse_echo(body: &str) -> Option<IpAddr> {
    let line = body.lines().find_map(|l| l.trim().strip_prefix("ip=")).unwrap_or(body);
    line.trim().parse().ok()
}

/// A name for the service, for the page.
fn echo_name(url: &str) -> String {
    if url.contains("1.1.1.1") {
        return "Cloudflare (1.1.1.1)".into();
    }
    if url.contains("ipify") {
        return "ipify.org".into();
    }
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.into())
}

/// How a connection attempt ended.
enum Probe {
    Timeout,
    Failed(String),
    /// Accepted, then closed at once: something forwards, but no node behind it.
    Hangup,
    /// Accepted and silent (the node waits for the peer's handshake), or data.
    Open,
}

/// Waits briefly on an open connection: a node keeps it open, a forwarder without a node
/// behind it hangs up.
async fn settle(mut stream: TcpStream) -> Probe {
    let mut buf = [0u8; 64];
    match tokio::time::timeout(HANGUP_WAIT, stream.read(&mut buf)).await {
        Ok(Ok(0)) | Ok(Err(_)) => Probe::Hangup,
        _ => Probe::Open,
    }
}

async fn probe_direct(addr: &str) -> Probe {
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
        Err(_) => Probe::Timeout,
        Ok(Err(e)) => Probe::Failed(e.to_string()),
        Ok(Ok(s)) => settle(s).await,
    }
}

/// Opens `host:port` through a SOCKS5 proxy (Tor), without authentication.
async fn socks_connect(proxy: &str, host: &str, port: u16) -> Result<TcpStream, String> {
    let io = |e: std::io::Error| format!("Tor proxy {proxy}: {e}");
    let mut s = TcpStream::connect(proxy).await.map_err(io)?;
    s.write_all(&[5, 1, 0]).await.map_err(io)?;
    let mut hello = [0u8; 2];
    s.read_exact(&mut hello).await.map_err(io)?;
    if hello != [5, 0] {
        return Err(format!("Tor proxy {proxy} refused the SOCKS handshake"));
    }
    let mut req = vec![5, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await.map_err(io)?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await.map_err(io)?;
    if head[1] != 0 {
        return Err(match head[1] {
            4 => "Tor could not find the onion service (host unreachable)".into(),
            5 => "the onion service refused the connection".into(),
            6 => "Tor gave up on the circuit (TTL expired)".into(),
            c => format!("Tor could not connect (SOCKS error {c})"),
        });
    }
    let skip = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut n = [0u8; 1];
            s.read_exact(&mut n).await.map_err(io)?;
            n[0] as usize
        }
        _ => 0,
    } + 2;
    let mut rest = vec![0u8; skip];
    s.read_exact(&mut rest).await.map_err(io)?;
    Ok(s)
}

async fn probe_tor(proxy: &str, onion: &str) -> Probe {
    match tokio::time::timeout(TOR_CONNECT_TIMEOUT, socks_connect(proxy, onion, NODE_PORT)).await {
        Err(_) => Probe::Timeout,
        Ok(Err(e)) => Probe::Failed(e),
        Ok(Ok(s)) => settle(s).await,
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

/// Where the choice leads: the addresses to announce and how the node listens.
struct Plan {
    clearnet: Option<String>,
    onion: Option<String>,
    listen: &'static str,
}

impl Plan {
    fn addresses(&self) -> Vec<String> {
        self.clearnet.iter().chain(self.onion.iter()).cloned().collect()
    }
}

impl DomainService {
    /// The saved choice (or the earlier gateway setting); `None` only before
    /// [`Self::ensure_connectivity`] first ran.
    pub async fn connectivity(&self) -> Result<Option<Connectivity>, DomainError> {
        if let Some(e) = self.db.config_get(CONFIG_KEY).await? {
            return Ok(serde_json::from_str(&e.value).ok());
        }
        Ok(self
            .db
            .config_get(LEGACY_CONFIG_KEY)
            .await?
            .and_then(|e| serde_json::from_str::<LightningGateway>(&e.value).ok())
            .map(|g| Connectivity {
                clearnet: Clearnet::Gateway,
                gateway_id: Some(g.gateway_id),
                gateway_port: Some(g.port),
                ..Default::default()
            }))
    }

    /// The gateway Lightning goes through, if it does.
    pub async fn lightning_config(&self) -> Result<Option<LightningGateway>, DomainError> {
        Ok(self.connectivity().await?.and_then(|c| match (c.clearnet, c.gateway_id) {
            (Clearnet::Gateway, Some(id)) => {
                Some(LightningGateway { gateway_id: id, port: c.gateway_port.unwrap_or(NODE_PORT) })
            }
            _ => None,
        }))
    }

    /// The choice an installation starts with: the address the stack announced so far
    /// (`ALLOD_ANNOUNCE_ADDRESSES`, a router port forward) as "home IP", with Tor if it
    /// held an onion; otherwise private.
    fn default_connectivity(&self) -> Connectivity {
        let mut c = Connectivity::default();
        for a in &self.cfg.env_announce {
            let Some((host, port)) = a.trim().rsplit_once(':') else { continue };
            let Ok(port) = port.parse::<u16>() else { continue };
            if host.ends_with(".onion") {
                c.tor |= self.cfg.tor_dir.is_some();
            } else if c.clearnet == Clearnet::None {
                if let Ok(h) = check_host(host) {
                    c.clearnet = Clearnet::Host;
                    c.host = Some(h);
                    c.host_port = Some(port);
                }
            }
        }
        c
    }

    /// Saves the starting choice once, so the settings always show the active option and
    /// the node's listeners and announcements always follow one.
    pub async fn ensure_connectivity(&self) -> Result<Connectivity, DomainError> {
        if self.db.config_get(CONFIG_KEY).await?.is_none() {
            let c = match self.connectivity().await? {
                Some(earlier) => earlier,
                None => {
                    let c = self.default_connectivity();
                    tracing::info!(
                        tor = c.tor,
                        clearnet = ?c.clearnet,
                        "lightning: connectivity starts from the stack's settings"
                    );
                    c
                }
            };
            let value =
                serde_json::to_string(&c).map_err(|e| DomainError::Internal(e.to_string()))?;
            self.db.config_set(CONFIG_KEY, &value, false).await?;
            self.db.config_delete(LEGACY_CONFIG_KEY).await?;
        }
        Ok(self.connectivity().await?.unwrap_or_default())
    }

    /// The host port Compose publishes for ldk-server (the usual router forward target).
    fn default_host_port(&self) -> u16 {
        self.cfg.lightning_host_port.unwrap_or(NODE_PORT)
    }

    /// Saves the choice. Gateway and host fields left out keep their saved values.
    pub async fn set_connectivity(&self, mut c: Connectivity) -> Result<(), DomainError> {
        {
            {
                let prev = self.connectivity().await?.unwrap_or_default();
                c.gateway_id = c.gateway_id.or(prev.gateway_id);
                c.gateway_port = c.gateway_port.or(prev.gateway_port);
                c.host = c.host.or(prev.host);
                c.host_port = c.host_port.or(prev.host_port);
                if c.tor && self.cfg.tor_dir.is_none() {
                    return Err(DomainError::BadRequest(
                        "this installation runs no Tor service for the node".into(),
                    ));
                }
                match c.clearnet {
                    Clearnet::None => {}
                    Clearnet::Gateway => {
                        let id = c.gateway_id.ok_or_else(|| {
                            DomainError::BadRequest("pick the gateway to use".into())
                        })?;
                        let g = store::gateway(&self.pool, id)
                            .await?
                            .ok_or_else(|| super::not_found("gateway"))?;
                        if g.state == "expired" {
                            return Err(DomainError::BadRequest(format!(
                                "{} has expired; renew it or pick another gateway",
                                g.label
                            )));
                        }
                        let port = c.gateway_port.unwrap_or(NODE_PORT);
                        check_gateway_port(port)?;
                        c.gateway_port = Some(port);
                    }
                    Clearnet::Host => {
                        c.host = Some(check_host(c.host.as_deref().unwrap_or(""))?);
                        let port = c.host_port.unwrap_or(self.default_host_port());
                        if port == 0 {
                            return Err(DomainError::BadRequest(
                                "enter a port between 1 and 65535".into(),
                            ));
                        }
                        c.host_port = Some(port);
                    }
                }
                if c.clearnet != Clearnet::Host {
                    // An entry that failed the checks is not kept for later.
                    c.host = c.host.filter(|h| check_host(h).is_ok());
                }
                let value =
                    serde_json::to_string(&c).map_err(|e| DomainError::Internal(e.to_string()))?;
                self.db.config_set(CONFIG_KEY, &value, false).await?;
            }
        }
        self.db.config_delete(LEGACY_CONFIG_KEY).await?;
        self.status.lock().unwrap().lightning = LightningReach::default();
        let gateways = store::list_gateways(&self.pool).await?;
        let mut errors = Vec::new();
        self.lightning_files(&gateways, &mut errors).await;
        if let Some(e) = errors.into_iter().next() {
            return Err(DomainError::Internal(e));
        }
        self.trigger();
        Ok(())
    }

    /// The onion host name tor-proxy published, if any.
    fn onion(&self) -> Option<String> {
        let dir = self.cfg.tor_dir.as_ref()?;
        valid_onion(&std::fs::read_to_string(dir.join(ONION_FILE)).ok()?)
    }

    fn plan(&self, c: &Connectivity, gateways: &[Gateway]) -> Plan {
        let clearnet = match c.clearnet {
            Clearnet::None => None,
            Clearnet::Gateway => c
                .gateway_id
                .and_then(|id| gateways.iter().find(|g| g.id == id))
                .and_then(|g| gateway_address(g, c.gateway_port.unwrap_or(NODE_PORT))),
            Clearnet::Host => {
                c.host.as_deref().map(|h| join(h, c.host_port.unwrap_or(self.default_host_port())))
            }
        };
        let onion = c.tor.then(|| self.onion()).flatten().map(|o| join(&o, NODE_PORT));
        let listen = match (c.clearnet, c.tor) {
            (Clearnet::Host, _) => "all",
            (Clearnet::Gateway, _) | (_, true) => "internal",
            (Clearnet::None, false) => "loopback",
        };
        Plan { clearnet, onion, listen }
    }

    /// Keeps the tunnel, Tor and handoff files in step with the choice (every pass).
    pub(super) async fn lightning_files(&self, gateways: &[Gateway], errors: &mut Vec<String>) {
        let choice = match self.ensure_connectivity().await {
            Ok(c) => Some(c),
            Err(e) => {
                errors.push(format!("lightning: {e}"));
                return;
            }
        };
        let mut push = |r: Result<bool, String>| {
            if let Err(e) = r {
                errors.push(format!("lightning: {e}"));
            }
        };
        let via_gateway = choice
            .as_ref()
            .filter(|c| c.clearnet == Clearnet::Gateway)
            .and_then(|c| c.gateway_id.and_then(|id| gateways.iter().find(|g| g.id == id)))
            .map(|g| (g.id, choice.as_ref().and_then(|c| c.gateway_port).unwrap_or(NODE_PORT)));
        if let Some(dir) = &self.cfg.tunnel_dir {
            let routes = dir.join("routes");
            let _ = std::fs::create_dir_all(&routes);
            // Every gateway gets its file, so one that no longer carries Lightning stops.
            for g in gateways {
                let Some(t) = self.tunnel(g.id) else { continue };
                let port = via_gateway
                    .filter(|(id, _)| *id == g.id)
                    .map(|(_, p)| p.to_string())
                    .unwrap_or_default();
                push(write_if_changed(&routes.join(format!("{}.lightning", t.id)), &port, 0o644));
            }
            let marker = dir.join(TUNNEL_FILE);
            push(if via_gateway.is_some() {
                write_if_changed(&marker, &NODE_PORT.to_string(), 0o644)
            } else {
                remove_if_present(&marker)
            });
        }
        let tor = choice.as_ref().is_some_and(|c| c.tor);
        if let Some(dir) = &self.cfg.tor_dir {
            // tor-proxy writes the onion host name here as group 1000, whichever image
            // first created the volume (and with which mode).
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = std::fs::metadata(dir) {
                    let mode = m.permissions().mode();
                    if mode & 0o070 != 0o070 {
                        let _ = std::fs::set_permissions(
                            dir,
                            std::fs::Permissions::from_mode((mode | 0o070) & 0o7777),
                        );
                    }
                }
            }
            let marker = dir.join(TOR_FILE);
            push(if tor {
                write_if_changed(&marker, &NODE_PORT.to_string(), 0o644)
            } else {
                remove_if_present(&marker)
            });
        }
        self.onion_waiting
            .store(tor && self.cfg.tor_dir.is_some() && self.onion().is_none(), Relaxed);
        if let Some(dir) = &self.cfg.handoff_dir {
            let file = dir.join(ANNOUNCE_FILE);
            match &choice {
                Some(c) => {
                    let plan = self.plan(c, gateways);
                    let addresses = plan.addresses();
                    let _ = std::fs::create_dir_all(dir);
                    let body = json!({ "addresses": addresses, "listen": plan.listen }).to_string();
                    let r = write_if_changed(&file, &body, 0o644);
                    if r == Ok(true) {
                        tracing::info!(
                            ?addresses,
                            listen = plan.listen,
                            "lightning: new connectivity (ldk-server restarts)"
                        );
                    }
                    push(r);
                }
                None => {
                    let r = remove_if_present(&file);
                    if r == Ok(true) {
                        tracing::info!(
                            "lightning: back to the stack's own settings (ldk-server restarts)"
                        );
                    }
                    push(r);
                }
            }
        }
    }

    /// Checks the clearnet path the way a peer would.
    pub async fn lightning_check_clearnet(&self, gateways: &[Gateway]) {
        let c = self.connectivity().await.ok().flatten().unwrap_or_default();
        let reach = match c.clearnet {
            Clearnet::None => None,
            kind => {
                let addr = self.plan(&c, gateways).clearnet;
                let (state, detail) = match (&addr, kind) {
                    (None, Clearnet::Gateway) => {
                        ("waiting", "the gateway has no public IPv4 yet".to_string())
                    }
                    (None, _) => ("waiting", "no public address entered".to_string()),
                    (Some(a), Clearnet::Gateway) => match probe_direct(a).await {
                        Probe::Timeout => (
                            "unreachable",
                            format!(
                                "no answer from {a}: a firewall at the server or its provider may \
                                 drop the port, or the gateway is offline"
                            ),
                        ),
                        Probe::Failed(e) => (
                            "unreachable",
                            format!(
                                "{a} refused the connection ({e}): the gateway does not pass the \
                                 port on yet. It picks the change up within a minute; a gateway set \
                                 up with an older Allod needs its setup script run once more"
                            ),
                        ),
                        Probe::Hangup => (
                            "no_node",
                            format!(
                                "{a} answers, but the connection ends at once: the Lightning node is \
                                 not running, or this installation does not run the tunnel service"
                            ),
                        ),
                        Probe::Open => ("reachable", format!("peers can connect at {a}")),
                    },
                    (Some(a), _) => match probe_direct(a).await {
                        Probe::Open => ("reachable", format!("peers can connect at {a}")),
                        Probe::Hangup => (
                            "no_node",
                            format!(
                                "{a} answers, but the connection ends at once: the port is \
                                 forwarded to something other than the Lightning node"
                            ),
                        ),
                        Probe::Timeout | Probe::Failed(_) => (
                            "unreachable",
                            format!(
                                "no connection to {a}: forward the port on your router to this \
                                 machine and open it in its firewall. This check runs from inside \
                                 your network, so a router without hairpin NAT fails it even when \
                                 the outside can connect"
                            ),
                        ),
                    },
                };
                Some(Reach { state, detail, checked_at: now(), address: addr })
            }
        };
        self.store_reach(|r| &mut r.clearnet, reach, "clearnet");
    }

    /// Connects to the onion address through Tor, as a Tor peer would.
    pub async fn lightning_check_tor(&self) {
        let c = self.connectivity().await.ok().flatten().unwrap_or_default();
        let reach = if !c.tor {
            None
        } else {
            let onion = self.onion();
            let address = onion.as_ref().map(|o| join(o, NODE_PORT));
            let (state, detail) = match (&onion, &self.cfg.tor_proxy) {
                _ if self.cfg.tor_dir.is_none() => {
                    ("waiting", "this installation runs no Tor service for the node".to_string())
                }
                (None, _) => ("waiting", "Tor is creating the onion address".to_string()),
                (Some(_), None) => (
                    "unchecked",
                    "published; this installation has no Tor proxy to check it".to_string(),
                ),
                (Some(o), Some(proxy)) => {
                    let a = address.clone().unwrap_or_default();
                    match probe_tor(proxy, o).await {
                        Probe::Open => ("reachable", format!("Tor peers can connect at {a}")),
                        Probe::Hangup => (
                            "no_node",
                            "the onion service answers, but the connection ends at once: the \
                             Lightning node is not running"
                                .to_string(),
                        ),
                        Probe::Timeout => (
                            "unreachable",
                            format!(
                                "no answer through Tor within {}s: a new onion address can take a \
                                 few minutes to become reachable",
                                TOR_CONNECT_TIMEOUT.as_secs()
                            ),
                        ),
                        Probe::Failed(e) => (
                            "unreachable",
                            format!(
                                "{e}. A new onion address can take a few minutes to become reachable"
                            ),
                        ),
                    }
                }
            };
            Some(Reach { state, detail, checked_at: now(), address })
        };
        self.store_reach(|r| &mut r.tor, reach, "tor");
    }

    fn store_reach(
        &self,
        slot: impl Fn(&mut LightningReach) -> &mut Option<Reach>,
        reach: Option<Reach>,
        path: &str,
    ) {
        let mut st = self.status.lock().unwrap();
        let cur = slot(&mut st.lightning);
        if let Some(r) = &reach {
            if cur.as_ref().map(|c| c.state) != Some(r.state) {
                tracing::info!(path, state = r.state, detail = %r.detail, "lightning reachability");
            }
        }
        *cur = reach;
    }

    /// Both paths (each full pass).
    pub(super) async fn lightning_check(&self, gateways: &[Gateway]) {
        self.lightning_check_clearnet(gateways).await;
        self.lightning_check_tor().await;
    }

    /// Checks again now (the "Check now" button).
    pub async fn lightning_recheck(&self) {
        let gateways = store::list_gateways(&self.pool).await.unwrap_or_default();
        self.lightning_check(&gateways).await;
    }

    /// Checks the clearnet path now (it answers within seconds; Tor takes longer).
    pub async fn lightning_recheck_clearnet(&self) {
        let gateways = store::list_gateways(&self.pool).await.unwrap_or_default();
        self.lightning_check_clearnet(&gateways).await;
    }

    /// The public IP this machine's outgoing connections come from, as an echo service sees
    /// it: the starting value of the home IP option. Returns `{ip, source}`.
    pub async fn detect_public_ip(&self) -> Result<Value, DomainError> {
        let urls: Vec<String> = if self.cfg.ip_echo_urls.is_empty() {
            IP_ECHO.iter().map(|u| u.to_string()).collect()
        } else {
            self.cfg.ip_echo_urls.clone()
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|e| DomainError::Internal(e.to_string()))?;
        let mut failures = Vec::new();
        for url in &urls {
            let answer = match http.get(url).send().await.and_then(|r| r.error_for_status()) {
                Ok(r) => r.text().await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            match answer.map(|b| parse_echo(&b)) {
                Ok(Some(ip)) => match check_host(&ip.to_string()) {
                    Ok(ip) => return Ok(json!({ "ip": ip, "source": echo_name(url) })),
                    Err(e) => failures.push(format!("{}: {e}", echo_name(url))),
                },
                Ok(None) => failures.push(format!("{}: no address in the answer", echo_name(url))),
                Err(e) => failures.push(format!("{}: {e}", echo_name(url))),
            }
        }
        Err(DomainError::Upstream(format!(
            "could not detect the public IP ({}); enter it yourself",
            failures.join("; ")
        )))
    }

    /// Settings → Lightning → Connectivity.
    pub async fn lightning_view(&self) -> Result<Value, DomainError> {
        let c = self.ensure_connectivity().await?;
        let gateways = store::list_gateways(&self.pool).await?;
        let plan = self.plan(&c, &gateways);
        let reach = self.status.lock().unwrap().lightning.clone();
        Ok(json!({
            "tor": c.tor,
            "clearnet": c.clearnet,
            "gateway_id": c.gateway_id,
            "port": c.gateway_port,
            "host": c.host,
            "host_port": c.host_port,
            "default_host_port": self.default_host_port(),
            "addresses": plan.addresses(),
            "address": plan.clearnet,
            "onion_address": plan.onion,
            "node_port": NODE_PORT,
            "managed": self.cfg.handoff_dir.is_some(),
            "tunnel": self.cfg.tunnel_dir.is_some(),
            "tor_available": self.cfg.tor_dir.is_some(),
            "tor_checked": self.cfg.tor_proxy.is_some(),
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
        assert!(check_gateway_port(9735).is_ok());
        assert!(check_gateway_port(19735).is_ok());
        for p in [0, 22, 443] {
            assert!(check_gateway_port(p).is_err(), "{p}");
        }
    }

    #[test]
    fn hosts() {
        assert_eq!(check_host(" 203.0.113.7 ").unwrap(), "203.0.113.7");
        assert_eq!(check_host("[2001:DB8::1]").unwrap(), "2001:db8::1");
        assert_eq!(check_host("Node.Example.com").unwrap(), "node.example.com");
        for h in [
            "",
            "192.168.1.20",
            "10.0.0.2",
            "127.0.0.1",
            "100.100.1.1",
            "fd00::1",
            "fe80::1",
            "localhost",
            "box.local",
            "bad_name.com",
            "abc.onion",
        ] {
            assert!(check_host(h).is_err(), "{h}");
        }
        assert_eq!(join("2001:db8::1", 9735), "[2001:db8::1]:9735");
        assert_eq!(join("1.2.3.4", 9735), "1.2.3.4:9735");
    }

    #[test]
    fn echo_answers() {
        let trace = "fl=12f\nh=1.1.1.1\nip=203.0.113.9\nts=1.2\n";
        assert_eq!(parse_echo(trace), Some("203.0.113.9".parse().unwrap()));
        assert_eq!(parse_echo("2001:db8::5\n"), Some("2001:db8::5".parse().unwrap()));
        assert_eq!(parse_echo("<html>"), None);
        assert_eq!(echo_name(IP_ECHO[0]), "Cloudflare (1.1.1.1)");
        assert_eq!(echo_name("http://127.0.0.1:5/ip"), "127.0.0.1");
    }

    #[test]
    fn onions() {
        let o = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
        assert_eq!(valid_onion(&format!(" {}\n", o.to_uppercase())).as_deref(), Some(o));
        assert!(valid_onion("abc.onion").is_none());
        assert!(valid_onion("example.com").is_none());
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
        assert!(matches!(probe_direct(&node_addr).await, Probe::Open));
        hold.abort();

        // The home tunnel without a node behind it: accepts, then hangs up.
        let tunnel = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel_addr = tunnel.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (s, _) = tunnel.accept().await.unwrap();
            drop(s);
        });
        assert!(matches!(probe_direct(&tunnel_addr).await, Probe::Hangup));

        // Nothing listens.
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let free_addr = free.local_addr().unwrap().to_string();
        drop(free);
        assert!(matches!(probe_direct(&free_addr).await, Probe::Failed(_)));
    }

    /// A minimal SOCKS5 server standing in for Tor: answers `rep` and, on success, keeps
    /// the connection open like a node would.
    async fn socks_stub(rep: u8) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 3];
            s.read_exact(&mut b).await.unwrap();
            s.write_all(&[5, 0]).await.unwrap();
            let mut head = [0u8; 5];
            s.read_exact(&mut head).await.unwrap();
            let mut rest = vec![0u8; head[4] as usize + 2];
            s.read_exact(&mut rest).await.unwrap();
            s.write_all(&[5, rep, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        addr
    }

    #[tokio::test]
    async fn probe_through_tor() {
        let onion = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
        assert!(matches!(probe_tor(&socks_stub(0).await, onion).await, Probe::Open));
        match probe_tor(&socks_stub(4).await, onion).await {
            Probe::Failed(e) => assert!(e.contains("host unreachable"), "{e}"),
            _ => panic!("expected a failure"),
        }
    }
}
