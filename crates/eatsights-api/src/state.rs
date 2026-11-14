//! Shared state, configuration, response cache and metrics.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use bytes::Bytes;
use eatsights_core::registry::{Registry, Scope};
use eatsights_core::submission::CheckConfig;
use eatsights_db::Db;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};

/// API configuration (from the environment in the binary).
#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub base_url: String,
    pub pseudonym_salt: Vec<u8>,
    pub pseudonym_salt_version: i32,
    pub check: CheckConfig,
    pub trusted_proxies: Vec<Cidr>,
    pub rate_per_key_per_min: u32,
    pub rate_per_ip_per_min: u32,
    pub keyreg_per_ip_per_hour: u32,
    pub cache_mb: u64,
    /// max-age = min relevant half-life * this factor, clamped.
    pub max_age_factor: f64,
    pub request_timeout_secs: u64,
    pub body_limit_bytes: usize,
    /// Serve Swagger UI at /docs. Off by default (AGENTS.md: no UI); see ADR-0005.
    pub swagger_ui: bool,
    /// Trust policy numbers (ADR-0006).
    pub trust: eatsights_core::trust::TrustConfig,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            base_url: eatsights_core::BASE_URL.to_owned(),
            pseudonym_salt: Vec::new(),
            pseudonym_salt_version: 1,
            check: CheckConfig::default(),
            trusted_proxies: Vec::new(),
            rate_per_key_per_min: 120,
            rate_per_ip_per_min: 600,
            keyreg_per_ip_per_hour: 30,
            cache_mb: 64,
            max_age_factor: 0.01,
            request_timeout_secs: 10,
            body_limit_bytes: 1 << 20,
            swagger_ui: false,
            trust: eatsights_core::trust::TrustConfig::default(),
        }
    }
}

/// An IPv4 or IPv6 network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: u128,
    v4: bool,
    bits: u8,
}

fn ip_bits(ip: IpAddr) -> (u128, bool) {
    match ip {
        IpAddr::V4(a) => (u128::from(u32::from(a)), true),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => (u128::from(u32::from(v4)), true),
            None => (u128::from(a), false),
        },
    }
}

impl Cidr {
    pub fn parse(s: &str) -> Option<Self> {
        let (a, bits) = match s.split_once('/') {
            Some((a, b)) => (a, Some(b.parse::<u8>().ok()?)),
            None => (s, None),
        };
        let ip: IpAddr = a.trim().parse().ok()?;
        let (addr, v4) = ip_bits(ip);
        let max = if v4 { 32 } else { 128 };
        let bits = bits.unwrap_or(max);
        (bits <= max).then_some(Self { addr, v4, bits })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let (a, v4) = ip_bits(ip);
        if v4 != self.v4 {
            return false;
        }
        let width = if v4 { 32 } else { 128 };
        if self.bits == 0 {
            return true;
        }
        let shift = width - u32::from(self.bits);
        (a >> shift) == (self.addr >> shift)
    }

    pub fn parse_list(s: &str) -> Vec<Self> {
        s.split(',')
            .filter(|p| !p.trim().is_empty())
            .filter_map(|p| Self::parse(p.trim()))
            .collect()
    }
}

