//! Gateways (SPEC §12.7): WireGuard keys, the home tunnel's configuration and status,
//! the gateway's setup script, and adding, changing and removing gateways.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{now, store, Domain, DomainError, DomainService, Gateway};

/// The home end's WireGuard private key (one for all gateways).
const HOME_KEY: &str = "wireguard_home";
/// One gateway's tunnel, derived from the gateway's WireGuard public key, so tunnels of
/// different Allod installations on one server never collide: its id names the
/// interface (`allod-<id>`), units and files there; its UDP port is never 51820 (a VPN
/// already on the server keeps it); its addresses are a /24 in 10.64.0.0/10.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunnel {
    pub id: String,
    pub ifname: String,
    pub port: u16,
    pub gateway_ip: String,
    pub home_ip: String,
}

/// The tunnel of the gateway with this public key (base64).
pub fn tunnel_of(gateway_public: &str) -> Tunnel {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(gateway_public.trim().as_bytes());
    let id = hex::encode(&h[..3]);
    let port = 51821 + (u16::from_be_bytes([h[3], h[4]]) % 4000);
    let (a, b) = (64 + h[5] % 64, h[6]);
    Tunnel {
        ifname: format!("allod-{id}"),
        id,
        port,
        gateway_ip: format!("10.{a}.{b}.1"),
        home_ip: format!("10.{a}.{b}.2"),
    }
}
/// A handshake this recent means the tunnel is up.
const ACTIVE_SECS: i64 = 180;
/// No handshake for this long: offline.
const OFFLINE_SECS: i64 = 300;
/// `allod-relay` (SPEC §12.7.1), embedded in setup scripts for shared servers.
const ALLOD_RELAY: &str = include_str!("../../../../deploy/relay/allod-relay");
/// Port the home tunnel serves each gateway's domain list on (inside the tunnel only).
pub const ROUTES_PORT: u16 = 8089;
/// Version of the setup script's features: LNVPS gateways with an older one are set up
/// again when they need a newer feature (2: Lightning port forwarding).
pub const SCRIPT_VERSION: u32 = 2;

