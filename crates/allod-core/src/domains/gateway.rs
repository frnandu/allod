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
/// UDP port gateways listen on.
pub const WG_PORT: u16 = 51820;
/// A handshake this recent means the tunnel is up.
const ACTIVE_SECS: i64 = 180;
/// No handshake for this long: offline.
const OFFLINE_SECS: i64 = 300;
/// `allod-relay` (SPEC §12.7.1), embedded in setup scripts for shared servers.
const ALLOD_RELAY: &str = include_str!("../../../../deploy/relay/allod-relay");

fn gateway_secret(id: i64) -> String {
    format!("gateway_{id}_wg")
}

/// Tunnel addresses of gateway `n`: (gateway, home).
pub fn tunnel_ips(n: i64) -> (String, String) {
    (format!("10.89.{n}.1"), format!("10.89.{n}.2"))
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
pub fn view(g: &Gateway, domains: &[Domain]) -> Value {
    let (gw_ip, home_ip) = tunnel_ips(g.tunnel_index);
    json!({
        "id": g.id,
        "label": g.label,
        "provider": g.provider,
        "ipv4": g.ipv4,
        "ipv6": g.ipv6,
        "mode": g.mode,
        "state": g.state,
        "tunnel": {"gateway": gw_ip, "home": home_ip, "port": WG_PORT},
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
        self.gateway_key(id)?;
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
        Ok(setup_script(&g, &self.gateway_key(id)?, &home_pub, &domains))
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
        if std::fs::read_to_string(&path).ok().as_deref() != Some(conf.as_str()) {
            if let Err(e) = allod_keys::write_file_with_mode(&path, &conf, 0o600) {
                errors.push(format!("tunnel: cannot write {}: {e}", path.display()));
            }
        }
        let status: Option<TunnelStatus> = std::fs::read_to_string(dir.join("status.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        let handshakes: BTreeMap<String, i64> = status
            .map(|s| s.peers.into_iter().map(|p| (p.public_key, p.latest_handshake)).collect())
            .unwrap_or_default();
        let t = now();
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
    }

    /// The home end's WireGuard configuration: one address and one peer per gateway.
    fn home_config(&self, gateways: &[Gateway]) -> Result<String, DomainError> {
        let mut addresses = Vec::new();
        let mut peers = String::new();
        for g in gateways.iter().filter(|g| g.state != "expired") {
            let Some(ip) = &g.ipv4 else { continue };
            let (gw, home) = tunnel_ips(g.tunnel_index);
            let public = public_key(&self.gateway_key(g.id)?)
                .ok_or_else(|| DomainError::Internal("bad gateway key".into()))?;
            addresses.push(format!("{home}/24"));
            peers.push_str(&format!(
                "\n[Peer]\n# {label}\nPublicKey = {public}\nEndpoint = {ip}:{WG_PORT}\n\
                 AllowedIPs = {gw}/32\nPersistentKeepalive = 25\n",
                label = g.label.replace('\n', " "),
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
pub fn setup_script(g: &Gateway, gateway_key: &str, home_pub: &str, domains: &[String]) -> String {
    let (gw, home) = tunnel_ips(g.tunnel_index);
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
cat >/etc/wireguard/allod.conf <<'EOF'
[Interface]
Address = {gw}/24
ListenPort = {port}
PrivateKey = {gateway_key}

[Peer]
PublicKey = {home_pub}
AllowedIPs = {home}/32
EOF
systemctl enable wg-quick@allod >/dev/null 2>&1
systemctl restart wg-quick@allod

# Firewalls: let the tunnel in.
if need ufw && ufw status 2>/dev/null | grep -q "Status: active"; then
  ufw allow {port}/udp >/dev/null
fi
"#,
        port = WG_PORT,
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
echo "Allod gateway ready (shared): $WANT"
"#,
            relay = ALLOD_RELAY,
            domains = domains.join(" "),
        ));
    } else {
        s.push_str(&format!(
            r#"
# Dedicated server: all of port 443 goes to the node.
cat >/usr/local/sbin/allod-gateway-up <<'EOF'
#!/bin/sh
set -e
sysctl -qw net.ipv4.ip_forward=1
nft delete table ip allod 2>/dev/null || true
nft -f - <<'NFT'
table ip allod {{
  chain prerouting {{
    type nat hook prerouting priority dstnat; policy accept;
    iifname != "allod" tcp dport 443 dnat to {home}:443
  }}
  chain postrouting {{
    type nat hook postrouting priority srcnat; policy accept;
    oifname "allod" ip daddr {home} masquerade
  }}
}}
NFT
# Docker or ufw may drop forwarded traffic: allow this flow.
if command -v iptables >/dev/null 2>&1; then
  iptables -C FORWARD -o allod -d {home} -p tcp --dport 443 -j ACCEPT 2>/dev/null ||
    iptables -I FORWARD -o allod -d {home} -p tcp --dport 443 -j ACCEPT
  iptables -C FORWARD -i allod -s {home} -p tcp --sport 443 -j ACCEPT 2>/dev/null ||
    iptables -I FORWARD -i allod -s {home} -p tcp --sport 443 -j ACCEPT
fi
EOF
chmod 755 /usr/local/sbin/allod-gateway-up
cat >/etc/systemd/system/allod-gateway.service <<'EOF'
[Unit]
Description=Allod gateway: pass port 443 to the node
After=network-online.target wg-quick@allod.service
Wants=wg-quick@allod.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/sbin/allod-gateway-up

[Install]
WantedBy=multi-user.target
EOF
echo 'net.ipv4.ip_forward = 1' >/etc/sysctl.d/99-allod-gateway.conf
systemctl daemon-reload
systemctl enable allod-gateway >/dev/null 2>&1
systemctl restart allod-gateway
if need ufw && ufw status 2>/dev/null | grep -q "Status: active"; then
  ufw allow 443/tcp >/dev/null
  ufw route allow proto tcp to {home} port 443 >/dev/null
fi
echo "Allod gateway ready (dedicated): port 443 -> {home}"
"#,
        ));
    }
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
    fn keys() {
        let k = new_private_key();
        let p = public_key(&k).unwrap();
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&p).unwrap().len(), 32);
        assert_eq!(public_key(&k), Some(p));
        assert_eq!(public_key("nope"), None);
    }

    #[test]
    fn scripts() {
        let d = setup_script(&gw("dedicated"), "GWKEY", "HOMEPUB", &["pay.example.com".into()]);
        assert!(d.contains("Address = 10.89.3.1/24"));
        assert!(d.contains("PrivateKey = GWKEY"));
        assert!(d.contains("PublicKey = HOMEPUB"));
        assert!(d.contains("AllowedIPs = 10.89.3.2/32"));
        assert!(d.contains("dnat to 10.89.3.2:443"));
        assert!(!d.contains("allod-relay route add"));
        let s = setup_script(
            &gw("shared"),
            "GWKEY",
            "HOMEPUB",
            &["a.example.com".into(), "b.example.com".into()],
        );
        assert!(s.contains(r#"WANT="a.example.com b.example.com""#));
        assert!(s.contains("route add \"$d\" 10.89.3.2:443"));
        assert!(s.contains("ALLOD_RELAY_EOF"));
        assert!(!s.contains("dnat to"));
    }
}