/// The client address: the peer, or the right-most untrusted `X-Forwarded-For` hop
/// when the peer is a trusted proxy.
pub fn client_ip(peer: Option<IpAddr>, xff: Option<&str>, trusted: &[Cidr]) -> Option<IpAddr> {
    let peer = peer?;
    if !trusted.iter().any(|c| c.contains(peer)) {
        return Some(peer);
    }
    let mut ip = peer;
    if let Some(x) = xff {
        for hop in x.rsplit(',') {
            match hop.trim().parse::<IpAddr>() {
                Ok(h) => {
                    ip = h;
                    if !trusted.iter().any(|c| c.contains(h)) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    }
    Some(ip)
}

#[derive(Debug, Default)]
pub struct Metrics {
    pub requests_2xx: AtomicU64,
    pub requests_3xx: AtomicU64,
    pub requests_4xx: AtomicU64,
    pub requests_5xx: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub not_modified: AtomicU64,
    pub items_accepted: AtomicU64,
    pub items_duplicate: AtomicU64,
    pub items_rejected: AtomicU64,
}

impl Metrics {
    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }

    pub fn render(&self, seq: i64) -> String {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        format!(
            "# TYPE eatsights_http_requests_total counter\n\
             eatsights_http_requests_total{{class=\"2xx\"}} {}\n\
             eatsights_http_requests_total{{class=\"3xx\"}} {}\n\
             eatsights_http_requests_total{{class=\"4xx\"}} {}\n\
             eatsights_http_requests_total{{class=\"5xx\"}} {}\n\
             # TYPE eatsights_cache_total counter\n\
             eatsights_cache_total{{result=\"hit\"}} {}\n\
             eatsights_cache_total{{result=\"miss\"}} {}\n\
             eatsights_cache_total{{result=\"not_modified\"}} {}\n\
             # TYPE eatsights_write_items_total counter\n\
             eatsights_write_items_total{{status=\"accepted\"}} {}\n\
             eatsights_write_items_total{{status=\"duplicate\"}} {}\n\
             eatsights_write_items_total{{status=\"rejected\"}} {}\n\
             # TYPE eatsights_changes_seq gauge\n\
             eatsights_changes_seq {}\n",
            g(&self.requests_2xx),
            g(&self.requests_3xx),
            g(&self.requests_4xx),
            g(&self.requests_5xx),
            g(&self.cache_hits),
            g(&self.cache_misses),
            g(&self.not_modified),
            g(&self.items_accepted),
            g(&self.items_duplicate),
            g(&self.items_rejected),
            seq
        )
    }
}

/// A rendered response body, identity and gzip, valid for one change sequence.
#[derive(Debug)]
pub struct Cached {
    pub seq: i64,
    pub body: Bytes,
    pub gzip: Bytes,
}

pub struct AppState {
    pub db: Db,
    pub registry: &'static Registry,
    pub cfg: ApiConfig,
    /// Latest `public.changes.seq` seen (LISTEN/NOTIFY + polling).
    pub seq: Arc<AtomicI64>,
    pub cache: moka::sync::Cache<String, Arc<Cached>>,
    pub limit_key: DefaultKeyedRateLimiter<String>,
    pub limit_ip: DefaultKeyedRateLimiter<IpAddr>,
    pub limit_keyreg: DefaultKeyedRateLimiter<IpAddr>,
    pub metrics: Metrics,
}

fn nz(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n.max(1)).unwrap_or(NonZeroU32::MIN)
}

impl AppState {
    pub fn new(db: Db, registry: &'static Registry, cfg: ApiConfig, seq: Arc<AtomicI64>) -> Self {
        let cache = moka::sync::Cache::builder()
            .max_capacity(cfg.cache_mb.saturating_mul(1 << 20))
            .weigher(|k: &String, v: &Arc<Cached>| {
                u32::try_from(k.len() + v.body.len() + v.gzip.len()).unwrap_or(u32::MAX)
            })
            .build();
        Self {
            limit_key: RateLimiter::keyed(Quota::per_minute(nz(cfg.rate_per_key_per_min))),
            limit_ip: RateLimiter::keyed(Quota::per_minute(nz(cfg.rate_per_ip_per_min))),
            limit_keyreg: RateLimiter::keyed(Quota::per_hour(nz(cfg.keyreg_per_ip_per_hour))),
            db,
            registry,
            cfg,
            seq,
            cache,
            metrics: Metrics::default(),
        }
    }

    pub fn current_seq(&self) -> i64 {
        self.seq.load(Ordering::Acquire)
    }

    /// Cache lifetime in seconds from the shortest half-life among the given scopes
    /// (or one property), clamped to [5 s, 1 h].
    pub fn max_age(&self, scopes: &[Scope], property: Option<&str>) -> u64 {
        let hl = self
            .registry
            .iter()
            .filter(|p| match property {
                Some(id) => p.id == id,
                None => scopes.contains(&p.scope),
            })
            .map(|p| p.half_life_hours)
            .fold(f64::INFINITY, f64::min);
        let secs = if hl.is_finite() {
            hl * 3600.0 * self.cfg.max_age_factor
        } else {
            3600.0
        };
        secs.clamp(5.0, 3600.0) as u64
    }

    pub fn forget_old_limits(&self) {
        self.limit_key.retain_recent();
        self.limit_ip.retain_recent();
        self.limit_keyreg.retain_recent();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn cidr_and_xff() {
        let t = Cidr::parse_list("10.0.0.0/8, ::1");
        assert_eq!(t.len(), 2);
        let peer: IpAddr = "10.1.2.3".parse().unwrap();
        assert_eq!(
            client_ip(Some(peer), Some("1.2.3.4, 10.0.0.9"), &t),
            Some("1.2.3.4".parse().unwrap())
        );
        let outside: IpAddr = "8.8.8.8".parse().unwrap();
        assert_eq!(client_ip(Some(outside), Some("1.2.3.4"), &t), Some(outside));
        assert!(Cidr::parse("1.2.3.4/33").is_none());
    }
}
