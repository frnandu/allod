//! Owner authentication (SPEC §11.1): Nostr (NIP-98) login, in-memory JWT sessions,
//! and hashed API tokens.

pub mod nip98;

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::HeaderMap;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::AppState;

/// Session lifetime.
pub const SESSION_SECS: i64 = 12 * 3600;
/// Challenge lifetime.
pub const CHALLENGE_SECS: i64 = 120;
const MAX_CHALLENGES: usize = 1024;
const PUBLIC_WINDOW: Duration = Duration::from_secs(60);
const MAX_PUBLIC_SOURCES: usize = 4096;
/// Prefix of API tokens.
pub const API_TOKEN_PREFIX: &str = "allod_";

pub fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// SHA-256 of an API token, hex. Only this is stored.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Generates a new API token (`allod_` + 64 hex chars).
pub fn new_api_token() -> String {
    format!("{API_TOKEN_PREFIX}{}", random_hex(32))
}

/// What a credential may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Everything.
    Full,
    /// GET routes only.
    Readonly,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Full => "full",
            Permission::Readonly => "readonly",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "full" => Some(Permission::Full),
            "readonly" => Some(Permission::Readonly),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    perm: Permission,
    iat: i64,
    exp: i64,
    jti: String,
}

/// In-memory auth state. The signing key is created at startup and never stored,
/// so sessions end when the process restarts (SPEC §4.3).
pub struct AuthState {
    encoding: EncodingKey,
    decoding: DecodingKey,
    challenges: Mutex<HashMap<String, i64>>,
    revoked: Mutex<HashSet<String>>,
    tickets: Mutex<HashMap<String, i64>>,
    public_limits: Mutex<PublicLimits>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum PublicEndpoint {
    Challenge,
    Login,
    Setup,
}

impl PublicEndpoint {
    fn limits(self) -> (u32, u32) {
        match self {
            Self::Challenge | Self::Login => (30, 300),
            Self::Setup => (10, 100),
        }
    }
}

#[derive(Default)]
struct PublicLimits {
    global: HashMap<PublicEndpoint, (Instant, u32)>,
    sources: HashMap<(PublicEndpoint, IpAddr), (Instant, u32)>,
}

impl PublicLimits {
    fn take(&mut self, endpoint: PublicEndpoint, source: IpAddr, now: Instant) -> bool {
        self.sources.retain(|_, (start, _)| now.duration_since(*start) < PUBLIC_WINDOW);
        let (source_max, global_max) = endpoint.limits();
        let global = self.global.entry(endpoint).or_insert((now, 0));
        if now.duration_since(global.0) >= PUBLIC_WINDOW {
            *global = (now, 0);
        }
        let key = (endpoint, source);
        if !self.sources.contains_key(&key) && self.sources.len() >= MAX_PUBLIC_SOURCES {
            return false;
        }
        let local = self.sources.entry(key).or_insert((now, 0));
        if global.1 >= global_max || local.1 >= source_max {
            return false;
        }
        global.1 += 1;
        local.1 += 1;
        true
    }
}

impl Default for AuthState {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthState {
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        let state = AuthState {
            encoding: EncodingKey::from_secret(&key),
            decoding: DecodingKey::from_secret(&key),
            challenges: Mutex::new(HashMap::new()),
            revoked: Mutex::new(HashSet::new()),
            tickets: Mutex::new(HashMap::new()),
            public_limits: Mutex::new(PublicLimits::default()),
        };
        key.fill(0);
        state
    }

    pub fn allow_public(&self, endpoint: PublicEndpoint, source: IpAddr) -> bool {
        self.public_limits.lock().expect("public limit lock").take(endpoint, source, Instant::now())
    }

    /// Creates a single-use login challenge.
    pub fn new_challenge(&self, now: i64) -> Result<String, ApiError> {
        let mut map = self.challenges.lock().expect("challenge lock");
        map.retain(|_, expires| *expires > now);
        if map.len() >= MAX_CHALLENGES {
            return Err(ApiError::too_many("too many pending login challenges"));
        }
        let challenge = random_hex(32);
        map.insert(challenge.clone(), now + CHALLENGE_SECS);
        Ok(challenge)
    }

