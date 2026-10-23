//! JWKS cache with a rate-limited, singleflight refresh on unknown `kid`.
//!
//! Invariants and what holds them:
//! - Only keys of the last successful JWKS are trusted: every successful fetch
//!   replaces the whole snapshot (`ArcSwap::store`), nothing is merged, so a key
//!   that vanished from Keycloak is gone from the next snapshot on.
//! - An unknown `kid` causes at most one fetch per window per issuer across all
//!   worker threads: the right to fetch is claimed with one compare-and-swap on
//!   the `gate`, which publishes the end of the window and the refresh in flight
//!   together. A token flood with random `kid` therefore cannot become a flood on
//!   Keycloak (SEC-03). Callers that lose the race do not fetch; they wait for the
//!   refresh already running (if any) and then answer from the resulting snapshot,
//!   so they see a freshly rotated key without a second fetch.
//! - The fetch runs in a detached task: a client that disconnects while it is the
//!   one that claimed the window cannot cancel the refresh for everybody else.
//! - A failed fetch keeps the previous snapshot. During a Keycloak outage tokens
//!   keep validating against the last good keys, and `exp`, `iss`, `aud` and the
//!   signature are still checked on every request by the validator.
//! - No lock is held across an `.await`: the snapshot and the gate are `ArcSwap`s.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::{HeaderMap, header};
use jsonwebtoken::{Algorithm, DecodingKey};
use serde::Deserialize;
use tokio::sync::watch;

use super::config::{MAX_JWKS_TTL, MIN_JWKS_TTL, OidcConfig};
use super::discovery::{Discovery, DiscoveryError};
use super::http::{HttpError, MAX_METADATA_BYTES, OidcHttp};
use crate::telemetry::{self, OidcOp, RequestResult};

/// Smallest accepted RSA modulus. Anything weaker is skipped, not trusted.
const MIN_RSA_BITS: usize = 2048;
/// Length of a P-256 coordinate in bytes.
const P256_COORDINATE_LEN: usize = 32;
/// More keys than this in one JWKS is not a realm, it is an attack on memory.
const MAX_KEYS: usize = 64;
const MAX_KID_LEN: usize = 256;

/// Why a JWKS fetch failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum JwksError {
    #[error("jwks request failed: {0}")]
    Http(#[from] HttpError),
    #[error("discovery is not available: {0}")]
    Discovery(#[from] DiscoveryError),
    #[error("discovery has not produced a jwks_uri yet")]
    NoDiscovery,
    #[error("jwks document is invalid: {0}")]
    Invalid(String),
    #[error("jwks contains no usable signing key")]
    NoUsableKeys,
}

impl JwksError {
    /// How this failure counts in `oidc_requests_total`.
    fn request_result(&self) -> RequestResult {
        match self {
            Self::Http(error) => error.request_result(),
            Self::Discovery(error) => error.request_result(),
            Self::NoDiscovery | Self::Invalid(_) | Self::NoUsableKeys => RequestResult::Error,
        }
    }
}

/// Why no verification key could be produced for a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyError {
    /// The `kid` is not in the last successful JWKS (and a refresh did not bring it).
    UnknownKid,
    /// There is no JWKS at all yet: Keycloak was never reachable.
    Unavailable,
}

/// A trusted verification key together with the only algorithm it may be used with.
#[derive(Debug)]
pub(crate) struct VerifyKey {
    pub(crate) alg: Algorithm,
    pub(crate) key: DecodingKey,
}

struct Snapshot {
    keys: HashMap<String, Arc<VerifyKey>>,
    /// `None` until the first successful fetch.
    fetched_at: Option<Instant>,
    ttl: Duration,
}

enum Claim {
    /// This caller fetches; the sender announces completion to everyone who waits.
    Won(watch::Sender<bool>),
    /// Someone else fetched recently or is fetching now.
    Throttled(Option<watch::Receiver<bool>>),
}

/// Counters for the metrics layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct JwksStats {
    pub(crate) refresh_ok: u64,
    pub(crate) refresh_err: u64,
    pub(crate) forced_started: u64,
    pub(crate) forced_throttled: u64,
    pub(crate) keys_skipped: u64,
}

