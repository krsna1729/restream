//! Ingest authentication rate limiter — tracks per-IP failure counts and
//! applies time-based bans after exceeding the threshold. Protects RTMP/SRT
//! stream key brute-force attempts.

use crate::domain::ingest_security::IngestSecurityConfig;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;

#[derive(Clone, Copy)]
pub enum RateLimitScope {
    DashboardLogin,
    RtmpPublish,
    SrtPublish,
    SrtRead,
}

impl RateLimitScope {
    pub fn key(self) -> &'static str {
        match self {
            Self::DashboardLogin => "dashboard-login",
            Self::RtmpPublish => "rtmp-publish",
            Self::SrtPublish => "srt-publish",
            Self::SrtRead => "srt-read",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "dashboard-login" => Some(Self::DashboardLogin),
            "rtmp-publish" => Some(Self::RtmpPublish),
            "srt-publish" => Some(Self::SrtPublish),
            "srt-read" => Some(Self::SrtRead),
            _ => None,
        }
    }

    fn exempts_loopback(self) -> bool {
        !matches!(self, Self::DashboardLogin)
    }
}

struct FailureRecord {
    failures: Vec<Instant>,
    banned_until: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    pub scope: String,
    pub ip: String,
    pub failure_count: usize,
    pub banned: bool,
    pub ban_remaining_ms: Option<u64>,
}

pub struct IngestSecurityService {
    config: RwLock<IngestSecurityConfig>,
    state: RwLock<HashMap<String, FailureRecord>>,
}

impl IngestSecurityService {
    pub fn new(config: IngestSecurityConfig) -> Self {
        let mut config = config;
        config.normalize();
        Self {
            config: RwLock::new(config),
            state: RwLock::new(HashMap::new()),
        }
    }

    fn is_loopback_ip(ip: &str) -> bool {
        // Parse as IpAddr to cover the full loopback ranges:
        //   IPv4: 127.0.0.0/8 (not just 127.0.0.1)
        //   IPv6: ::1 and IPv4-mapped ::ffff:127.x.x.x
        //   Literal "localhost" fallback for non-parseable strings.
        if ip == "localhost" {
            return true;
        }
        ip.parse::<std::net::IpAddr>()
            .map(|a| a.is_loopback())
            .unwrap_or(false)
    }

    pub fn update_config(&self, new_config: IngestSecurityConfig) {
        let mut new_config = new_config;
        new_config.normalize();
        if let Ok(mut config) = self.config.write() {
            *config = new_config;
        }
    }

    pub fn get_config(&self) -> IngestSecurityConfig {
        self.config
            .read()
            .map(|c| c.clone())
            .unwrap_or(DEFAULT_INGEST_SECURITY_CONFIG)
    }

    pub fn is_ip_banned(&self, ip: &str) -> Option<Duration> {
        self.is_ip_banned_for(RateLimitScope::RtmpPublish, ip)
    }

    pub fn is_ip_banned_for(&self, scope: RateLimitScope, ip: &str) -> Option<Duration> {
        if scope.exempts_loopback() && Self::is_loopback_ip(ip) {
            return None;
        }

        // Read lock only — no mutations. Cleanup of stale entries happens
        // lazily in record_failure, keeping this hot check lock-free under
        // concurrent ban lookups (e.g., flood from many IPs).
        let state = self.state.read().ok()?;
        let key = Self::scoped_key(scope, ip);
        let record = state.get(&key)?;
        let now = Instant::now();

        if let Some(banned_until) = record.banned_until
            && banned_until > now
        {
            return Some(banned_until.duration_since(now));
        }

        None
    }

    pub fn record_failure(&self, ip: &str) -> bool {
        self.record_failure_for(RateLimitScope::RtmpPublish, ip)
    }