    /// Consumes a challenge; true when it existed and had not expired.
    pub fn take_challenge(&self, challenge: &str, now: i64) -> bool {
        let mut map = self.challenges.lock().expect("challenge lock");
        matches!(map.remove(challenge), Some(expires) if expires > now)
    }

    /// A single-use, 60-second ticket for the SSE endpoint.
    pub fn new_ticket(&self, _subject: &str, now: i64) -> Result<String, ApiError> {
        let mut map = self.tickets.lock().expect("ticket lock");
        map.retain(|_, exp| *exp > now);
        if map.len() >= MAX_CHALLENGES {
            return Err(ApiError::too_many("too many pending event tickets"));
        }
        let t = random_hex(24);
        map.insert(t.clone(), now + 60);
        Ok(t)
    }

    /// Consumes an SSE ticket.
    pub fn take_ticket(&self, ticket: &str, now: i64) -> bool {
        matches!(self.tickets.lock().expect("ticket lock").remove(ticket), Some(exp) if exp > now)
    }

    /// Issues a session token for an owner.
    pub fn issue_session(&self, pubkey: &str, now: i64) -> Result<(String, i64), ApiError> {
        let exp = now + SESSION_SECS;
        let claims = Claims {
            sub: pubkey.into(),
            perm: Permission::Full,
            iat: now,
            exp,
            jti: random_hex(16),
        };
        let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| ApiError::internal(format!("cannot sign session: {e}")))?;
        Ok((token, exp))
    }

    fn verify_session(&self, token: &str) -> Option<Claims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 5;
        validation.set_required_spec_claims(&["exp", "sub"]);
        let claims =
            jsonwebtoken::decode::<Claims>(token, &self.decoding, &validation).ok()?.claims;
        if self.revoked.lock().expect("revoked lock").contains(&claims.jti) {
            return None;
        }
        Some(claims)
    }

    fn revoke(&self, jti: &str) {
        self.revoked.lock().expect("revoked lock").insert(jti.to_string());
    }
}

/// Who made the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// An owner logged in with Nostr.
    Owner { pubkey: String, jti: String, expires_at: i64 },
    /// An API token.
    Token { id: i64, name: String },
}

/// An authenticated request. Use [`Auth::require_full`] / [`Auth::require_owner`]
/// in handlers that need more than read access.
#[derive(Debug, Clone)]
pub struct Auth {
    pub subject: Subject,
    pub permission: Permission,
}

impl Auth {
    pub fn require_full(&self) -> Result<(), ApiError> {
        match self.permission {
            Permission::Full => Ok(()),
            Permission::Readonly => Err(ApiError::forbidden("this token is read-only")),
        }
    }

    /// Owner session required (API tokens cannot manage owners or tokens).
    pub fn require_owner(&self) -> Result<&str, ApiError> {
        match &self.subject {
            Subject::Owner { pubkey, .. } => Ok(pubkey),
            Subject::Token { .. } => Err(ApiError::forbidden("an owner session is required")),
        }
    }

    pub fn logout(&self, state: &AuthState) {
        if let Subject::Owner { jti, .. } = &self.subject {
            state.revoke(jti);
        }
    }
}

/// The session credential: `X-Allod-Session` (used when `Authorization` carries a
/// NIP-98 re-authentication), otherwise `Authorization: Bearer`.
fn credential(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("x-allod-session").and_then(|v| v.to_str().ok()) {
        return Some(v.trim().to_string());
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string())
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token =
            credential(&parts.headers).ok_or_else(|| ApiError::unauthorized("sign in required"))?;
        let now = unix_now();
        if token.starts_with(API_TOKEN_PREFIX) {
            let row = state
                .db
                .use_api_token(&hash_token(&token), now)
                .await?
                .ok_or_else(|| ApiError::unauthorized("invalid API token"))?;
            if row.expires_at.is_some_and(|e| e <= now) {
                return Err(ApiError::unauthorized("API token expired"));
            }
            let permission = Permission::parse(&row.permission).unwrap_or(Permission::Readonly);
            return Ok(Auth { subject: Subject::Token { id: row.id, name: row.name }, permission });
        }
        let claims = state
            .auth
            .verify_session(&token)
            .ok_or_else(|| ApiError::unauthorized("session expired or invalid"))?;
        // An owner removed since login loses access immediately.
        if !state.db.is_owner(&claims.sub).await? {
            return Err(ApiError::unauthorized("not an owner"));
        }
        Ok(Auth {
            subject: Subject::Owner { pubkey: claims.sub, jti: claims.jti, expires_at: claims.exp },
            permission: claims.perm,
        })
    }
}

