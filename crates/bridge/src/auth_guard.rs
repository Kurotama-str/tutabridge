//! Brute-force protection for the IMAP and SMTP password checks.
//!
//! Failed logins are counted per client IP, shared by both protocols. The
//! first [`FREE_FAILURES`] failures cost nothing; every further failure locks
//! that IP out for one more minute than the last, up to [`MAX_LOCKOUT`].
//! While locked out, attempts are refused without looking at the password.
//!
//! Two files live next to `config.toml`:
//! * `blacklist.txt`: one IP per line (`#` starts a comment). Connections from
//!   a listed IP are dropped before the TLS handshake. The file is re-read
//!   whenever it changes, so edits apply without a restart.
//! * `auth_failures.log`: one line per failure once an IP is past its free
//!   attempts, so repeat offenders are easy to find and copy into the blacklist.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{debug, warn};

use crate::util::lock_recover;

/// Failures an IP gets before lockouts start.
const FREE_FAILURES: u32 = 3;
/// Lockout added per failure beyond the free ones.
const LOCKOUT_STEP: Duration = Duration::from_secs(60);
/// Longest lockout, however many failures pile up.
const MAX_LOCKOUT: Duration = Duration::from_secs(300);
/// An IP that stays quiet this long starts over with a clean count.
const FORGET_AFTER: Duration = Duration::from_secs(3600);
/// Past this many tracked IPs, stale entries are dropped on the next failure.
const PRUNE_ABOVE: usize = 10_000;

const BLACKLIST_FILE: &str = "blacklist.txt";
const FAILURE_LOG_FILE: &str = "auth_failures.log";

const BLACKLIST_TEMPLATE: &str = "\
# IPs listed here are refused before any login is attempted.
# One address per line, IPv4 or IPv6. Lines starting with # are ignored.
# Changes apply without restarting the bridge.
";

struct Entry {
    failures: u32,
    locked_until: Option<Instant>,
    last_failure: Instant,
}

#[derive(Default)]
struct Blacklist {
    modified: Option<SystemTime>,
    ips: HashSet<IpAddr>,
}

pub(crate) struct AuthGuard {
    entries: Mutex<HashMap<IpAddr, Entry>>,
    blacklist: Mutex<Blacklist>,
    blacklist_path: Option<PathBuf>,
    failure_log: Option<PathBuf>,
}

impl AuthGuard {
    /// A guard that keeps its blacklist and failure log in `dir`, creating the
    /// blacklist file with an explanatory header when it does not exist yet.
    pub(crate) fn in_dir(dir: &Path) -> Self {
        let blacklist_path = dir.join(BLACKLIST_FILE);
        if !blacklist_path.exists() {
            if let Err(e) = crate::fs_private::create_dir_all_private(dir)
                .and_then(|_| std::fs::write(&blacklist_path, BLACKLIST_TEMPLATE))
            {
                warn!("Could not create {}: {e}", blacklist_path.display());
            }
        }
        Self {
            entries: Mutex::new(HashMap::new()),
            blacklist: Mutex::new(Blacklist::default()),
            blacklist_path: Some(blacklist_path),
            failure_log: Some(dir.join(FAILURE_LOG_FILE)),
        }
    }