    pub fn record_failure_for(&self, scope: RateLimitScope, ip: &str) -> bool {
        if scope.exempts_loopback() && Self::is_loopback_ip(ip) {
            return false;
        }
        let mut state = match self.state.write() {
            Ok(s) => s,
            Err(_) => return false,
        };

        let now = Instant::now();
        let config = self.get_config();

        // Enforce the tracked-IP limit before inserting a new entry.
        let limit = config.tracked_ip_limit.max(1) as usize;
        Self::evict_oldest_if_needed(&mut state, limit.saturating_sub(1));

        let key = Self::scoped_key(scope, ip);
        let record = state.entry(key).or_insert_with(|| FailureRecord {
            failures: Vec::new(),
            banned_until: None,
        });

        record.failures.push(now);

        let window = Duration::from_millis(config.failure_window_ms as u64);
        record.failures.retain(|&t| now.duration_since(t) < window);

        if record.failures.len() >= config.failure_limit as usize {
            record.banned_until = Some(now + Duration::from_millis(config.ban_ms as u64));
            true // Banned
        } else {
            false // Not yet banned
        }
    }

    /// Evict the oldest entries when the map is over the tracked-IP limit.
    /// Keeps memory bounded under a sustained flood of distinct IPs.
    fn evict_oldest_if_needed(state: &mut HashMap<String, FailureRecord>, limit: usize) {
        if state.len() <= limit {
            return;
        }
        // Remove IPs whose ban has expired and have no recent failures first,
        // then fall back to evicting by oldest most-recent-failure to keep the map bounded.
        let now = Instant::now();
        state.retain(|_, r| {
            let expired_ban = r.banned_until.is_none_or(|t| t <= now);
            let has_failures = !r.failures.is_empty();
            !expired_ban || has_failures
        });
        // Hard cap: if still over limit, evict non-banned entries before banned
        // ones, and within each group evict the stalest (least recently failed)
        // first. HashMap's arbitrary iteration order would otherwise evict
        // random entries, potentially letting an attacker clear their own
        // active ban early by flooding from many disposable IPs. Ranking by
        // the *most recent* failure (not the earliest) matters too: an
        // earliest-failure ranking would preferentially evict a long-running,
        // still-active attacker over a single-shot flood IP, since a
        // sustained attacker's first failure is older than a flood IP's only
        // failure even though the attacker is still currently active.
        if state.len() > limit {
            let excess = state.len() - limit;
            let mut entries: Vec<(&String, &FailureRecord)> = state.iter().collect();
            entries.sort_by_key(|(_, r)| {
                let currently_banned = r.banned_until.is_some_and(|t| t > now);
                let most_recent_failure = r.failures.iter().copied().max().unwrap_or(now);
                (currently_banned, most_recent_failure)
            });
            let keys_to_remove: Vec<String> = entries
                .iter()
                .take(excess)
                .map(|(k, _)| (*k).clone())
                .collect();
            for k in keys_to_remove {
                state.remove(&k);
            }
        }
    }

    pub fn record_success(&self, ip: &str) {
        self.record_success_for(RateLimitScope::RtmpPublish, ip);
    }

    pub fn record_success_for(&self, scope: RateLimitScope, ip: &str) {
        if scope.exempts_loopback() && Self::is_loopback_ip(ip) {
            return;
        }
        if let Ok(mut state) = self.state.write() {
            state.remove(&Self::scoped_key(scope, ip));
        }
    }

    fn scoped_key(scope: RateLimitScope, ip: &str) -> String {
        format!("{}\0{}", scope.key(), ip)
    }

    fn parse_scoped_key(key: &str) -> Option<(&str, &str)> {
        key.split_once('\0')
            .filter(|(scope, ip)| RateLimitScope::from_key(scope).is_some() && !ip.is_empty())
    }

    pub fn snapshots(&self) -> Vec<RateLimitSnapshot> {
        let Ok(state) = self.state.read() else {
            return Vec::new();
        };
        let now = Instant::now();
        let mut snapshots = state
            .iter()
            .filter_map(|(key, record)| {
                let (scope, ip) = Self::parse_scoped_key(key)?;
                let banned_until = record.banned_until.filter(|until| *until > now);
                Some(RateLimitSnapshot {
                    scope: scope.to_string(),
                    ip: ip.to_string(),
                    failure_count: record.failures.len(),
                    banned: banned_until.is_some(),
                    ban_remaining_ms: banned_until
                        .map(|until| until.duration_since(now).as_millis() as u64),
                })
            })
            .collect::<Vec<_>>();
        snapshots.sort_by(|a, b| {
            b.banned
                .cmp(&a.banned)
                .then_with(|| a.scope.cmp(&b.scope))
                .then_with(|| a.ip.cmp(&b.ip))
        });
        snapshots
    }

