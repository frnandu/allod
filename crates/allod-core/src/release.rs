//! Finding and downloading Allod releases published on Nostr, for the VPS installer.
//!
//! Releases are NIP-82 style events made by `ngit release publish`:
//! - kind 30063, the release: tags `a` (the application, `32267:<pubkey>:<app>`),
//!   `version`, `c` (channel: `main`, `nightly`, ...) and one `e` per asset event;
//! - kind 3063, one per file: `filename`, `x` (sha256), `url`, `size`;
//! - kind 10063, the publisher's Blossom server list (BUD-03).
//!
//! Relays are untrusted. Every event must be signed by the pinned publisher key, and
//! every file must hash to the `x` of its signed asset event. Files live on Blossom
//! servers, which serve any blob at `/<sha256>`, so any server holding a copy will do:
//! the asset's own URL first, then the publisher's servers, then the caller's fallbacks.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use nostr::prelude::*;
use nostr_sdk::prelude::Client;
use sha2::{Digest, Sha256};

/// Release event (NIP-82 software release).
pub const KIND_RELEASE: u16 = 30063;
/// Release asset event (NIP-82 software asset).
pub const KIND_ASSET: u16 = 3063;
/// Software application event (NIP-82), the release's `a` tag points at it.
pub const KIND_APP: u16 = 32267;
/// Blossom user server list (BUD-03).
pub const KIND_BLOSSOM_SERVERS: u16 = 10063;

/// The key that signs Allod releases (npub1md8jahxf2u9fh2mlufaa3kjhshn96wm8eqyck7y99fmyd76jwynq99sxe7).
pub const ALLOD_PUBLISHER: &str =
    "db4f2edcc9570a9bab7fe27bd8da5785e65d3b67c8098b78852a7646fb527126";
/// Application identifier of Allod's releases.
pub const ALLOD_APP: &str = "allod";

/// Relays the installer asks when the caller names none.
pub const DEFAULT_RELAYS: &[&str] =
    &["wss://relay.ditto.pub", "wss://relay.primal.net", "wss://relay.ngit.dev", "wss://nos.lol"];
/// Blossom servers tried after the publisher's own list.
pub const DEFAULT_BLOSSOM: &[&str] =
    &["https://blossom.primal.net", "https://blossom.yakihonne.com", "https://blossom.data.haus"];

const RELAY_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// File name, a plain base name (checked).
    pub filename: String,
    /// Lowercase hex sha256 the file must have.
    pub sha256: String,
    pub size: Option<u64>,
    /// URLs given in the asset event, in order.
    pub urls: Vec<String>,
}

/// A release chosen from the relays, with its files.
#[derive(Debug, Clone)]
pub struct Release {
    pub id: EventId,
    pub version: String,
    pub channel: String,
    pub commit: Option<String>,
    pub assets: Vec<Asset>,
}

/// What to look for.
#[derive(Debug, Clone)]
pub struct Query {
    pub publisher: PublicKey,
    /// Application identifier (the `d` of its kind 32267 event).
    pub app: String,
    pub channel: String,
    /// An exact version; `None` picks the newest.
    pub version: Option<String>,
}

impl Query {
    fn app_coordinate(&self) -> String {
        format!("{KIND_APP}:{}:{}", self.publisher.to_hex(), self.app)
    }
}

fn tag_values<'a>(event: &'a Event, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    event.tags.iter().filter_map(move |t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == name).then(|| s[1].as_str())
    })
}

fn first_tag<'a>(event: &'a Event, name: &'a str) -> Option<&'a str> {
    tag_values(event, name).next()
}

/// Events of `kind` by `publisher` with a valid signature; anything else from a relay is
/// dropped.
fn trusted<'a>(
    events: impl IntoIterator<Item = &'a Event>,
    publisher: &PublicKey,
    kind: u16,
) -> Vec<&'a Event> {
    events
        .into_iter()
        .filter(|e| e.pubkey == *publisher && e.kind == Kind::Custom(kind) && e.verify().is_ok())
        .collect()
}

/// Sort key for versions: the numbers in it, in order (`0.2.10` > `0.2.9`,
/// `nightly-20261116-1649` > `nightly-20261116-1600`).
fn version_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect()
}