/// When the next forced refresh may start and which refresh is running right now.
/// One value so that "window taken" and "refresh in flight" change atomically: a caller
/// that loses the claim always sees the flight of the caller that won it.
struct Gate {
    /// Earliest instant (ms since the cache was created) of the next forced refresh.
    next_forced_ms: u64,
    flight: Option<watch::Receiver<bool>>,
}

#[derive(Default)]
struct Counters {
    refresh_ok: AtomicU64,
    refresh_err: AtomicU64,
    forced_started: AtomicU64,
    forced_throttled: AtomicU64,
    keys_skipped: AtomicU64,
}

/// JWKS of one issuer.
pub(crate) struct JwksCache {
    discovery: Arc<Discovery>,
    http: OidcHttp,
    snapshot: ArcSwap<Snapshot>,
    window_ms: u64,
    default_ttl: Duration,
    /// Longest a waiter stays on a refresh that someone else is running.
    wait_cap: Duration,
    epoch: Instant,
    gate: ArcSwap<Gate>,
    counters: Counters,
}

impl JwksCache {
    pub(crate) fn new(config: &OidcConfig, http: OidcHttp, discovery: Arc<Discovery>) -> Arc<Self> {
        Arc::new(Self {
            discovery,
            http,
            snapshot: ArcSwap::from_pointee(Snapshot {
                keys: HashMap::new(),
                fetched_at: None,
                ttl: config.jwks_ttl,
            }),
            window_ms: millis(config.jwks_min_refresh_interval),
            default_ttl: config.jwks_ttl,
            // A fetch costs at most the request timeout; the extra second covers discovery
            // happening first inside the same claimed window.
            wait_cap: config.request_timeout * 2 + Duration::from_secs(1),
            epoch: Instant::now(),
            gate: ArcSwap::from_pointee(Gate {
                next_forced_ms: 0,
                flight: None,
            }),
            counters: Counters::default(),
        })
    }

    /// The verification key for `kid`, refreshing at most once per window when it is unknown.
    pub(crate) async fn key_for(self: &Arc<Self>, kid: &str) -> Result<Arc<VerifyKey>, KeyError> {
        if let Some(key) = self.snapshot.load().keys.get(kid) {
            return Ok(Arc::clone(key));
        }
        self.refresh_for_unknown_kid().await;
        let snapshot = self.snapshot.load();
        match snapshot.keys.get(kid) {
            Some(key) => Ok(Arc::clone(key)),
            None if snapshot.fetched_at.is_none() => Err(KeyError::Unavailable),
            None => Err(KeyError::UnknownKid),
        }
    }

    /// Fetches the JWKS now, outside the unknown-`kid` window rules. Used by the background
    /// loop, which runs at most once per TTL, and by tests.
    pub(crate) async fn refresh(&self) -> Result<(), JwksError> {
        let outcome = self.fetch_and_store().await;
        // Whoever fetched, the next forced refresh waits a full window: the keys are as fresh
        // as they can be, and Keycloak sees at most one JWKS request per window.
        let next = self.now_ms().saturating_add(self.window_ms);
        self.gate.rcu(|gate| {
            Arc::new(Gate {
                next_forced_ms: gate.next_forced_ms.max(next),
                flight: gate.flight.clone(),
            })
        });
        outcome
    }

    /// Age of the current key set, `None` before the first success.
    pub(crate) fn age(&self) -> Option<Duration> {
        self.snapshot.load().fetched_at.map(|at| at.elapsed())
    }

    /// The value of `oidc_jwks_age_seconds`: the age of the key set, or the time since this
    /// cache was created while no fetch has succeeded yet. A node that cannot reach Keycloak
    /// from its start must show a growing age too: with no series the stale alert could never
    /// fire for exactly the node that needs it.
    pub(crate) fn age_for_metrics(&self) -> Duration {
        self.age().unwrap_or_else(|| self.epoch.elapsed())
    }