    pub fn reset(&self, scope: Option<RateLimitScope>, ip: Option<&str>) -> usize {
        let Ok(mut state) = self.state.write() else {
            return 0;
        };
        let before = state.len();
        state.retain(|key, _| {
            let Some((stored_scope, stored_ip)) = Self::parse_scoped_key(key) else {
                return true;
            };
            if let Some(scope) = scope
                && stored_scope != scope.key()
            {
                return true;
            }
            if let Some(ip) = ip
                && stored_ip != ip
            {
                return true;
            }
            false
        });
        before.saturating_sub(state.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_scope_from_key_round_trips_and_rejects_unknown_keys() {
        for scope in [
            RateLimitScope::DashboardLogin,
            RateLimitScope::RtmpPublish,
            RateLimitScope::SrtPublish,
            RateLimitScope::SrtRead,
        ] {
            assert_eq!(
                RateLimitScope::from_key(scope.key()).map(|s| s.key()),
                Some(scope.key())
            );
        }
        for bogus in ["", "unknown", "Rtmp-Publish", "rtmp-publish "] {
            assert!(
                RateLimitScope::from_key(bogus).is_none(),
                "key={bogus:?} must not resolve to a scope"
            );
        }
    }

    // scoped_key joins "{scope}\0{ip}" and parse_scoped_key splits on the
    // first NUL. An ip string that itself contains a NUL (fully attacker
    // controlled input from e.g. a spoofed header) must not be able to
    // forge a different scope's key or otherwise get misparsed, since
    // split_once only ever separates at the *first* NUL.
    #[test]
    fn embedded_null_byte_in_ip_does_not_forge_a_different_scope_key() {
        let cfg = IngestSecurityConfig {
            failure_limit: 1,
            failure_window_ms: 60_000,
            ban_ms: 60_000,
            tracked_ip_limit: 1000,
        };
        let svc = IngestSecurityService::new(cfg);
        let hostile_ip = "srt-publish\x0198.51.100.9";

        assert!(svc.record_failure_for(RateLimitScope::DashboardLogin, hostile_ip));
        assert!(
            svc.is_ip_banned_for(RateLimitScope::DashboardLogin, hostile_ip)
                .is_some(),
            "the hostile ip must be banned under its real scope"
        );
        assert!(
            svc.is_ip_banned_for(RateLimitScope::SrtPublish, "198.51.100.9")
                .is_none(),
            "an embedded NUL in the ip must not let it masquerade as an srt-publish ban"
        );

        let snapshots = svc.snapshots();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].scope, "dashboard-login");
        assert_eq!(snapshots[0].ip, hostile_ip);
    }

    #[test]
    fn update_config_changes_take_effect_for_subsequent_calls() {
        let svc = IngestSecurityService::new(IngestSecurityConfig {
            failure_limit: 100,
            failure_window_ms: 60_000,
            ban_ms: 10_000,
            tracked_ip_limit: 1000,
        });
        let ip = "203.0.113.70";

        // Starting limit is high — one failure must not ban.
        assert!(!svc.record_failure(ip));
        assert!(svc.is_ip_banned(ip).is_none());

        svc.update_config(IngestSecurityConfig {
            failure_limit: 1,
            failure_window_ms: 60_000,
            ban_ms: 10_000,
            tracked_ip_limit: 1000,
        });
        assert_eq!(svc.get_config().failure_limit, 1);

        // Next failure is evaluated against the new, stricter limit.
        assert!(svc.record_failure(ip));
        assert!(svc.is_ip_banned(ip).is_some());
    }