    /// A guard with no files: counts and locks out, but has no blacklist and
    /// writes no log.
    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            blacklist: Mutex::new(Blacklist::default()),
            blacklist_path: None,
            failure_log: None,
        }
    }

    /// True when `ip` is listed in the blacklist file.
    pub(crate) fn is_blacklisted(&self, ip: IpAddr) -> bool {
        let Some(path) = &self.blacklist_path else {
            return false;
        };
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let mut list = lock_recover(&self.blacklist);
        if list.modified != modified {
            list.ips = match std::fs::read_to_string(path) {
                Ok(text) => parse_blacklist(&text),
                Err(_) => HashSet::new(),
            };
            list.modified = modified;
            debug!("Blacklist reloaded: {} address(es)", list.ips.len());
        }
        list.ips.contains(&ip.to_canonical())
    }

    /// `Some(wait)` while `ip` is locked out.
    pub(crate) fn locked_for(&self, ip: IpAddr) -> Option<Duration> {
        self.locked_for_at(ip, Instant::now())
    }

    fn locked_for_at(&self, ip: IpAddr, now: Instant) -> Option<Duration> {
        let entries = lock_recover(&self.entries);
        let until = entries.get(&ip.to_canonical())?.locked_until?;
        until.checked_duration_since(now).filter(|d| !d.is_zero())
    }

    /// Count a failed login from `ip` over `protocol`.
    pub(crate) fn record_failure(&self, ip: IpAddr, protocol: &str) {
        let lockout = self.record_failure_at(ip, Instant::now());
        if let Some((failures, lockout)) = lockout {
            let ip = ip.to_canonical();
            warn!(
                "{protocol} login from {ip} failed {failures} times; locked out for {}s",
                lockout.as_secs()
            );
            self.append_failure_log(&format!(
                "{} {protocol} {ip} failures={failures} locked_for={}s",
                utc_timestamp(),
                lockout.as_secs()
            ));
        }
    }

    /// Returns the failure count and lockout when this failure triggered one.
    fn record_failure_at(&self, ip: IpAddr, now: Instant) -> Option<(u32, Duration)> {
        let mut entries = lock_recover(&self.entries);
        if entries.len() > PRUNE_ABOVE {
            entries.retain(|_, e| now.saturating_duration_since(e.last_failure) < FORGET_AFTER);
        }
        let entry = entries.entry(ip.to_canonical()).or_insert(Entry {
            failures: 0,
            locked_until: None,
            last_failure: now,
        });
        if now.saturating_duration_since(entry.last_failure) >= FORGET_AFTER {
            entry.failures = 0;
            entry.locked_until = None;
        }
        entry.failures += 1;
        entry.last_failure = now;
        if entry.failures <= FREE_FAILURES {
            return None;
        }
        let lockout = (LOCKOUT_STEP * (entry.failures - FREE_FAILURES)).min(MAX_LOCKOUT);
        entry.locked_until = Some(now + lockout);
        Some((entry.failures, lockout))
    }

    /// A correct password clears the failure count for `ip`.
    pub(crate) fn record_success(&self, ip: IpAddr) {
        lock_recover(&self.entries).remove(&ip.to_canonical());
    }

    fn append_failure_log(&self, line: &str) {
        let Some(path) = &self.failure_log else {
            return;
        };
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = result {
            warn!("Could not write {}: {e}", path.display());
        }
    }
}

/// The addresses in a blacklist file. Anything that is not an IP is skipped
/// with a warning rather than failing the whole list.
fn parse_blacklist(text: &str) -> HashSet<IpAddr> {
    let mut ips = HashSet::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        match line.parse::<IpAddr>() {
            Ok(ip) => {
                ips.insert(ip.to_canonical());
            }
            Err(_) => warn!("Ignoring invalid blacklist entry: {line}"),
        }
    }
    ips
}

fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day) = crate::mail::rfc2822::days_to_ymd(secs / 86400);
    let tod = secs % 86400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// The guard both servers share, with its files in the config directory.
pub(crate) fn global() -> Arc<AuthGuard> {
    static GUARD: OnceLock<Arc<AuthGuard>> = OnceLock::new();
    GUARD
        .get_or_init(|| Arc::new(AuthGuard::in_dir(&crate::config::data_dir())))
        .clone()
}

/// The guard together with the client it is judging, so a login handler needs
/// no more than this one value.
#[derive(Clone)]
pub(crate) struct PeerGuard {
    guard: Arc<AuthGuard>,
    ip: IpAddr,
    protocol: &'static str,
}

impl PeerGuard {
    pub(crate) fn new(guard: Arc<AuthGuard>, ip: IpAddr, protocol: &'static str) -> Self {
        Self {
            guard,
            ip,
            protocol,
        }
    }

    /// Whole seconds (at least 1) until this client may try again, if locked out.
    pub(crate) fn retry_after_secs(&self) -> Option<u64> {
        self.guard
            .locked_for(self.ip)
            .map(|d| d.as_secs().max(1))
    }

    pub(crate) fn failed(&self) {
        self.guard.record_failure(self.ip, self.protocol);
    }