/// The release event matching `q`, newest version first (ties: newest event).
pub fn select_release<'a>(events: &'a [Event], q: &Query) -> Option<&'a Event> {
    let app = q.app_coordinate();
    trusted(events, &q.publisher, KIND_RELEASE)
        .into_iter()
        .filter(|e| tag_values(e, "a").any(|a| a == app))
        .filter(|e| tag_values(e, "c").any(|c| c == q.channel))
        .filter(|e| first_tag(e, "version").is_some())
        .filter(|e| match &q.version {
            Some(v) => first_tag(e, "version") == Some(v.as_str()),
            None => true,
        })
        .max_by(|a, b| {
            let (va, vb) =
                (first_tag(a, "version").unwrap_or(""), first_tag(b, "version").unwrap_or(""));
            version_key(va).cmp(&version_key(vb)).then(a.created_at.cmp(&b.created_at))
        })
}

/// Rejects names that could leave the output directory.
fn safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && !name.starts_with('.')
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The release's files: asset events it lists by id, signed by the publisher.
pub fn release_assets(release: &Event, events: &[Event], publisher: &PublicKey) -> Vec<Asset> {
    let listed: HashSet<&str> = tag_values(release, "e").collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in trusted(events, publisher, KIND_ASSET) {
        if !listed.contains(e.id.to_hex().as_str()) || !seen.insert(e.id) {
            continue;
        }
        let (Some(filename), Some(sha256)) = (first_tag(e, "filename"), first_tag(e, "x")) else {
            continue;
        };
        let sha256 = sha256.to_ascii_lowercase();
        if !safe_filename(filename) || !is_sha256_hex(&sha256) {
            continue;
        }
        out.push(Asset {
            filename: filename.to_string(),
            sha256,
            size: first_tag(e, "size").and_then(|s| s.parse().ok()),
            urls: tag_values(e, "url")
                .filter(|u| u.starts_with("https://"))
                .map(String::from)
                .collect(),
        });
    }
    out.sort_by(|a, b| a.filename.cmp(&b.filename));
    out
}