/// Lightning through the gateway (SPEC §12.7.2), in every setup script: a timer asks the
/// node over the tunnel which public port (if any) to pass on to its Lightning port, so
/// turning it on, off or onto another port needs nothing on the server.
const LIGHTNING_SYNC: &str = r#"
# Lightning (SPEC §12.7.2): when Allod sends Lightning connections through this gateway,
# one public TCP port here is passed to the node's port @NODE_PORT@. Allod says which (if
# any) over the tunnel; this follows it every minute.
need curl || apt-get install -y -qq curl >/dev/null
cat >/usr/local/sbin/allod-gateway-lightning-@ID@ <<'LN_EOF'
#!/bin/sh
# Passes the public Lightning port Allod asks for on to the node (SPEC §12.7.2).
set -u
state=/run/allod-gateway-lightning-@ID@
want=$(curl -fsS -m 10 http://@HOME@:@ROUTES_PORT@/@ID@.lightning) || exit 0  # tunnel down: keep it
want=$(printf '%s' "$want" | tr -dc 0-9)
have=$(cat "$state" 2>/dev/null || true)
if [ "$want" = "$have" ] && { [ -z "$want" ] || nft list table ip allod_@ID@_ln >/dev/null 2>&1; }; then
  exit 0
fi
ufw_on() { command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; }
nft delete table ip allod_@ID@_ln 2>/dev/null || true
if [ -n "$have" ] && ufw_on; then ufw delete allow "$have/tcp" >/dev/null 2>&1 || true; fi
if [ -n "$want" ]; then
  sysctl -qw net.ipv4.ip_forward=1
  nft -f - <<NFT
table ip allod_@ID@_ln {
  chain prerouting {
    type nat hook prerouting priority dstnat; policy accept;
    iifname != "@IFNAME@" tcp dport $want dnat to @HOME@:@NODE_PORT@
  }
  chain postrouting {
    type nat hook postrouting priority srcnat; policy accept;
    oifname "@IFNAME@" ip daddr @HOME@ tcp dport @NODE_PORT@ masquerade
  }
}
NFT
  # Docker or ufw may drop forwarded traffic: allow this flow.
  if command -v iptables >/dev/null 2>&1; then
    iptables -C FORWARD -o @IFNAME@ -d @HOME@ -p tcp --dport @NODE_PORT@ -j ACCEPT 2>/dev/null ||
      iptables -I FORWARD -o @IFNAME@ -d @HOME@ -p tcp --dport @NODE_PORT@ -j ACCEPT
    iptables -C FORWARD -i @IFNAME@ -s @HOME@ -p tcp --sport @NODE_PORT@ -j ACCEPT 2>/dev/null ||
      iptables -I FORWARD -i @IFNAME@ -s @HOME@ -p tcp --sport @NODE_PORT@ -j ACCEPT
  fi
  if ufw_on; then
    ufw allow "$want/tcp" >/dev/null
    ufw route allow proto tcp to @HOME@ port @NODE_PORT@ >/dev/null
  fi
  echo "Allod gateway @ID@: Lightning port $want -> @HOME@:@NODE_PORT@"
else
  echo "Allod gateway @ID@: Lightning off"
fi
printf '%s' "$want" >"$state"
LN_EOF
chmod 755 /usr/local/sbin/allod-gateway-lightning-@ID@
cat >/etc/systemd/system/allod-gateway-lightning-@ID@.service <<'LN_EOF'
[Unit]
Description=Allod gateway @ID@: pass the Lightning port Allod asks for to the node
After=wg-quick@@IFNAME@.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/allod-gateway-lightning-@ID@
LN_EOF
cat >/etc/systemd/system/allod-gateway-lightning-@ID@.timer <<'LN_EOF'
[Unit]
Description=Allod gateway @ID@: follow the node's Lightning port, every minute

[Timer]
OnBootSec=30s
OnUnitActiveSec=1min

[Install]
WantedBy=timers.target
LN_EOF
systemctl daemon-reload
systemctl enable --now allod-gateway-lightning-@ID@.timer >/dev/null 2>&1
/usr/local/sbin/allod-gateway-lightning-@ID@ || true
"#;

/// The Lightning part of one gateway's setup script.
fn lightning_section(t: &Tunnel) -> String {
    LIGHTNING_SYNC
        .replace("@IFNAME@", &t.ifname)
        .replace("@ID@", &t.id)
        .replace("@HOME@", &t.home_ip)
        .replace("@ROUTES_PORT@", &ROUTES_PORT.to_string())
        .replace("@NODE_PORT@", &super::lightning::NODE_PORT.to_string())
}

fn gateway_secret(id: i64) -> String {
    format!("gateway_{id}_wg")
}

/// A new WireGuard private key, base64.
fn new_private_key() -> String {
    let secret = x25519_dalek::StaticSecret::random_from_rng(rand::rngs::OsRng);
    base64::engine::general_purpose::STANDARD.encode(secret.to_bytes())
}

/// The public key of a base64 private key.
pub fn public_key(private_b64: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(private_b64.trim()).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(arr));
    Some(base64::engine::general_purpose::STANDARD.encode(public.as_bytes()))
}

fn check_ipv4(ip: &str) -> Result<String, DomainError> {
    ip.trim().parse::<Ipv4Addr>().map(|a| a.to_string()).map_err(|_| {
        DomainError::BadRequest("enter the server's public IPv4, like 203.0.113.7".into())
    })
}

fn check_mode(mode: &str) -> Result<(), DomainError> {
    match mode {
        "dedicated" | "shared" => Ok(()),
        _ => Err(DomainError::BadRequest("mode is dedicated or shared".into())),
    }
}