    pub(crate) fn succeeded(&self) {
        self.guard.record_success(self.ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([203, 0, 113, last])
    }

    #[test]
    fn the_first_three_failures_are_free() {
        let guard = AuthGuard::in_memory();
        let now = Instant::now();
        for _ in 0..3 {
            assert_eq!(guard.record_failure_at(ip(1), now), None);
            assert_eq!(guard.locked_for_at(ip(1), now), None);
        }
    }

    #[test]
    fn each_further_failure_adds_a_minute_up_to_five() {
        let guard = AuthGuard::in_memory();
        let now = Instant::now();
        for _ in 0..3 {
            guard.record_failure_at(ip(1), now);
        }
        let mut seen = Vec::new();
        for _ in 0..8 {
            let (_, lockout) = guard.record_failure_at(ip(1), now).unwrap();
            seen.push(lockout.as_secs());
        }
        assert_eq!(seen, [60, 120, 180, 240, 300, 300, 300, 300]);
    }

    #[test]
    fn a_locked_ip_is_free_again_once_the_lockout_passes() {
        let guard = AuthGuard::in_memory();
        let now = Instant::now();
        for _ in 0..4 {
            guard.record_failure_at(ip(1), now);
        }
        assert_eq!(
            guard.locked_for_at(ip(1), now),
            Some(Duration::from_secs(60))
        );
        assert_eq!(guard.locked_for_at(ip(1), now + Duration::from_secs(61)), None);
    }

    #[test]
    fn lockouts_are_per_ip() {
        let guard = AuthGuard::in_memory();
        let now = Instant::now();
        for _ in 0..4 {
            guard.record_failure_at(ip(1), now);
        }
        assert!(guard.locked_for_at(ip(1), now).is_some());
        assert_eq!(guard.locked_for_at(ip(2), now), None);
    }

    #[test]
    fn success_clears_the_count() {
        let guard = AuthGuard::in_memory();
        let now = Instant::now();
        for _ in 0..3 {
            guard.record_failure_at(ip(1), now);
        }
        guard.record_success(ip(1));
        assert_eq!(guard.record_failure_at(ip(1), now), None);
    }

    #[test]
    fn a_quiet_hour_wipes_the_count() {
        let guard = AuthGuard::in_memory();
        let now = Instant::now();
        for _ in 0..3 {
            guard.record_failure_at(ip(1), now);
        }
        let later = now + FORGET_AFTER;
        assert_eq!(guard.record_failure_at(ip(1), later), None);
    }

    #[test]
    fn mapped_ipv6_and_ipv4_share_a_count() {
        let guard = AuthGuard::in_memory();
        let now = Instant::now();
        let v4 = ip(1);
        let mapped: IpAddr = "::ffff:203.0.113.1".parse().unwrap();
        for _ in 0..3 {
            guard.record_failure_at(v4, now);
        }
        assert!(guard.record_failure_at(mapped, now).is_some());
    }

    #[test]
    fn blacklist_parsing_skips_comments_blanks_and_junk() {
        let ips = parse_blacklist(
            "# header\n\n203.0.113.5\n  203.0.113.6  # trailing note\nnot-an-ip\n2001:db8::1\n::ffff:203.0.113.7\n",
        );
        assert_eq!(ips.len(), 4);
        assert!(ips.contains(&"203.0.113.5".parse().unwrap()));
        assert!(ips.contains(&"203.0.113.6".parse().unwrap()));
        assert!(ips.contains(&"2001:db8::1".parse().unwrap()));
        assert!(ips.contains(&"203.0.113.7".parse().unwrap()));
    }

    #[test]
    fn the_blacklist_file_is_created_and_reloaded_on_change() {
        let dir = std::env::temp_dir().join(format!("tutabridge-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let guard = AuthGuard::in_dir(&dir);
        assert!(dir.join(BLACKLIST_FILE).exists());
        assert!(!guard.is_blacklisted(ip(9)));

        // Backdate the first read so the rewrite below is seen even on
        // filesystems with coarse modification times.
        guard.blacklist.lock().unwrap().modified = Some(UNIX_EPOCH);
        std::fs::write(dir.join(BLACKLIST_FILE), "203.0.113.9\n").unwrap();
        assert!(guard.is_blacklisted(ip(9)));
        assert!(!guard.is_blacklisted(ip(10)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failures_past_the_free_ones_are_logged() {
        let dir = std::env::temp_dir().join(format!("tutabridge-guardlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let guard = AuthGuard::in_dir(&dir);
        for _ in 0..4 {
            guard.record_failure(ip(3), "IMAP");
        }
        let log = std::fs::read_to_string(dir.join(FAILURE_LOG_FILE)).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(log.contains("IMAP 203.0.113.3 failures=4 locked_for=60s"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timestamps_are_iso_8601_utc() {
        let ts = utc_timestamp();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z') && ts.as_bytes()[10] == b'T');
    }
}