/// Sensitive actions need a fresh NIP-98 signature over this exact request by the
/// logged-in owner, in addition to the session (SPEC §11.1).
pub fn reauthenticate(
    state: &AppState,
    auth: &Auth,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<(), ApiError> {
    let session_owner = auth.require_owner()?;
    let urls = expected_urls(state.public_url.as_deref(), headers, path);
    let authorization =
        headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    let signer =
        nip98::verify(authorization, &nip98::Expect { urls: &urls, method, body, now: unix_now() })
            .map_err(|e| ApiError::unauthorized(format!("re-authentication required: {e}")))?;
    if signer != session_owner {
        return Err(ApiError::forbidden("re-authentication must be signed by the logged-in owner"));
    }
    Ok(())
}

/// URLs a NIP-98 event may name for `path` on this server: `ALLOD_PUBLIC_URL` when
/// configured, otherwise the request's Host with http or https.
pub fn expected_urls(public_url: Option<&str>, headers: &HeaderMap, path: &str) -> Vec<String> {
    if let Some(base) = public_url {
        return vec![format!("{}{path}", base.trim_end_matches('/'))];
    }
    let Some(host) = headers.get(axum::http::header::HOST).and_then(|h| h.to_str().ok()) else {
        return Vec::new();
    };
    vec![format!("http://{host}{path}"), format!("https://{host}{path}")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn public_auth_has_source_and_global_limits() {
        let mut limits = PublicLimits::default();
        let now = Instant::now();
        let one = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for _ in 0..30 {
            assert!(limits.take(PublicEndpoint::Login, one, now));
        }
        assert!(!limits.take(PublicEndpoint::Login, one, now));
        for i in 0..270 {
            let ip = IpAddr::V4(Ipv4Addr::new(10, 0, (i / 256) as u8, (i % 256) as u8));
            assert!(limits.take(PublicEndpoint::Login, ip, now));
        }
        assert!(!limits.take(PublicEndpoint::Login, IpAddr::V4(Ipv4Addr::new(10, 1, 0, 1)), now));
        assert!(limits.take(PublicEndpoint::Login, one, now + PUBLIC_WINDOW));
    }

    #[test]
    fn challenges_are_single_use_and_expire() {
        let s = AuthState::new();
        let c = s.new_challenge(100).unwrap();
        assert!(s.take_challenge(&c, 101));
        assert!(!s.take_challenge(&c, 101));
        let old = s.new_challenge(100).unwrap();
        assert!(!s.take_challenge(&old, 100 + CHALLENGE_SECS));
    }

    #[test]
    fn sessions_verify_and_revoke() {
        let s = AuthState::new();
        let now = unix_now();
        let (token, exp) = s.issue_session("aa", now).unwrap();
        assert_eq!(exp, now + SESSION_SECS);
        let claims = s.verify_session(&token).unwrap();
        assert_eq!(claims.sub, "aa");
        s.revoke(&claims.jti);
        assert!(s.verify_session(&token).is_none());
        // Another process (different key) cannot verify it.
        assert!(AuthState::new().verify_session(&s.issue_session("aa", now).unwrap().0).is_none());
    }

    #[test]
    fn expired_sessions_are_rejected() {
        let s = AuthState::new();
        let (token, _) = s.issue_session("aa", unix_now() - SESSION_SECS - 60).unwrap();
        assert!(s.verify_session(&token).is_none());
    }

    #[test]
    fn api_tokens_have_prefix_and_hash() {
        let t = new_api_token();
        assert!(t.starts_with(API_TOKEN_PREFIX));
        assert_eq!(t.len(), API_TOKEN_PREFIX.len() + 64);
        assert_eq!(hash_token(&t).len(), 64);
        assert_ne!(hash_token(&t), hash_token(&new_api_token()));
    }
}