/// The JSON a gateway is shown as.
pub fn view(g: &Gateway, tunnel: Option<&Tunnel>, domains: &[Domain]) -> Value {
    json!({
        "id": g.id,
        "label": g.label,
        "provider": g.provider,
        "ipv4": g.ipv4,
        "ipv6": g.ipv6,
        "mode": g.mode,
        "state": g.state,
        "tunnel": tunnel.map(|t| json!({
            "id": t.id, "interface": t.ifname, "gateway": t.gateway_ip,
            "home": t.home_ip, "port": t.port,
        })),
        "last_handshake_at": g.last_handshake_at,
        "expires_at": g.expires_at,
        "auto_renew": g.auto_renew,
        "renew_max_sats": g.renew_max_sats,
        "error": g.error,
        "created_at": g.created_at,
        "domains": domains.iter().filter(|d| d.gateway_id == Some(g.id))
            .map(|d| d.name.clone()).collect::<Vec<_>>(),
        "lnvps": g.provider_data.as_deref().and_then(|d| serde_json::from_str::<Value>(d).ok()),
    })
}

/// One peer in the tunnel service's `status.json`.
#[derive(Debug, Deserialize)]
struct PeerStatus {
    public_key: String,
    #[serde(default)]
    latest_handshake: i64,
}

#[derive(Debug, Deserialize)]
struct TunnelStatus {
    #[serde(default)]
    peers: Vec<PeerStatus>,
}

impl DomainService {
    /// The home end's private key, made on first use.
    fn home_key(&self) -> Result<String, DomainError> {
        if let Some(k) = self.secrets.read(HOME_KEY)? {
            return Ok(k.to_string());
        }
        let k = new_private_key();
        self.secrets.write(HOME_KEY, &k)?;
        Ok(k)
    }

    fn gateway_key(&self, id: i64) -> Result<String, DomainError> {
        if let Some(k) = self.secrets.read(&gateway_secret(id))? {
            return Ok(k.to_string());
        }
        let k = new_private_key();
        self.secrets.write(&gateway_secret(id), &k)?;
        Ok(k)
    }

    /// The tunnel of a gateway (from its key).
    pub(super) fn tunnel(&self, id: i64) -> Option<Tunnel> {
        let public = public_key(&self.gateway_key(id).ok()?)?;
        Some(tunnel_of(&public))
    }

    /// Makes a new gateway's key, drawing again while its tunnel's port or addresses
    /// would collide with another gateway of this installation.
    pub(super) async fn new_gateway_key(&self, id: i64) -> Result<(), DomainError> {
        let others: Vec<Tunnel> = store::list_gateways(&self.pool)
            .await?
            .iter()
            .filter(|g| g.id != id)
            .filter_map(|g| self.tunnel(g.id))
            .collect();
        for _ in 0..64 {
            let k = new_private_key();
            let t = tunnel_of(&public_key(&k).unwrap_or_default());
            if others.iter().all(|o| o.port != t.port && o.home_ip != t.home_ip && o.id != t.id) {
                self.secrets.write(&gateway_secret(id), &k)?;
                return Ok(());
            }
        }
        Err(DomainError::Internal("no free tunnel for the gateway".into()))
    }

    /// Adds the owner's own server as a gateway.
    pub async fn add_gateway(
        &self,
        label: Option<&str>,
        ipv4: &str,
        mode: &str,
    ) -> Result<i64, DomainError> {
        let ipv4 = check_ipv4(ipv4)?;
        check_mode(mode)?;
        let index = store::free_tunnel_index(&self.pool).await?;
        let label = label.map(str::trim).filter(|l| !l.is_empty()).unwrap_or(&ipv4).to_string();
        let id =
            store::insert_gateway(&self.pool, &label, "manual", Some(&ipv4), mode, index).await?;
        self.new_gateway_key(id).await?;
        self.home_key()?;
        self.trigger();
        Ok(id)
    }