    /// True when the key set is missing or older than its TTL.
    pub(crate) fn is_due(&self) -> bool {
        let snapshot = self.snapshot.load();
        snapshot
            .fetched_at
            .is_none_or(|at| at.elapsed() >= snapshot.ttl)
    }

    /// Time until the key set is due, zero when it already is.
    pub(crate) fn due_in(&self) -> Duration {
        let snapshot = self.snapshot.load();
        snapshot.fetched_at.map_or(Duration::ZERO, |at| {
            snapshot.ttl.saturating_sub(at.elapsed())
        })
    }

    pub(crate) fn key_count(&self) -> usize {
        self.snapshot.load().keys.len()
    }

    pub(crate) fn stats(&self) -> JwksStats {
        JwksStats {
            refresh_ok: self.counters.refresh_ok.load(Ordering::Relaxed),
            refresh_err: self.counters.refresh_err.load(Ordering::Relaxed),
            forced_started: self.counters.forced_started.load(Ordering::Relaxed),
            forced_throttled: self.counters.forced_throttled.load(Ordering::Relaxed),
            keys_skipped: self.counters.keys_skipped.load(Ordering::Relaxed),
        }
    }

    async fn refresh_for_unknown_kid(self: &Arc<Self>) {
        match self.claim() {
            Claim::Won(finished) => {
                self.counters.forced_started.fetch_add(1, Ordering::Relaxed);
                let mut done = finished.subscribe();
                self.spawn_refresh(finished);
                self.wait(&mut done).await;
            }
            Claim::Throttled(running) => {
                self.counters
                    .forced_throttled
                    .fetch_add(1, Ordering::Relaxed);
                if let Some(mut done) = running {
                    self.wait(&mut done).await;
                }
            }
        }
    }

    /// Takes the right to fetch: exactly one caller per window wins, and the same atomic
    /// step publishes its flight so that every loser can wait for it.
    fn claim(&self) -> Claim {
        let now = self.now_ms();
        let (finished, receiver) = watch::channel(false);
        let mut won = false;
        let previous = self.gate.rcu(|gate| {
            if now < gate.next_forced_ms {
                won = false;
                Arc::clone(gate)
            } else {
                won = true;
                Arc::new(Gate {
                    next_forced_ms: now.saturating_add(self.window_ms),
                    flight: Some(receiver.clone()),
                })
            }
        });
        if won {
            Claim::Won(finished)
        } else {
            Claim::Throttled(previous.flight.clone())
        }
    }

    fn spawn_refresh(self: &Arc<Self>, finished: watch::Sender<bool>) {
        let this = Arc::clone(self);
        // Detached on purpose: see the module docs. Errors are logged and counted in fetch_and_store.
        tokio::spawn(async move {
            let _ = this.fetch_and_store().await;
            // Leave the window as it is, drop only our own flight.
            this.gate.rcu(|gate| {
                let ours = gate
                    .flight
                    .as_ref()
                    .is_some_and(|flight| flight.same_channel(&finished.subscribe()));
                Arc::new(Gate {
                    next_forced_ms: gate.next_forced_ms,
                    flight: if ours { None } else { gate.flight.clone() },
                })
            });
            let _ = finished.send(true);
        });
    }

    async fn wait(&self, done: &mut watch::Receiver<bool>) {
        // A closed channel (the task died) or the cap both just mean "stop waiting":
        // the answer is then taken from the snapshot as it is.
        let _ = tokio::time::timeout(self.wait_cap, done.wait_for(|finished| *finished)).await;
    }

    async fn fetch_and_store(&self) -> Result<(), JwksError> {
        let outcome = self.fetch().await;
        self.discovery.report_contact(outcome.is_ok());
        telemetry::oidc_request(
            OidcOp::Jwks,
            outcome
                .as_ref()
                .map_or_else(JwksError::request_result, |_| RequestResult::Ok),
        );
        match outcome {
            Ok((parsed, ttl)) => {
                self.counters
                    .keys_skipped
                    .fetch_add(parsed.skipped, Ordering::Relaxed);
                self.snapshot.store(Arc::new(Snapshot {
                    keys: parsed.keys,
                    fetched_at: Some(Instant::now()),
                    ttl,
                }));
                self.counters.refresh_ok.fetch_add(1, Ordering::Relaxed);
                telemetry::oidc_jwks_age(0.0);
                Ok(())
            }
            Err(error) => {
                self.counters.refresh_err.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(%error, "oidc jwks refresh failed, keeping the previous key set");
                Err(error)
            }
        }
    }

