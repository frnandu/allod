//! The DNS provider calls, by provider kind (Njalla or deSEC; SPEC §12.1).

use super::njalla::{self, DnsRecord};
use super::{store, DomainService};

/// The provider kinds Allod can keep records at.
pub const KINDS: [&str; 2] = ["njalla", "desec"];

/// The label of a provider kind.
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "desec" => "deSEC",
        _ => "Njalla",
    }
}

impl DomainService {
    /// The kind of a DNS provider (`njalla` when unknown: the only kind before deSEC).
    pub(super) async fn provider_kind(&self, id: i64) -> String {
        store::provider(&self.pool, id)
            .await
            .ok()
            .flatten()
            .map(|p| p.kind)
            .unwrap_or_else(|| "njalla".into())
    }

    /// Checks a token: the domains it can manage.
    pub(super) async fn dns_check(&self, kind: &str, token: &str) -> Result<Vec<String>, String> {
        match kind {
            "desec" => self.desec.check(token).await.map_err(|e| e.to_string()),
            _ => self.njalla.check(token).await.map_err(|e| e.to_string()),
        }
    }

    /// The provider's zone holding `name`, and `name` below it (`@`: the zone itself).
    pub(super) async fn dns_zone(
        &self,
        kind: &str,
        token: &str,
        name: &str,
    ) -> Result<(String, String), String> {
        match kind {
            "desec" => match self.desec.zone_of(token, name).await {
                Ok(z) => Ok(z),
                // Not in the account yet: create the registered domain there (deSEC signs
                // it at once); the owner then points the registrar at deSEC.
                Err(e) if e.to_string().contains("not a domain of this deSEC account") => {
                    let (zone, _) = njalla::split(name)
                        .ok_or_else(|| format!("{name} is not a registered domain"))?;
                    self.desec
                        .create_domain(token, &zone)
                        .await
                        .map_err(|c| format!("{e}; creating {zone} there failed: {c}"))?;
                    tracing::info!(%zone, "deSEC: domain created; delegate it at the registrar");
                    self.desec.zone_of(token, name).await.map_err(|e| e.to_string())
                }
                Err(e) => Err(e.to_string()),
            },
            _ => njalla::split(name).ok_or_else(|| format!("{name} is not a registered domain")),
        }
    }

    pub(super) async fn dns_list(
        &self,
        kind: &str,
        token: &str,
        zone: &str,
    ) -> Result<Vec<DnsRecord>, String> {
        match kind {
            "desec" => self.desec.list(token, zone).await.map_err(|e| e.to_string()),
            _ => self.njalla.list(token, zone).await.map_err(|e| e.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn dns_add(
        &self,
        kind: &str,
        token: &str,
        zone: &str,
        rtype: &str,
        name: &str,
        content: &str,
        ttl: u32,
    ) -> Result<String, String> {
        match kind {
            "desec" => self
                .desec
                .add(token, zone, rtype, name, content, ttl)
                .await
                .map_err(|e| e.to_string()),
            _ => self
                .njalla
                .add(token, zone, rtype, name, content, ttl)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    pub(super) async fn dns_remove(
        &self,
        kind: &str,
        token: &str,
        zone: &str,
        id: &str,
    ) -> Result<(), String> {
        match kind {
            "desec" => self.desec.remove(token, zone, id).await.map_err(|e| e.to_string()),
            _ => self.njalla.remove(token, zone, id).await.map_err(|e| e.to_string()),
        }
    }

    /// The zone's DS values for the registrar, when the provider tells them (deSEC).
    pub(super) async fn dns_ds(&self, kind: &str, token: &str, zone: &str) -> Vec<String> {
        match kind {
            "desec" => self.desec.ds(token, zone).await.unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}