    pub async fn update_gateway(
        &self,
        id: i64,
        label: Option<&str>,
        ipv4: Option<&str>,
        mode: Option<&str>,
        auto_renew: Option<bool>,
        renew_max_sats: Option<i64>,
    ) -> Result<(), DomainError> {
        let g = store::gateway(&self.pool, id).await?.ok_or_else(|| super::not_found("gateway"))?;
        let ipv4 = match ipv4 {
            Some(ip) if g.provider == "manual" => Some(check_ipv4(ip)?),
            Some(_) => return Err(DomainError::BadRequest("LNVPS sets this gateway's IP".into())),
            None => g.ipv4.clone(),
        };
        if let Some(m) = mode {
            check_mode(m)?;
        }
        if renew_max_sats.is_some_and(|m| m < 0) {
            return Err(DomainError::BadRequest("the renewal maximum cannot be negative".into()));
        }
        sqlx::query(
            "UPDATE gateways SET label = $1, ipv4 = $2, mode = $3, auto_renew = $4, renew_max_sats = $5 \
             WHERE id = $6",
        )
        .bind(label.map(str::trim).filter(|l| !l.is_empty()).unwrap_or(&g.label))
        .bind(ipv4)
        .bind(mode.unwrap_or(&g.mode))
        .bind(auto_renew.unwrap_or(g.auto_renew))
        .bind(renew_max_sats.or(g.renew_max_sats))
        .bind(id)
        .execute(&self.pool)
        .await?;
        self.trigger();
        Ok(())
    }

    /// Forgets a gateway (an LNVPS one stops renewing). Its domains must move first.
    pub async fn remove_gateway(&self, id: i64) -> Result<(), DomainError> {
        store::gateway(&self.pool, id).await?.ok_or_else(|| super::not_found("gateway"))?;
        let used: Vec<String> = store::list_domains(&self.pool)
            .await?
            .into_iter()
            .filter(|d| d.gateway_id == Some(id))
            .map(|d| d.name)
            .collect();
        if !used.is_empty() {
            return Err(DomainError::Conflict(format!(
                "domains still use it: {}; serve them another way first",
                used.join(", ")
            )));
        }
        if self.lightning_config().await?.is_some_and(|l| l.gateway_id == id) {
            return Err(DomainError::Conflict(
                "Lightning connections come through it: pick another gateway or turn that off \
                 in Settings → Lightning first"
                    .into(),
            ));
        }
        sqlx::query("DELETE FROM gateways WHERE id = $1").bind(id).execute(&self.pool).await?;
        self.secrets.remove(&gateway_secret(id))?;
        self.trigger();
        Ok(())
    }

    /// The setup script for a gateway (SPEC §12.7): run as root on Debian/Ubuntu.
    pub async fn setup_script(&self, id: i64) -> Result<String, DomainError> {
        let g = store::gateway(&self.pool, id).await?.ok_or_else(|| super::not_found("gateway"))?;
        let domains: Vec<String> = store::list_domains(&self.pool)
            .await?
            .into_iter()
            .filter(|d| d.gateway_id == Some(id))
            .map(|d| d.name)
            .collect();
        let home_pub = public_key(&self.home_key()?)
            .ok_or_else(|| DomainError::Internal("bad home WireGuard key".into()))?;
        let key = self.gateway_key(id)?;
        let tunnel = tunnel_of(&public_key(&key).unwrap_or_default());
        Ok(setup_script(&g, &tunnel, &key, &home_pub, &domains))
    }