/// Blossom servers from the publisher's newest signed server list.
pub fn blossom_servers(events: &[Event], publisher: &PublicKey) -> Vec<String> {
    trusted(events, publisher, KIND_BLOSSOM_SERVERS)
        .into_iter()
        .max_by_key(|e| e.created_at)
        .map(|e| {
            tag_values(e, "server")
                .filter(|s| s.starts_with("https://"))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Where to look for an asset, in order, without repeats: its own URLs, then
/// `<server>/<sha256>` for each server.
pub fn candidate_urls(asset: &Asset, servers: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    asset
        .urls
        .iter()
        .cloned()
        .chain(servers.iter().map(|s| format!("{}/{}", s.trim_end_matches('/'), asset.sha256)))
        .filter(|u| seen.insert(u.clone()))
        .collect()
}

/// Whether `name` matches one of `patterns` (exact, or a prefix ending in `*`). No
/// patterns: everything matches.
pub fn wanted(name: &str, patterns: &[String]) -> bool {
    patterns.is_empty()
        || patterns.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == p,
        })
}

/// Reads the relays and picks the release. Returns it with the publisher's Blossom
/// servers.
pub async fn find(q: &Query, relays: &[String]) -> Result<(Release, Vec<String>), String> {
    let client = Client::default();
    for r in relays {
        if let Err(e) = client.add_relay(r.as_str()).await {
            tracing::warn!("relay {r}: {e}");
        }
    }
    client.connect().await;
    let _ = client.try_connect().timeout(Duration::from_secs(8)).await;
    let result = find_with(&client, q).await;
    client.disconnect().await;
    result
}

async fn fetch(client: &Client, filter: Filter) -> Result<Vec<Event>, String> {
    let events =
        client.fetch_events(filter).timeout(RELAY_TIMEOUT).await.map_err(|e| e.to_string())?;
    Ok(events.into_iter().collect())
}

async fn find_with(client: &Client, q: &Query) -> Result<(Release, Vec<String>), String> {
    let app = q.app_coordinate();
    let releases = fetch(
        client,
        Filter::new()
            .kind(Kind::Custom(KIND_RELEASE))
            .author(q.publisher)
            .custom_tag(SingleLetterTag::from_char('a').expect("a is a tag letter"), app.clone()),
    )
    .await
    .map_err(|e| format!("cannot read releases from the relays: {e}"))?;
    let release = select_release(&releases, q).ok_or_else(|| match &q.version {
        Some(v) => format!("no signed release {v} of {app} on channel {}", q.channel),
        None => format!("no signed release of {app} on channel {}", q.channel),
    })?;

    let ids: Vec<EventId> =
        tag_values(release, "e").filter_map(|id| EventId::from_hex(id).ok()).collect();
    let asset_events = if ids.is_empty() {
        Vec::new()
    } else {
        fetch(client, Filter::new().kind(Kind::Custom(KIND_ASSET)).ids(ids))
            .await
            .map_err(|e| format!("cannot read the release's files from the relays: {e}"))?
    };
    let assets = release_assets(release, &asset_events, &q.publisher);
    if assets.is_empty() {
        return Err("the release lists no signed files".into());
    }

    let lists =
        fetch(client, Filter::new().kind(Kind::Custom(KIND_BLOSSOM_SERVERS)).author(q.publisher))
            .await
            .unwrap_or_default();

    Ok((
        Release {
            id: release.id,
            version: first_tag(release, "version").unwrap_or_default().to_string(),
            channel: q.channel.clone(),
            commit: first_tag(release, "commit").map(String::from),
            assets,
        },
        blossom_servers(&lists, &q.publisher),
    ))
}

/// HTTP client for downloads.
pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(concat!("allod-release/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

/// Downloads `url` to `tmp`, returning the sha256 of what arrived. Stops early when the
/// body grows past the signed size.
async fn download_one(
    http: &reqwest::Client,
    url: &str,
    tmp: &Path,
    max: Option<u64>,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    let mut resp = http.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let mut file = tokio::fs::File::create(tmp).await.map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut len = 0u64;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        len += chunk.len() as u64;
        if max.is_some_and(|m| len > m) {
            return Err("larger than the signed size".into());
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    file.flush().await.map_err(|e| e.to_string())?;
    Ok(hex::encode(hasher.finalize()))
}

/// Downloads `asset` into `dir` from the first URL whose content has the signed hash.
/// Returns the file's path and the URL it came from.
pub async fn download(
    http: &reqwest::Client,
    asset: &Asset,
    servers: &[String],
    dir: &Path,
) -> Result<(PathBuf, String), String> {
    if !safe_filename(&asset.filename) {
        return Err(format!("refusing file name {:?}", asset.filename));
    }
    let dest = dir.join(&asset.filename);
    let tmp = dir.join(format!(".{}.part", asset.filename));
    let mut errors = Vec::new();
    for url in candidate_urls(asset, servers) {
        match download_one(http, &url, &tmp, asset.size).await {
            Ok(hash) if hash == asset.sha256 => {
                tokio::fs::rename(&tmp, &dest).await.map_err(|e| e.to_string())?;
                return Ok((dest, url));
            }
            Ok(hash) => errors.push(format!("{url}: wrong content (sha256 {hash})")),
            Err(e) => errors.push(format!("{url}: {e}")),
        }
    }
    let _ = tokio::fs::remove_file(&tmp).await;
    Err(format!("no server had {} ({}):\n  {}", asset.filename, asset.sha256, errors.join("\n  ")))
}

/// What `allod release fetch` does.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// Publisher key, hex or npub.
    pub publisher: String,
    pub app: String,
    pub channel: String,
    pub version: Option<String>,
    pub relays: Vec<String>,
    /// Blossom servers tried after the publisher's own list.
    pub blossom: Vec<String>,
    /// File names to download (exact, or a prefix ending in `*`); empty: all.
    pub files: Vec<String>,
    pub out: PathBuf,
}

/// Finds the release and downloads the wanted files into `out`, each checked against
/// its signed sha256. Progress goes to the log; returns the release.
pub async fn fetch_to_dir(o: &FetchOptions) -> Result<Release, String> {
    let publisher = PublicKey::parse(&o.publisher).map_err(|e| format!("publisher key: {e}"))?;
    let q = Query {
        publisher,
        app: o.app.clone(),
        channel: o.channel.clone(),
        version: o.version.clone(),
    };
    let (release, own) = find(&q, &o.relays).await?;
    tracing::info!("release {} ({}) on channel {}", release.version, release.id, release.channel);
    let picked: Vec<&Asset> =
        release.assets.iter().filter(|a| wanted(&a.filename, &o.files)).collect();
    for p in &o.files {
        if !release.assets.iter().any(|a| wanted(&a.filename, std::slice::from_ref(p))) {
            return Err(format!("release {} has no file matching {p}", release.version));
        }
    }
    let servers = merge_servers(&[&own, &o.blossom]);
    std::fs::create_dir_all(&o.out).map_err(|e| format!("{}: {e}", o.out.display()))?;
    let http = http_client()?;
    for asset in picked {
        let (path, url) = download(&http, asset, &servers, &o.out).await?;
        tracing::info!("{} from {url}", path.display());
    }
    Ok(release)
}

/// Merges server lists, keeping the first occurrence.
pub fn merge_servers(lists: &[&[String]]) -> Vec<String> {
    let mut seen = HashMap::new();
    let mut out = Vec::new();
    for s in lists.iter().flat_map(|l| l.iter()) {
        let key = s.trim_end_matches('/').to_string();
        if seen.insert(key.clone(), ()).is_none() {
            out.push(key);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(keys: &Keys) -> String {
        format!("{KIND_APP}:{}:allod", keys.public_key().to_hex())
    }

    fn release(keys: &Keys, version: &str, channel: &str, assets: &[&Event], ts: u64) -> Event {
        let mut tags = vec![
            Tag::parse(["a", &app(keys)]).unwrap(),
            Tag::parse(["version", version]).unwrap(),
            Tag::parse(["c", channel]).unwrap(),
            Tag::parse(["d", &format!("allod@{version}")]).unwrap(),
        ];
        for a in assets {
            tags.push(Tag::parse(["e", &a.id.to_hex()]).unwrap());
        }
        EventBuilder::new(Kind::Custom(KIND_RELEASE), "")
            .tags(tags)
            .custom_created_at(Timestamp::from(ts))
            .finalize(keys)
            .unwrap()
    }

    fn asset(keys: &Keys, name: &str, body: &[u8], url: &str) -> Event {
        let hash = hex::encode(Sha256::digest(body));
        EventBuilder::new(Kind::Custom(KIND_ASSET), "")
            .tags([
                Tag::parse(["filename", name]).unwrap(),
                Tag::parse(["x", &hash]).unwrap(),
                Tag::parse(["size", &body.len().to_string()]).unwrap(),
                Tag::parse(["url", url]).unwrap(),
            ])
            .finalize(keys)
            .unwrap()
    }

    fn query(keys: &Keys, channel: &str, version: Option<&str>) -> Query {
        Query {
            publisher: keys.public_key(),
            app: "allod".into(),
            channel: channel.into(),
            version: version.map(String::from),
        }
    }

    fn version_of(e: &Event) -> &str {
        first_tag(e, "version").unwrap()
    }

    #[test]
    fn picks_the_newest_signed_release_on_the_channel() {
        let k = Keys::generate();
        let events = vec![
            release(&k, "0.2.9", "main", &[], 10),
            release(&k, "0.2.10", "main", &[], 5),
            release(&k, "0.3.0", "nightly", &[], 20),
        ];
        assert_eq!(
            version_of(select_release(&events, &query(&k, "main", None)).unwrap()),
            "0.2.10"
        );
        assert_eq!(
            version_of(select_release(&events, &query(&k, "nightly", None)).unwrap()),
            "0.3.0"
        );
        assert_eq!(
            version_of(select_release(&events, &query(&k, "main", Some("0.2.9"))).unwrap()),
            "0.2.9"
        );
        assert!(select_release(&events, &query(&k, "main", Some("9.9.9"))).is_none());
    }

    #[test]
    fn ignores_other_keys_forged_signatures_and_other_apps() {
        let k = Keys::generate();
        let other = Keys::generate();
        let good = release(&k, "0.1.0", "main", &[], 1);
        // Someone else's "newer" release naming our app.
        let mut foreign = release(&other, "9.0.0", "main", &[], 2);
        foreign.tags = good.tags.clone();
        // Our release with its content changed after signing.
        let mut forged = release(&k, "8.0.0", "main", &[], 3);
        forged.content = "tampered".into();
        // Ours, but for another application.
        let other_app = EventBuilder::new(Kind::Custom(KIND_RELEASE), "")
            .tags([
                Tag::parse(["a", &format!("{KIND_APP}:{}:other", k.public_key().to_hex())])
                    .unwrap(),
                Tag::parse(["version", "7.0.0"]).unwrap(),
                Tag::parse(["c", "main"]).unwrap(),
            ])
            .finalize(&k)
            .unwrap();
        let events = vec![good, foreign, forged, other_app];
        assert_eq!(version_of(select_release(&events, &query(&k, "main", None)).unwrap()), "0.1.0");
    }

    #[test]
    fn nightly_names_sort_by_date() {
        assert!(version_key("nightly-20261116-1649") > version_key("nightly-20261116-1600"));
        assert!(version_key("0.2.10") > version_key("0.2.9"));
    }

    #[test]
    fn assets_come_only_from_listed_signed_events() {
        let k = Keys::generate();
        let other = Keys::generate();
        let bundle = asset(&k, "allod-stack-0.2.3.tar.gz", b"bundle", "https://a.example/x");
        let sums = asset(&k, "SHA256SUMS", b"sums", "https://a.example/y");
        let unlisted = asset(&k, "extra", b"extra", "https://a.example/z");
        let foreign = asset(&other, "evil", b"evil", "https://a.example/e");
        let sneaky = asset(&k, "../escape", b"x", "https://a.example/s");
        let rel = release(&k, "0.2.3", "main", &[&bundle, &sums, &foreign, &sneaky], 1);
        let got = release_assets(&rel, &[bundle, sums, unlisted, foreign, sneaky], &k.public_key());
        let names: Vec<_> = got.iter().map(|a| a.filename.as_str()).collect();
        assert_eq!(names, ["SHA256SUMS", "allod-stack-0.2.3.tar.gz"]);
        assert_eq!(got[1].sha256, hex::encode(Sha256::digest(b"bundle")));
        assert_eq!(got[1].size, Some(6));
    }

    #[test]
    fn candidates_are_the_asset_url_then_each_server() {
        let a = Asset {
            filename: "f".into(),
            sha256: "ab".repeat(32),
            size: None,
            urls: vec!["https://one.example/abab".into()],
        };
        let servers = merge_servers(&[
            &["https://one.example/".to_string(), "https://two.example".to_string()],
            &["https://two.example/".to_string()],
        ]);
        assert_eq!(
            candidate_urls(&a, &servers),
            [
                "https://one.example/abab".to_string(),
                format!("https://one.example/{}", "ab".repeat(32)),
                format!("https://two.example/{}", "ab".repeat(32)),
            ]
        );
    }

    #[test]
    fn file_patterns() {
        let p = vec!["allod-stack-*".to_string(), "SHA256SUMS".to_string()];
        assert!(wanted("allod-stack-0.2.3.tar.gz", &p));
        assert!(wanted("SHA256SUMS", &p));
        assert!(!wanted("install.sh", &p));
        assert!(wanted("anything", &[]));
    }

    #[tokio::test]
    async fn download_skips_servers_with_the_wrong_content() {
        use axum::{routing::get, Router};
        let body: &'static [u8] = b"the real bundle";
        let hash = hex::encode(Sha256::digest(body));
        let path = format!("/{hash}");
        let app = Router::new()
            .route(&format!("/bad{path}"), get(|| async { "something else" }))
            .route(&format!("/good{path}"), get(move || async move { body }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let asset = Asset {
            filename: "bundle.tar.gz".into(),
            sha256: hash,
            size: Some(body.len() as u64),
            urls: vec![],
        };
        let servers =
            vec![format!("{base}/missing"), format!("{base}/bad"), format!("{base}/good")];
        let dir = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        let (file, url) = download(&http, &asset, &servers, dir.path()).await.unwrap();
        assert_eq!(url, format!("{base}/good/{}", asset.sha256));
        assert_eq!(std::fs::read(file).unwrap(), body);

        // No server has it: an error naming each attempt, and no file left behind.
        let missing =
            Asset { filename: "x".into(), sha256: "00".repeat(32), size: None, urls: vec![] };
        let err = download(&http, &missing, &servers[..2], dir.path()).await.unwrap_err();
        assert!(err.contains("missing") && err.contains("bad"), "{err}");
        assert!(!dir.path().join("x").exists());
    }
}