    async fn fetch(&self) -> Result<(ParsedJwks, Duration), JwksError> {
        if self.discovery.current().is_none() {
            self.discovery.refresh().await?;
        }
        let provider = self.discovery.current().ok_or(JwksError::NoDiscovery)?;
        let fetched = self
            .http
            .get(provider.jwks_uri.as_str(), MAX_METADATA_BYTES)
            .await?;
        let ttl = cache_control_max_age(&fetched.headers).map_or(self.default_ttl, |max_age| {
            max_age.clamp(MIN_JWKS_TTL, MAX_JWKS_TTL)
        });
        Ok((parse_jwks(&fetched.body)?, ttl))
    }

    fn now_ms(&self) -> u64 {
        // +1 keeps "now" strictly above the initial `next_forced_ms` of 0.
        millis(self.epoch.elapsed()).saturating_add(1)
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// `max-age` of `Cache-Control`, `None` when absent, zero, or when the response forbids caching.
fn cache_control_max_age(headers: &HeaderMap) -> Option<Duration> {
    let mut max_age = None;
    for value in headers.get_all(header::CACHE_CONTROL) {
        let text = value.to_str().ok()?;
        for directive in text.split(',').map(|d| d.trim().to_ascii_lowercase()) {
            if directive == "no-store" || directive == "no-cache" {
                return None;
            }
            if let Some(seconds) = directive.strip_prefix("max-age=") {
                max_age = seconds
                    .trim_matches('"')
                    .parse::<u64>()
                    .ok()
                    .filter(|s| *s > 0);
            }
        }
    }
    max_age.map(Duration::from_secs)
}

struct ParsedJwks {
    keys: HashMap<String, Arc<VerifyKey>>,
    skipped: u64,
}

#[derive(Deserialize)]
struct JwksDocument {
    keys: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawJwk {
    kty: String,
    kid: Option<String>,
    #[serde(rename = "use")]
    key_use: Option<String>,
    alg: Option<String>,
    n: Option<String>,
    e: Option<String>,
    crv: Option<String>,
    x: Option<String>,
    y: Option<String>,
}

/// Builds the key table from a JWKS body. Only RS256 and ES256 verification keys survive;
/// everything else (encryption keys, `oct` keys, weak or odd keys, duplicate `kid`) is skipped.
fn parse_jwks(body: &[u8]) -> Result<ParsedJwks, JwksError> {
    let document: JwksDocument =
        serde_json::from_slice(body).map_err(|e| JwksError::Invalid(e.to_string()))?;
    if document.keys.len() > MAX_KEYS {
        return Err(JwksError::Invalid(format!("more than {MAX_KEYS} keys")));
    }
    let mut skipped = 0u64;
    let mut candidates: Vec<(String, VerifyKey)> = Vec::new();
    for value in document.keys {
        match serde_json::from_value::<RawJwk>(value)
            .ok()
            .and_then(|raw| build_key(&raw))
        {
            Some(candidate) => candidates.push(candidate),
            None => skipped += 1,
        }
    }
    // Two keys under one `kid` make the choice ambiguous: trust neither.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (kid, _) in &candidates {
        *counts.entry(kid.as_str()).or_default() += 1;
    }
    let duplicated: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(kid, _)| kid.to_owned())
        .collect();
    let mut keys = HashMap::new();
    for (kid, key) in candidates {
        if duplicated.contains(&kid) {
            skipped += 1;
        } else {
            keys.insert(kid, Arc::new(key));
        }
    }
    if keys.is_empty() {
        return Err(JwksError::NoUsableKeys);
    }
    Ok(ParsedJwks { keys, skipped })
}

fn build_key(raw: &RawJwk) -> Option<(String, VerifyKey)> {
    let kid = raw
        .kid
        .as_deref()
        .filter(|kid| !kid.is_empty() && kid.len() <= MAX_KID_LEN)?
        .to_owned();
    if raw.key_use.as_deref().is_some_and(|u| u != "sig") {
        return None;
    }
    let verify_key = match raw.kty.as_str() {
        "RSA" => rsa_key(raw)?,
        "EC" => ec_key(raw)?,
        _ => return None,
    };
    Some((kid, verify_key))
}

fn rsa_key(raw: &RawJwk) -> Option<VerifyKey> {
    if raw.alg.as_deref().is_some_and(|alg| alg != "RS256") {
        return None;
    }
    let (n, e) = (raw.n.as_deref()?, raw.e.as_deref()?);
    let modulus = URL_SAFE_NO_PAD.decode(n).ok()?;
    if bit_length(&modulus) < MIN_RSA_BITS {
        return None;
    }
    Some(VerifyKey {
        alg: Algorithm::RS256,
        key: DecodingKey::from_rsa_components(n, e).ok()?,
    })
}

fn ec_key(raw: &RawJwk) -> Option<VerifyKey> {
    if raw.crv.as_deref() != Some("P-256") || raw.alg.as_deref().is_some_and(|a| a != "ES256") {
        return None;
    }
    let (x, y) = (raw.x.as_deref()?, raw.y.as_deref()?);
    let lengths_ok = [x, y].iter().all(|c| {
        URL_SAFE_NO_PAD
            .decode(c)
            .is_ok_and(|b| b.len() == P256_COORDINATE_LEN)
    });
    if !lengths_ok {
        return None;
    }
    Some(VerifyKey {
        alg: Algorithm::ES256,
        key: DecodingKey::from_ec_components(x, y).ok()?,
    })
}

fn bit_length(big_endian: &[u8]) -> usize {
    let significant: &[u8] = match big_endian.iter().position(|b| *b != 0) {
        Some(first) => &big_endian[first..],
        None => return 0,
    };
    let top_bits = 8 - significant[0].leading_zeros() as usize;
    (significant.len() - 1) * 8 + top_bits
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    fn b64(bytes: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn rsa_jwk(kid: &str, modulus_bytes: usize) -> serde_json::Value {
        let mut n = vec![0xC5u8; modulus_bytes];
        n[modulus_bytes - 1] |= 1;
        serde_json::json!({"kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256",
            "n": b64(&n), "e": b64(&[1, 0, 1])})
    }

    fn ec_jwk(kid: &str) -> serde_json::Value {
        serde_json::json!({"kty": "EC", "kid": kid, "crv": "P-256",
            "x": b64(&[7u8; 32]), "y": b64(&[9u8; 32])})
    }

    fn body(keys: &[serde_json::Value]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "keys": keys })).unwrap()
    }

    #[test]
    fn keeps_only_rs256_and_es256_signing_keys() {
        let keys = [
            rsa_jwk("rsa", 256),
            ec_jwk("ec"),
            // Keycloak publishes an RSA-OAEP encryption key next to the signing key.
            serde_json::json!({"kty": "RSA", "kid": "enc", "use": "enc", "alg": "RSA-OAEP",
                "n": b64(&[0xC5; 256]), "e": b64(&[1, 0, 1])}),
            serde_json::json!({"kty": "oct", "kid": "hmac", "k": b64(b"secret")}),
            serde_json::json!({"kty": "OKP", "kid": "ed", "crv": "Ed25519", "x": b64(&[1; 32])}),
        ];
        let parsed = parse_jwks(&body(&keys)).unwrap();
        let mut kids: Vec<_> = parsed.keys.keys().cloned().collect();
        kids.sort();
        assert_eq!(kids, ["ec", "rsa"]);
        assert_eq!(parsed.skipped, 3);
        assert_eq!(parsed.keys["rsa"].alg, Algorithm::RS256);
        assert_eq!(parsed.keys["ec"].alg, Algorithm::ES256);
    }

    #[test]
    fn must_not_trust_weak_or_mislabelled_keys() {
        let mut rs384 = rsa_jwk("rs384", 256);
        rs384["alg"] = "RS384".into();
        let mut ps256 = rsa_jwk("ps256", 256);
        ps256["alg"] = "PS256".into();
        let mut p384 = ec_jwk("p384");
        p384["crv"] = "P-384".into();
        let mut es384 = ec_jwk("es384");
        es384["alg"] = "ES384".into();
        let mut short_coordinate = ec_jwk("short");
        short_coordinate["x"] = b64(&[1u8; 31]).into();
        let mut nokid = rsa_jwk("x", 256);
        nokid.as_object_mut().unwrap().remove("kid");
        let keys = [
            rsa_jwk("weak-1024", 128),
            rs384,
            ps256,
            p384,
            es384,
            short_coordinate,
            nokid,
            rsa_jwk("", 256),
            rsa_jwk(&"k".repeat(MAX_KID_LEN + 1), 256),
            rsa_jwk("ok", 256),
        ];
        let parsed = parse_jwks(&body(&keys)).unwrap();
        assert_eq!(parsed.keys.keys().collect::<Vec<_>>(), ["ok"]);
        assert_eq!(parsed.skipped, 9);
    }

    #[test]
    fn must_not_trust_ambiguous_duplicate_kid() {
        let keys = [rsa_jwk("dup", 256), ec_jwk("dup"), rsa_jwk("fine", 256)];
        let parsed = parse_jwks(&body(&keys)).unwrap();
        assert_eq!(parsed.keys.keys().collect::<Vec<_>>(), ["fine"]);
        assert_eq!(parsed.skipped, 2);
    }

    #[test]
    fn rejects_empty_garbage_and_oversized_documents() {
        assert!(matches!(
            parse_jwks(b"not json"),
            Err(JwksError::Invalid(_))
        ));
        assert!(matches!(parse_jwks(b"{}"), Err(JwksError::Invalid(_))));
        assert!(matches!(
            parse_jwks(&body(&[])),
            Err(JwksError::NoUsableKeys)
        ));
        let many: Vec<_> = (0..=MAX_KEYS).map(|i| ec_jwk(&format!("k{i}"))).collect();
        assert!(matches!(
            parse_jwks(&body(&many)),
            Err(JwksError::Invalid(_))
        ));
        let only_junk = [serde_json::json!({"kty": 5}), serde_json::json!("str")];
        assert!(matches!(
            parse_jwks(&body(&only_junk)),
            Err(JwksError::NoUsableKeys)
        ));
    }

    #[test]
    fn bit_length_counts_significant_bits() {
        assert_eq!(bit_length(&[]), 0);
        assert_eq!(bit_length(&[0, 0]), 0);
        assert_eq!(bit_length(&[1]), 1);
        assert_eq!(bit_length(&[0, 0x80]), 8);
        assert_eq!(bit_length(&[0x01, 0x00]), 9);
        assert_eq!(bit_length(&vec![0xFF; 256]), 2048);
    }

    fn headers(values: &[&'static str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for v in values {
            map.append(header::CACHE_CONTROL, HeaderValue::from_static(v));
        }
        map
    }

    #[test]
    fn cache_control_max_age_rules() {
        assert_eq!(
            cache_control_max_age(&headers(&["public, max-age=120"])),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            cache_control_max_age(&headers(&["MAX-AGE=\"60\""])),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            cache_control_max_age(&headers(&["no-cache, max-age=600"])),
            None
        );
        assert_eq!(cache_control_max_age(&headers(&["no-store"])), None);
        assert_eq!(cache_control_max_age(&headers(&["max-age=0"])), None);
        assert_eq!(cache_control_max_age(&headers(&["max-age=abc"])), None);
        assert_eq!(cache_control_max_age(&headers(&[])), None);
        assert_eq!(
            cache_control_max_age(&headers(&["public", "max-age=45"])),
            Some(Duration::from_secs(45))
        );
    }
}
