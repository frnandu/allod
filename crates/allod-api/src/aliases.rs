//! Node aliases for display, from the Lightning node's own network graph.
//!
//! ldk-server's `GraphGetNode` returns the alias a node put in its gossip
//! `node_announcement`; no third-party service is asked. Aliases change rarely, so
//! they are kept in memory per pubkey.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use allod_ln::LightningBackend;
use futures_util::stream::{self, StreamExt};
use tokio::sync::Mutex;

/// A known alias is reused this long.
const FOUND_TTL: Duration = Duration::from_secs(6 * 3600);
/// A node without an alias (private, or gossip not synced yet) is asked again after this.
const MISSING_TTL: Duration = Duration::from_secs(15 * 60);
/// Bound on cached pubkeys; the whole map is dropped when exceeded.
const MAX_ENTRIES: usize = 5000;
/// Parallel `GraphGetNode` calls when filling misses.
const PARALLEL: usize = 8;
/// BOLT 7 aliases are 32 bytes.
const MAX_CHARS: usize = 32;

#[derive(Default)]
pub struct AliasCache {
    map: Mutex<HashMap<String, (Instant, Option<String>)>>,
}

impl AliasCache {
    /// Aliases for `node_ids`; `None` for nodes without one. Backend errors are not
    /// cached, so a node that was unreachable is asked again on the next call.
    pub async fn resolve(
        &self,
        backend: &dyn LightningBackend,
        node_ids: impl IntoIterator<Item = String>,
    ) -> HashMap<String, Option<String>> {
        let mut out = HashMap::new();
        let mut misses = Vec::new();
        {
            let map = self.map.lock().await;
            for id in node_ids {
                if out.contains_key(&id) || misses.contains(&id) {
                    continue;
                }
                match map.get(&id) {
                    Some((at, alias)) if at.elapsed() < ttl(alias) => {
                        out.insert(id, alias.clone());
                    }
                    _ => misses.push(id),
                }
            }
        }
        if misses.is_empty() {
            return out;
        }
        let fetched: Vec<(String, Option<Option<String>>)> = stream::iter(misses)
            .map(|id| async move {
                let alias =
                    backend.node_alias(&id).await.ok().map(|a| a.and_then(|a| sanitize(&a)));
                (id, alias)
            })
            .buffer_unordered(PARALLEL)
            .collect()
            .await;
        let mut map = self.map.lock().await;
        if map.len() + fetched.len() > MAX_ENTRIES {
            map.clear();
        }
        let now = Instant::now();
        for (id, alias) in fetched {
            if let Some(alias) = &alias {
                map.insert(id.clone(), (now, alias.clone()));
            }
            out.insert(id, alias.flatten());
        }
        out
    }
}

fn ttl(alias: &Option<String>) -> Duration {
    if alias.is_some() {
        FOUND_TTL
    } else {
        MISSING_TTL
    }
}

/// Aliases are chosen by anyone: drop control and invisible formatting characters
/// (bidi overrides, zero-width), trim, and cap the length.
pub fn sanitize(raw: &str) -> Option<String> {
    let invisible = |c: char| matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
    let clean: String = raw.chars().filter(|c| !c.is_control() && !invisible(*c)).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return None;
    }
    Some(clean.chars().take(MAX_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_untrusted_aliases() {
        assert_eq!(sanitize("  ACINQ\u{0}\n ").as_deref(), Some("ACINQ"));
        assert_eq!(sanitize("evil\u{202E}gnp").as_deref(), Some("evilgnp"));
        assert_eq!(sanitize("\u{200B}\t"), None);
        assert_eq!(sanitize(&"x".repeat(50)).unwrap().len(), 32);
        assert_eq!(sanitize("Kraken 🐙⚡").as_deref(), Some("Kraken 🐙⚡"));
    }
}