    /// Writes the home tunnel's `wg0.conf` and reads its `status.json` into the gateway
    /// rows (state, last handshake).
    pub(super) async fn gateway_health(&self, gateways: &[Gateway], errors: &mut Vec<String>) {
        let Some(dir) = self.cfg.tunnel_dir.clone() else { return };
        let conf = match self.home_config(gateways) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("tunnel: {e}"));
                return;
            }
        };
        let path = dir.join("wg0.conf");
        if std::fs::read_to_string(&path).ok().map(|c| c.trim_end().to_string())
            != Some(conf.trim_end().to_string())
        {
            if let Err(e) = allod_keys::write_file_with_mode(&path, &conf, 0o600) {
                errors.push(format!("tunnel: cannot write {}: {e}", path.display()));
            }
        }
        // Each gateway's domains, which a shared gateway fetches over the tunnel every
        // minute to keep its routes (SPEC §12.7).
        let domains = store::list_domains(&self.pool).await.unwrap_or_default();
        let routes = dir.join("routes");
        let _ = std::fs::create_dir_all(&routes);
        // Lists of gateways that are gone (or of an older layout) go too.
        let ids: Vec<String> =
            gateways.iter().filter_map(|g| self.tunnel(g.id)).map(|t| t.id).collect();
        if let Ok(entries) = std::fs::read_dir(&routes) {
            for e in entries.flatten() {
                // `<id>` (domains) and `<id>.lightning` (Lightning port, SPEC §12.7.2).
                let name = e.file_name().to_string_lossy().to_string();
                let id = name.strip_suffix(".lightning").unwrap_or(&name);
                if !ids.iter().any(|i| i == id) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        for g in gateways {
            let list: String = domains
                .iter()
                .filter(|d| d.gateway_id == Some(g.id))
                .map(|d| format!("{}\n", d.name))
                .collect();
            let Some(t) = self.tunnel(g.id) else { continue };
            let file = routes.join(&t.id);
            // The file writer ends it with a newline of its own.
            if std::fs::read_to_string(&file).ok().map(|c| c.trim_end().to_string())
                != Some(list.trim_end().to_string())
            {
                if let Err(e) = allod_keys::write_file_with_mode(&file, &list, 0o644) {
                    errors.push(format!("tunnel: cannot write {}: {e}", file.display()));
                }
            }
        }
        let status: Option<TunnelStatus> = std::fs::read_to_string(dir.join("status.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        let handshakes: BTreeMap<String, i64> = status
            .map(|s| s.peers.into_iter().map(|p| (p.public_key, p.latest_handshake)).collect())
            .unwrap_or_default();
        let t = now();
        let mut waiting = false;
        for g in gateways {
            if ["ordering", "expired", "error"].contains(&g.state.as_str()) {
                continue;
            }
            let Some(public) = self.gateway_key(g.id).ok().and_then(|k| public_key(&k)) else {
                continue;
            };
            let last = handshakes.get(&public).copied().filter(|h| *h > 0).or(g.last_handshake_at);
            let state = match last {
                Some(h) if t - h <= ACTIVE_SECS => "active",
                Some(h) if t - h > OFFLINE_SECS => "offline",
                Some(_) => g.state.as_str(),
                None => "setup",
            };
            waiting |= state == "setup" && g.ipv4.is_some();
            if state != g.state {
                tracing::info!(gateway = g.id, label = %g.label, from = %g.state, to = state, "gateway tunnel");
                self.trigger();
            }
            if state != g.state || last != g.last_handshake_at {
                let _ = sqlx::query(
                    "UPDATE gateways SET state = $1, last_handshake_at = $2, checked_at = $3 WHERE id = $4",
                )
                .bind(state)
                .bind(last)
                .bind(t)
                .bind(g.id)
                .execute(&self.pool)
                .await;
            }
        }
        self.gateway_waiting.store(waiting, std::sync::atomic::Ordering::Relaxed);
    }

    /// The home end's WireGuard configuration: one address and one peer per gateway.
    fn home_config(&self, gateways: &[Gateway]) -> Result<String, DomainError> {
        let mut addresses = Vec::new();
        let mut peers = String::new();
        for g in gateways.iter().filter(|g| g.state != "expired") {
            let Some(ip) = &g.ipv4 else { continue };
            let public = public_key(&self.gateway_key(g.id)?)
                .ok_or_else(|| DomainError::Internal("bad gateway key".into()))?;
            let t = tunnel_of(&public);
            let (gw, home) = (t.gateway_ip, t.home_ip);
            addresses.push(format!("{home}/24"));
            peers.push_str(&format!(
                "\n[Peer]\n# {label}\nPublicKey = {public}\nEndpoint = {ip}:{port}\n\
                 AllowedIPs = {gw}/32\nPersistentKeepalive = 25\n",
                label = g.label.replace('\n', " "),
                port = t.port,
            ));
        }
        Ok(format!(
            "# Written by Allod (SPEC §12.7): the home end of the gateway tunnels.\n\
             [Interface]\nPrivateKey = {key}\nAddress = {addresses}\n{peers}",
            key = self.home_key()?,
            addresses = addresses.join(", "),
        ))
    }
}

/// The setup script (idempotent) for one gateway.
pub fn setup_script(
    g: &Gateway,
    t: &Tunnel,
    gateway_key: &str,
    home_pub: &str,
    domains: &[String],
) -> String {
    let (gw, home, ifname, id) = (&t.gateway_ip, &t.home_ip, &t.ifname, &t.id);
    let label = g.label.replace(['\n', '\''], " ");
    let mut s = format!(
        r#"#!/bin/sh
# Allod gateway "{label}" (SPEC §12.7): passes HTTPS through to the Allod node over
# WireGuard, without decrypting it. Run as root on Debian or Ubuntu; safe to run again.
set -eu
export DEBIAN_FRONTEND=noninteractive
[ "$(id -u)" = 0 ] || {{ echo "run as root (sudo sh)" >&2; exit 1; }}
need() {{ command -v "$1" >/dev/null 2>&1; }}
if ! need wg || ! need nft; then
  apt-get update -qq
  apt-get install -y -qq wireguard-tools nftables iproute2 >/dev/null
fi

umask 077
mkdir -p /etc/wireguard
# Older Allod versions used one interface named allod: drop it if it is this gateway's.
if [ -f /etc/wireguard/allod.conf ] && grep -qxF 'PrivateKey = {gateway_key}' /etc/wireguard/allod.conf; then
  systemctl disable --now wg-quick@allod >/dev/null 2>&1 || true
  rm -f /etc/wireguard/allod.conf
fi
# Their route sync (one per server) fetched from addresses no Allod uses any more.
if [ -f /etc/systemd/system/allod-gateway-sync.timer ]; then
  systemctl disable --now allod-gateway-sync.timer >/dev/null 2>&1 || true
  rm -f /etc/systemd/system/allod-gateway-sync.timer /etc/systemd/system/allod-gateway-sync.service \
    /usr/local/sbin/allod-gateway-sync
fi
# This gateway's own interface, port and addresses: other Allod installations on this
# server have their own.
if ss -Hlun "sport = :{port}" | grep -q . && ! wg show {ifname} >/dev/null 2>&1; then
  echo "UDP port {port} is already in use here; this gateway needs it" >&2
  exit 1
fi
cat >/etc/wireguard/{ifname}.conf <<'EOF'
[Interface]
Address = {gw}/24
ListenPort = {port}
PrivateKey = {gateway_key}

[Peer]
PublicKey = {home_pub}
AllowedIPs = {home}/32
EOF
systemctl enable wg-quick@{ifname} >/dev/null 2>&1
systemctl restart wg-quick@{ifname}

# Firewalls: let the tunnel in.
if need ufw && ufw status 2>/dev/null | grep -q "Status: active"; then
  ufw allow {port}/udp >/dev/null
fi
"#,
        port = t.port,
    );
    if g.mode == "shared" {
        s.push_str(&format!(
            r#"
# Shared server: port 443 also serves other sites, so it is routed by name with
# allod-relay (SPEC §12.7.1).
cat >/usr/local/sbin/allod-relay <<'ALLOD_RELAY_EOF'
{relay}ALLOD_RELAY_EOF
chmod 755 /usr/local/sbin/allod-relay
[ -e /etc/nginx/allod-relay/stream-443.conf ] || /usr/local/sbin/allod-relay install --default none
WANT="{domains}"
for d in $WANT; do /usr/local/sbin/allod-relay route add "$d" {home}:443; done
# Routes to this node for domains it no longer serves.
/usr/local/sbin/allod-relay route list | while read -r d t; do
  [ "$t" = "{home}:443" ] || continue
  case " $WANT " in *" $d "*) ;; *) /usr/local/sbin/allod-relay route remove "$d" ;; esac
done
# Later changes: every minute, the domains this gateway serves are fetched from the
# node over the tunnel and the routes follow (nothing to run here again).
need curl || apt-get install -y -qq curl >/dev/null
cat >/usr/local/sbin/allod-gateway-sync-{id} <<'SYNC_EOF'
#!/bin/sh
# Keeps the routes of the domains Allod serves through this gateway (SPEC §12.7).
set -u
want=$(curl -fsS -m 10 http://{home}:{routes_port}/{id}) || exit 0  # tunnel down: keep them
current=$(/usr/local/sbin/allod-relay route list)
for d in $want; do
  printf '%s\n' "$current" | grep -qx "$d {home}:443" || /usr/local/sbin/allod-relay route add "$d" {home}:443
done
printf '%s\n' "$current" | while read -r d t; do
  [ "$t" = "{home}:443" ] || continue
  case " $(echo $want) " in *" $d "*) ;; *) /usr/local/sbin/allod-relay route remove "$d" ;; esac
done
SYNC_EOF
chmod 755 /usr/local/sbin/allod-gateway-sync-{id}
cat >/etc/systemd/system/allod-gateway-sync-{id}.service <<'SYNC_EOF'
[Unit]
Description=Allod gateway {id}: follow the domains the node serves here
After=wg-quick@{ifname}.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/allod-gateway-sync-{id}
SYNC_EOF
cat >/etc/systemd/system/allod-gateway-sync-{id}.timer <<'SYNC_EOF'
[Unit]
Description=Allod gateway {id}: follow the domains the node serves here, every minute

[Timer]
OnBootSec=1min
OnUnitActiveSec=1min

[Install]
WantedBy=timers.target
SYNC_EOF
systemctl daemon-reload
systemctl enable --now allod-gateway-sync-{id}.timer >/dev/null 2>&1
echo "Allod gateway ready (shared): $WANT"
echo "Domains added or removed in Allod are picked up within a minute."
"#,
            relay = ALLOD_RELAY,
            domains = domains.join(" "),
            routes_port = ROUTES_PORT,
        ));
    } else {
        s.push_str(&format!(
            r#"
# Dedicated server: all of port 443 goes to the node.
cat >/usr/local/sbin/allod-gateway-up-{id} <<'EOF'
#!/bin/sh
set -e
sysctl -qw net.ipv4.ip_forward=1
nft delete table ip allod_{id} 2>/dev/null || true
nft -f - <<'NFT'
table ip allod_{id} {{
  chain prerouting {{
    type nat hook prerouting priority dstnat; policy accept;
    iifname != "{ifname}" tcp dport 443 dnat to {home}:443
  }}
  chain postrouting {{
    type nat hook postrouting priority srcnat; policy accept;
    oifname "{ifname}" ip daddr {home} masquerade
  }}
}}
NFT
# Docker or ufw may drop forwarded traffic: allow this flow.
if command -v iptables >/dev/null 2>&1; then
  iptables -C FORWARD -o {ifname} -d {home} -p tcp --dport 443 -j ACCEPT 2>/dev/null ||
    iptables -I FORWARD -o {ifname} -d {home} -p tcp --dport 443 -j ACCEPT
  iptables -C FORWARD -i {ifname} -s {home} -p tcp --sport 443 -j ACCEPT 2>/dev/null ||
    iptables -I FORWARD -i {ifname} -s {home} -p tcp --sport 443 -j ACCEPT
fi
EOF
chmod 755 /usr/local/sbin/allod-gateway-up-{id}
cat >/etc/systemd/system/allod-gateway-{id}.service <<'EOF'
[Unit]
Description=Allod gateway: pass port 443 to the node
After=network-online.target wg-quick@{ifname}.service
Wants=wg-quick@{ifname}.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/sbin/allod-gateway-up-{id}

[Install]
WantedBy=multi-user.target
EOF
echo 'net.ipv4.ip_forward = 1' >/etc/sysctl.d/99-allod-gateway.conf
systemctl daemon-reload
systemctl enable allod-gateway-{id} >/dev/null 2>&1
systemctl restart allod-gateway-{id}
if need ufw && ufw status 2>/dev/null | grep -q "Status: active"; then
  ufw allow 443/tcp >/dev/null
  ufw route allow proto tcp to {home} port 443 >/dev/null
fi
echo "Allod gateway ready (dedicated): port 443 -> {home}"
"#,
        ));
    }
    s.push_str(&lightning_section(t));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gw(mode: &str) -> Gateway {
        Gateway {
            id: 1,
            label: "Dublin".into(),
            provider: "manual".into(),
            ipv4: Some("203.0.113.10".into()),
            ipv6: None,
            mode: mode.into(),
            tunnel_index: 3,
            state: "setup".into(),
            provider_ref: None,
            provider_data: None,
            expires_at: None,
            auto_renew: false,
            renew_max_sats: None,
            last_handshake_at: None,
            checked_at: None,
            error: None,
            created_at: 0,
        }
    }

    #[test]
    fn tunnels_differ_by_key_and_avoid_51820() {
        let a = tunnel_of("key-a");
        let b = tunnel_of("key-b");
        assert_ne!((a.port, &a.home_ip, &a.id), (b.port, &b.home_ip, &b.id));
        assert_eq!(a, tunnel_of("key-a"), "stable");
        for t in [&a, &b] {
            assert!((51821..55821).contains(&t.port));
            assert!(t.ifname.len() <= 15 && t.ifname.starts_with("allod-"));
            assert!(t.home_ip.starts_with("10.") && t.home_ip.ends_with(".2"));
        }
    }

    #[test]
    fn keys() {
        let k = new_private_key();
        let p = public_key(&k).unwrap();
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&p).unwrap().len(), 32);
        assert_eq!(public_key(&k), Some(p));
        assert_eq!(public_key("nope"), None);
    }

    #[test]
    fn scripts() {
        let t = tunnel_of("GWPUB");
        let (gw_ip, home) = (&t.gateway_ip, &t.home_ip);
        let d = setup_script(&gw("dedicated"), &t, "GWKEY", "HOMEPUB", &["pay.example.com".into()]);
        assert!(d.contains(&format!("Address = {gw_ip}/24")));
        assert!(d.contains(&format!("ListenPort = {}", t.port)));
        assert!(d.contains("PrivateKey = GWKEY"));
        assert!(d.contains("PublicKey = HOMEPUB"));
        assert!(d.contains(&format!("AllowedIPs = {home}/32")));
        assert!(d.contains(&format!("dnat to {home}:443")));
        assert!(d.contains(&format!("/etc/wireguard/{}.conf", t.ifname)));
        assert!(d.contains(&format!("table ip allod_{}", t.id)));
        assert!(!d.contains("allod-relay route add"));
        let s = setup_script(
            &gw("shared"),
            &t,
            "GWKEY",
            "HOMEPUB",
            &["a.example.com".into(), "b.example.com".into()],
        );
        assert!(s.contains(r#"WANT="a.example.com b.example.com""#));
        assert!(s.contains(&format!("route add \"$d\" {home}:443")));
        assert!(s.contains("ALLOD_RELAY_EOF"));
        assert!(!s.contains("tcp dport 443 dnat to"), "shared: 443 is routed by name");
        // Later domain changes come over the tunnel every minute.
        assert!(s.contains(&format!("curl -fsS -m 10 http://{home}:8089/{}", t.id)), "{s}");
        assert!(s.contains(&format!("allod-gateway-sync-{}.timer", t.id)));
        // Both follow the Lightning port Allod asks for, over the tunnel.
        for script in [&d, &s] {
            assert!(script.contains(&format!("http://{home}:8089/{}.lightning", t.id)), "{script}");
            assert!(script.contains(&format!("dnat to {home}:9735")));
            assert!(script.contains(&format!("allod-gateway-lightning-{}.timer", t.id)));
            assert!(script.contains(&format!("After=wg-quick@{}.service", t.ifname)));
            assert!(!script.contains("@ID@") && !script.contains("@HOME@"));
        }
        // Both scripts are valid shell.
        for script in [&d, &s] {
            let f = std::env::temp_dir().join(format!("allod-gw-{}.sh", rand::random::<u64>()));
            std::fs::write(&f, script).unwrap();
            let ok = std::process::Command::new("sh").arg("-n").arg(&f).status().unwrap().success();
            let _ = std::fs::remove_file(&f);
            assert!(ok, "sh -n failed:\n{script}");
        }
    }
}