    #[test]
    fn ipv4_mapped_ipv6_loopback_is_exempt() {
        let service = IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG);
        assert!(
            service.is_ip_banned("::ffff:127.0.0.1").is_none(),
            "IPv4-mapped IPv6 loopback must be treated as loopback"
        );
        assert!(!service.record_failure("::ffff:127.0.0.1"));
    }

    #[test]
    fn malformed_ip_strings_are_treated_as_non_loopback_and_rate_limited() {
        let cfg = IngestSecurityConfig {
            failure_limit: 1,
            failure_window_ms: 60_000,
            ban_ms: 10_000,
            tracked_ip_limit: 1000,
        };
        let svc = IngestSecurityService::new(cfg);
        for ip in &["", "not-an-ip", "999.999.999.999"] {
            assert!(
                svc.record_failure(ip),
                "malformed ip {ip:?} must fall back to non-loopback and be rate limited"
            );
            assert!(svc.is_ip_banned(ip).is_some(), "ip {ip:?} should be banned");
        }
    }

    #[test]
    fn loopback_ips_are_not_rate_limited() {
        let service = IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG);

        // IPv4 loopback variants
        for ip in &["127.0.0.1", "127.0.0.2", "127.255.255.255", "localhost"] {
            assert!(
                service.is_ip_banned(ip).is_none(),
                "loopback {ip} should be exempt"
            );
            assert!(
                !service.record_failure(ip),
                "loopback {ip} should not record failure"
            );
            assert!(
                service.is_ip_banned(ip).is_none(),
                "loopback {ip} should remain exempt after failure"
            );
        }
        // IPv6 loopback
        assert!(service.is_ip_banned("::1").is_none());
        assert!(!service.record_failure("::1"));
    }

    #[test]
    fn non_loopback_ips_are_rate_limited() {
        let cfg = IngestSecurityConfig {
            failure_limit: 2,
            failure_window_ms: 60_000,
            ban_ms: 10_000,
            tracked_ip_limit: 1000,
        };
        let svc = IngestSecurityService::new(cfg);
        // 10.x.x.x is not loopback
        assert!(!svc.record_failure("10.0.0.1"));
        assert!(svc.record_failure("10.0.0.1")); // 2nd → banned
        assert!(svc.is_ip_banned("10.0.0.1").is_some());
        // 192.168.x.x is not loopback
        assert!(!svc.record_failure("192.168.1.1"));
        assert!(svc.is_ip_banned("192.168.1.1").is_none()); // only 1 failure, not banned
    }

    #[test]
    fn dashboard_login_does_not_exempt_loopback() {
        let cfg = IngestSecurityConfig {
            failure_limit: 2,
            failure_window_ms: 60_000,
            ban_ms: 10_000,
            tracked_ip_limit: 1000,
        };
        let svc = IngestSecurityService::new(cfg);

        assert!(!svc.record_failure_for(RateLimitScope::DashboardLogin, "127.0.0.1"));
        assert!(svc.record_failure_for(RateLimitScope::DashboardLogin, "127.0.0.1"));
        assert!(
            svc.is_ip_banned_for(RateLimitScope::DashboardLogin, "127.0.0.1")
                .is_some()
        );
        assert!(svc.is_ip_banned("127.0.0.1").is_none());
    }

    #[test]
    fn success_only_clears_matching_rate_limit_scope() {
        let cfg = IngestSecurityConfig {
            failure_limit: 2,
            failure_window_ms: 60_000,
            ban_ms: 10_000,
            tracked_ip_limit: 1000,
        };
        let svc = IngestSecurityService::new(cfg);
        let ip = "203.0.113.55";

        assert!(!svc.record_failure_for(RateLimitScope::DashboardLogin, ip));
        assert!(svc.record_failure_for(RateLimitScope::DashboardLogin, ip));
        assert!(
            svc.is_ip_banned_for(RateLimitScope::DashboardLogin, ip)
                .is_some()
        );

        svc.record_success_for(RateLimitScope::SrtPublish, ip);
        assert!(
            svc.is_ip_banned_for(RateLimitScope::DashboardLogin, ip)
                .is_some(),
            "SRT publish success must not clear dashboard login failures"
        );

        svc.record_success_for(RateLimitScope::DashboardLogin, ip);
        assert!(
            svc.is_ip_banned_for(RateLimitScope::DashboardLogin, ip)
                .is_none()
        );
    }

    #[test]
    fn snapshots_and_reset_expose_scoped_failures() {
        let cfg = IngestSecurityConfig {
            failure_limit: 2,
            failure_window_ms: 60_000,
            ban_ms: 10_000,
            tracked_ip_limit: 1000,
        };
        let svc = IngestSecurityService::new(cfg);
        let ip = "203.0.113.60";

        assert!(!svc.record_failure_for(RateLimitScope::SrtPublish, ip));
        assert!(svc.record_failure_for(RateLimitScope::SrtPublish, ip));
        assert!(!svc.record_failure_for(RateLimitScope::DashboardLogin, ip));

        let snapshots = svc.snapshots();
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots.iter().any(|snapshot| {
            snapshot.scope == "srt-publish"
                && snapshot.ip == ip
                && snapshot.failure_count == 2
                && snapshot.banned
                && snapshot.ban_remaining_ms.is_some()
        }));
        assert!(snapshots.iter().any(|snapshot| {
            snapshot.scope == "dashboard-login"
                && snapshot.ip == ip
                && snapshot.failure_count == 1
                && !snapshot.banned
        }));

        assert_eq!(svc.reset(Some(RateLimitScope::SrtPublish), Some(ip)), 1);
        let snapshots = svc.snapshots();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].scope, "dashboard-login");

        assert_eq!(svc.reset(None, None), 1);
        assert!(svc.snapshots().is_empty());
    }

    #[test]
    fn ip_is_banned_after_failure_limit() {
        let cfg = IngestSecurityConfig {
            failure_limit: 3,
            failure_window_ms: 60_000,
            ban_ms: 10_000,
            tracked_ip_limit: 1000,
        };
        let svc = IngestSecurityService::new(cfg);
        let ip = "1.2.3.4";

        assert!(!svc.record_failure(ip)); // 1
        assert!(!svc.record_failure(ip)); // 2
        assert!(svc.record_failure(ip)); // 3 → banned
        assert!(svc.is_ip_banned(ip).is_some(), "IP should be banned");
    }

    #[test]
    fn record_success_clears_failure_state() {
        let cfg = IngestSecurityConfig {
            failure_limit: 3,
            failure_window_ms: 60_000,
            ban_ms: 10_000,
            tracked_ip_limit: 1000,
        };
        let svc = IngestSecurityService::new(cfg);
        let ip = "5.6.7.8";

        svc.record_failure(ip);
        svc.record_failure(ip);
        svc.record_success(ip); // should clear state
        // After success, two more failures should not ban (below limit)
        assert!(!svc.record_failure(ip));
        assert!(!svc.record_failure(ip));
        assert!(svc.is_ip_banned(ip).is_none());
    }

    #[test]
    fn tracked_ip_limit_is_enforced() {
        let cfg = IngestSecurityConfig {
            failure_limit: 100,
            failure_window_ms: 60_000,
            ban_ms: 60_000,
            tracked_ip_limit: 5, // very small limit
        };
        let svc = IngestSecurityService::new(cfg);

        // Insert 10 distinct IPs — the map must not exceed the limit
        for i in 0..10u8 {
            svc.record_failure(&format!("10.0.0.{i}"));
        }

        let state = svc.state.read().unwrap();
        assert!(
            state.len() <= 5,
            "tracked IP map must not exceed limit, got {}",
            state.len()
        );
    }

    // Regression for a real bug: the tracked-IP eviction hard cap sorted by
    // each record's *earliest* failure and ignored ban status entirely, so
    // an attacker could clear their own active ban well before ban_ms
    // elapsed by flooding the table with disposable throwaway IPs from
    // other addresses. An active ban must survive an unrelated flood.
    #[test]
    fn flood_of_other_ips_does_not_evict_an_active_ban() {
        let cfg = IngestSecurityConfig {
            failure_limit: 2,
            failure_window_ms: 60_000,
            ban_ms: 5 * 60_000,
            tracked_ip_limit: 3,
        };
        let svc = IngestSecurityService::new(cfg);
        // Attacker gets banned on 10.0.0.1.
        svc.record_failure("10.0.0.1");
        svc.record_failure("10.0.0.1");
        assert!(svc.is_ip_banned("10.0.0.1").is_some(), "must be banned");

        // Two more IPs fill the table to tracked_ip_limit.
        svc.record_failure("10.0.0.2");
        svc.record_failure("10.0.0.3");

        // Flood of throwaway single-failure IPs from other addresses, far
        // beyond the tracked-IP limit.
        for i in 4..20u8 {
            svc.record_failure(&format!("10.0.0.{i}"));
        }

        assert!(
            svc.is_ip_banned("10.0.0.1").is_some(),
            "an active ban must survive a flood of unrelated throwaway IPs"
        );
    }

    // Companion case: a currently-banned entry must be evicted before it is
    // allowed to displace a *non-banned* record with more recent activity —
    // i.e. banned status is a hard priority tier, not just a tiebreaker.
    #[test]
    fn eviction_prefers_non_banned_entries_over_banned_ones_regardless_of_recency() {
        let cfg = IngestSecurityConfig {
            failure_limit: 2,
            failure_window_ms: 60_000,
            ban_ms: 5 * 60_000,
            tracked_ip_limit: 2,
        };
        let svc = IngestSecurityService::new(cfg);

        // 10.0.0.1 gets banned first (its failures are therefore older).
        svc.record_failure("10.0.0.1");
        svc.record_failure("10.0.0.1");
        assert!(svc.is_ip_banned("10.0.0.1").is_some());

        // 10.0.0.2 fails once, more recently than the ban above, but is not
        // itself banned yet.
        svc.record_failure("10.0.0.2");

        // A third IP forces eviction down to the limit of 2. Despite
        // 10.0.0.2's failure being more recent than nothing (there is no
        // older non-banned competitor here), the still-banned 10.0.0.1 must
        // not be the one dropped merely for having an older timestamp.
        svc.record_failure("10.0.0.3");

        assert!(
            svc.is_ip_banned("10.0.0.1").is_some(),
            "banned entry must not be evicted ahead of a non-banned one"
        );
    }

    // H1: verify the exact call sequence SRT handle_client uses:
    //   1. is_ip_banned → None (clean IP)
    //   2. record_failure × N → eventually banned
    //   3. is_ip_banned → Some (banned)
    //   4. record_success → ban cleared
    //   5. is_ip_banned → None again
    #[test]
    fn srt_ingest_security_call_sequence() {
        let cfg = IngestSecurityConfig {
            failure_limit: 3,
            failure_window_ms: 60_000,
            ban_ms: 60_000,
            tracked_ip_limit: 1000,
        };
        let svc = IngestSecurityService::new(cfg);
        let ip = "203.0.113.1"; // TEST-NET, never a real loopback

        // Step 1: clean IP — allowed through
        assert!(svc.is_ip_banned(ip).is_none());

        // Step 2: three auth failures — third call triggers ban
        assert!(!svc.record_failure(ip));
        assert!(!svc.record_failure(ip));
        assert!(svc.record_failure(ip), "third failure must ban the IP");

        // Step 3: now rejected
        assert!(
            svc.is_ip_banned(ip).is_some(),
            "banned IP must be rejected at gate"
        );

        // Step 4: successful auth clears state
        svc.record_success(ip);

        // Step 5: allowed again
        assert!(
            svc.is_ip_banned(ip).is_none(),
            "IP must be allowed after record_success"
        );
    }

    // H3: is_ip_banned uses a read lock — many concurrent callers must not
    // deadlock and must all see the correct ban status.
    #[test]
    fn concurrent_is_ip_banned_no_deadlock_or_wrong_result() {
        use std::sync::Arc;

        let cfg = IngestSecurityConfig {
            failure_limit: 1,
            failure_window_ms: 60_000,
            ban_ms: 60_000,
            tracked_ip_limit: 1000,
        };
        let svc = Arc::new(IngestSecurityService::new(cfg));
        let ip = "198.51.100.1"; // TEST-NET

        // Pre-ban the IP
        svc.record_failure(ip);
        assert!(svc.is_ip_banned(ip).is_some());

        // 16 concurrent threads all read the ban status simultaneously.
        // A write lock would serialise them; a read lock allows parallelism.
        // Any deadlock shows up as a test timeout.
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let s = svc.clone();
                let ip = ip.to_string();
                std::thread::spawn(move || {
                    assert!(
                        s.is_ip_banned(&ip).is_some(),
                        "all readers must see the ban"
                    );
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }
    }
}
