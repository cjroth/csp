//! Parsing & writing of `.context/authorized_keys` entries (§10).
//!
//! Standard OpenSSH format with a small extension: trailing
//! whitespace-separated `key=value` tokens in the comment field encode
//! per-entry policy. Standard SSH tooling still parses these as opaque
//! comment text — they only have meaning to CSP.
//!
//! Recognized tokens:
//! - `expires=YYYY-MM-DD` — absolute UTC date expiry. After this date the
//!   listener refuses admission via this entry; the line stays for audit.
//! - `expires=never` — explicit opt-out, never expires; never rewritten by
//!   listen-start migration.
//! - `ttl=NNd added=YYYY-MM-DD` — equivalent input form (`expires=` =
//!   `added` + `ttl`). Normalizes to absolute `expires=` on next write.
//! - *(no expiry token)* — "unset, please apply default" — admitted at run
//!   time (footgun guard: a hand-pasted line is never silently rejected),
//!   rewritten by listen-start migration with `expires=<today + default>`.
//!
//! This module is **wasm-safe and I/O-free**: it only manipulates strings.
//! Caller (Vault on native, MemEngine in wasm) supplies `now_unix` and owns
//! file I/O.

use crate::identity::parse_ssh_pubkey;
use crate::order::NodeId;

const TOK_EXPIRES: &str = "expires=";
const TOK_TTL: &str = "ttl=";
const TOK_ADDED: &str = "added=";
const NEVER: &str = "never";

const SECS_PER_DAY: u64 = 86_400;

/// Effective expiry for one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    /// Absolute UTC expiry instant (unix seconds). Admit while `now < at`.
    At(u64),
    /// `expires=never` — explicit opt-out. Never migrated.
    Never,
    /// No expiry token on the line. Treated as non-expiring at admit time
    /// (so a manually pasted line is never silently rejected); listen-start
    /// migration converts these to `At(today + default_ttl)`.
    Unset,
}

impl Expiry {
    /// True if this entry is currently admissible (i.e. not past `expires=`).
    /// `Unset` and `Never` are always admissible.
    pub fn is_valid(&self, now_unix: u64) -> bool {
        match self {
            Expiry::At(t) => now_unix < *t,
            Expiry::Never | Expiry::Unset => true,
        }
    }
}

/// One parsed line.
#[derive(Debug, Clone)]
pub struct KeyEntry {
    /// Original line text (no trailing newline). Lines that aren't keys
    /// (blank, `#` comments) are kept verbatim with `node = None`.
    pub raw: String,
    /// Parsed NodeId, or `None` if this isn't an ssh-ed25519 key line.
    pub node: Option<NodeId>,
    /// Resolved expiry. `Unset` if the line carries no expiry token.
    pub expiry: Expiry,
}

impl KeyEntry {
    /// Is this a real key entry (not a comment/blank/malformed line)?
    pub fn is_key(&self) -> bool {
        self.node.is_some()
    }
}

/// Parse a single `authorized_keys` line. Always succeeds: lines we don't
/// understand round-trip as `node = None` (preserved verbatim).
pub fn parse_line(line: &str) -> KeyEntry {
    let raw = line.trim_end_matches('\n').trim_end_matches('\r').to_string();
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return KeyEntry { raw, node: None, expiry: Expiry::Unset };
    }
    let node = parse_ssh_pubkey(trimmed);
    let expiry = if node.is_some() { parse_expiry_tokens(trimmed) } else { Expiry::Unset };
    KeyEntry { raw, node, expiry }
}

/// Parse the entire file content (newline-separated). Preserves comment /
/// blank / malformed lines so a round-trip write doesn't drop them.
pub fn parse_file(s: &str) -> Vec<KeyEntry> {
    s.lines().map(parse_line).collect()
}

/// Serialize entries back to file content (terminating newline).
pub fn serialize(entries: &[KeyEntry]) -> String {
    let mut s: String = entries
        .iter()
        .map(|e| e.raw.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Read the expiry policy out of a key line by scanning its trailing tokens
/// (the OpenSSH "comment" — really whitespace-separated text after the
/// base64 key blob). Last `expires=` wins; `ttl=` + `added=` is the
/// fallback equivalent form.
fn parse_expiry_tokens(line: &str) -> Expiry {
    // Tokenize after `ssh-ed25519 <base64> …`.
    let mut it = line.split_whitespace();
    let _algo = it.next();
    let _b64 = it.next();
    let mut expires: Option<Expiry> = None;
    let mut ttl_days: Option<u64> = None;
    let mut added: Option<u64> = None;
    for tok in it {
        if let Some(v) = tok.strip_prefix(TOK_EXPIRES) {
            expires = Some(if v == NEVER {
                Expiry::Never
            } else {
                match parse_date_ymd_utc(v) {
                    Some(t) => Expiry::At(t),
                    None => continue, // malformed → ignore; remains Unset
                }
            });
        } else if let Some(v) = tok.strip_prefix(TOK_TTL) {
            if let Some(days) = parse_duration_days(v) {
                ttl_days = Some(days);
            }
        } else if let Some(v) = tok.strip_prefix(TOK_ADDED) {
            if let Some(t) = parse_date_ymd_utc(v) {
                added = Some(t);
            }
        }
    }
    if let Some(e) = expires {
        return e;
    }
    match (ttl_days, added) {
        (Some(days), Some(start)) => Expiry::At(start.saturating_add(days * SECS_PER_DAY)),
        _ => Expiry::Unset,
    }
}

/// Strip any existing `expires=` / `ttl=` / `added=` tokens from a line's
/// comment field, returning `(head_without_expiry_tokens, removed_count)`.
fn strip_expiry_tokens(line: &str) -> String {
    line.split_whitespace()
        .filter(|t| {
            !t.starts_with(TOK_EXPIRES) && !t.starts_with(TOK_TTL) && !t.starts_with(TOK_ADDED)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Produce a fresh authorized_keys line for `ssh_line` (an OpenSSH-format
/// public key — algorithm + base64 + optional comment) with the given
/// expiry. Idempotent for the same `ssh_line` + `expiry`. Any pre-existing
/// expiry tokens in `ssh_line` are replaced.
pub fn build_line(ssh_line: &str, expiry: Expiry) -> String {
    let head = strip_expiry_tokens(ssh_line.trim());
    match expiry {
        Expiry::At(t) => {
            let ymd = format_date_ymd_utc(t);
            format!("{head} {TOK_EXPIRES}{ymd}")
        }
        Expiry::Never => format!("{head} {TOK_EXPIRES}{NEVER}"),
        Expiry::Unset => head,
    }
}

/// `now_unix + ttl_days * 86400`, rounded UP to the next UTC midnight so the
/// on-disk date is exactly `YYYY-MM-DD`. Returns the unix seconds for that
/// midnight.
pub fn expiry_from_ttl_days(now_unix: u64, ttl_days: u64) -> u64 {
    let target = now_unix.saturating_add(ttl_days * SECS_PER_DAY);
    // Round to start-of-day UTC so the on-disk date matches one human day.
    (target / SECS_PER_DAY + 1) * SECS_PER_DAY
}

/// Parse a duration string like `90d`, `1y`, `12w`, `30d`, or `never`.
/// `never` returns `None` (caller treats as `Expiry::Never`). Returns days.
pub fn parse_duration_days(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s == NEVER || s == "0" {
        return None;
    }
    if let Some(n) = s.strip_suffix('d') {
        return n.parse::<u64>().ok();
    }
    if let Some(n) = s.strip_suffix('w') {
        return n.parse::<u64>().ok().map(|w| w * 7);
    }
    if let Some(n) = s.strip_suffix('y') {
        return n.parse::<u64>().ok().map(|y| y * 365);
    }
    // Bare integer = days.
    s.parse::<u64>().ok()
}

/// Parse `YYYY-MM-DD` → unix seconds at 00:00:00Z. Strict format.
pub fn parse_date_ymd_utc(s: &str) -> Option<u64> {
    let mut it = s.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let mo: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || !(1970..=9999).contains(&y) {
        return None;
    }
    let days = days_from_civil(y, mo as u32, d as u32);
    u64::try_from(days * SECS_PER_DAY as i64).ok()
}

/// Format unix seconds → `YYYY-MM-DD` (UTC).
pub fn format_date_ymd_utc(t: u64) -> String {
    let days = (t / SECS_PER_DAY) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days_from_civil — pure arithmetic, no allocations.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = (yy - era * 400) as i64;
    let mm = m as i64;
    let dd = d as i64;
    let doy = (153 * (if mm > 2 { mm - 3 } else { mm + 9 }) + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Inverse of `days_from_civil`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(seed: u8) -> String {
        crate::identity::Identity::from_seed(&[seed; 32]).to_ssh_string()
    }

    #[test]
    fn round_trip_ymd_dates() {
        // Round-trip: every YYYY-MM-DD we format back is what we started
        // with, and parse(format(x)) == x for arbitrary day-aligned unix
        // seconds. Spot-checks for an epoch baseline + a leap day + a
        // non-leap century day.
        assert_eq!(parse_date_ymd_utc("1970-01-01"), Some(0));
        assert_eq!(format_date_ymd_utc(0), "1970-01-01");
        for s in ["2026-05-20", "2026-08-18", "2024-02-29", "2100-03-01"] {
            let t = parse_date_ymd_utc(s).unwrap_or_else(|| panic!("parse {s}"));
            assert_eq!(t % SECS_PER_DAY, 0, "day-aligned: {s}");
            assert_eq!(format_date_ymd_utc(t), s, "round-trip: {s}");
        }
        // Day arithmetic stays correct across many random offsets.
        for &days in &[1i64, 365, 366, 1461, 10_000, 36_525, 50_000] {
            let t = (days as u64) * SECS_PER_DAY;
            let s = format_date_ymd_utc(t);
            assert_eq!(parse_date_ymd_utc(&s), Some(t), "round-trip days={days}");
        }
    }

    #[test]
    fn rejects_malformed_dates() {
        assert!(parse_date_ymd_utc("2026/05/20").is_none());
        assert!(parse_date_ymd_utc("2026-13-01").is_none());
        assert!(parse_date_ymd_utc("2026-05-32").is_none());
        assert!(parse_date_ymd_utc("").is_none());
        assert!(parse_date_ymd_utc("notadate").is_none());
    }

    #[test]
    fn duration_forms() {
        assert_eq!(parse_duration_days("90d"), Some(90));
        assert_eq!(parse_duration_days("12w"), Some(84));
        assert_eq!(parse_duration_days("1y"), Some(365));
        assert_eq!(parse_duration_days("30"), Some(30));
        assert_eq!(parse_duration_days("never"), None);
        assert_eq!(parse_duration_days("NEVER"), None);
        assert_eq!(parse_duration_days("0"), None);
        assert_eq!(parse_duration_days("trash"), None);
    }

    #[test]
    fn parses_expires_token() {
        let line = format!("{} expires=2026-08-18", pk(1));
        let e = parse_line(&line);
        assert!(e.is_key());
        match e.expiry {
            Expiry::At(t) => assert_eq!(format_date_ymd_utc(t), "2026-08-18"),
            other => panic!("expected At, got {other:?}"),
        }
    }

    #[test]
    fn parses_expires_never_explicitly() {
        let line = format!("{} expires=never", pk(2));
        let e = parse_line(&line);
        assert!(matches!(e.expiry, Expiry::Never));
    }

    #[test]
    fn bare_line_is_unset_not_rejected() {
        let line = pk(3);
        let e = parse_line(&line);
        assert!(e.is_key());
        assert!(matches!(e.expiry, Expiry::Unset));
        assert!(e.expiry.is_valid(now_after_year_3000()));
    }

    #[test]
    fn ttl_plus_added_resolves_to_absolute() {
        // 90 days after 2026-05-20 is 2026-08-18.
        let line = format!("{} ttl=90d added=2026-05-20", pk(4));
        let e = parse_line(&line);
        match e.expiry {
            Expiry::At(t) => assert_eq!(format_date_ymd_utc(t), "2026-08-18"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn expires_takes_precedence_over_ttl_added() {
        let line = format!("{} ttl=10d added=2026-05-20 expires=2027-01-01", pk(5));
        let e = parse_line(&line);
        match e.expiry {
            Expiry::At(t) => assert_eq!(format_date_ymd_utc(t), "2027-01-01"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn expired_entry_rejected_after_expiry() {
        let line = format!("{} expires=2020-01-01", pk(6));
        let e = parse_line(&line);
        // It's well past 2020. Use anything > 2020-01-01.
        let now_2026 = 1_747_699_200;
        assert!(!e.expiry.is_valid(now_2026));
    }

    #[test]
    fn build_line_appends_token_and_strips_existing() {
        let base = pk(7);
        let l1 = build_line(&base, Expiry::At(parse_date_ymd_utc("2027-01-01").unwrap()));
        assert!(l1.contains("expires=2027-01-01"), "got {l1}");
        // Re-applying replaces, doesn't duplicate.
        let l2 = build_line(&l1, Expiry::At(parse_date_ymd_utc("2028-06-15").unwrap()));
        assert!(l2.contains("expires=2028-06-15"));
        assert!(!l2.contains("2027-01-01"));
    }

    #[test]
    fn build_line_never_uses_explicit_sentinel() {
        let base = pk(8);
        let l = build_line(&base, Expiry::Never);
        assert!(l.contains("expires=never"));
        let back = parse_line(&l);
        assert!(matches!(back.expiry, Expiry::Never));
    }

    #[test]
    fn build_line_unset_drops_any_tokens() {
        let with_exp = format!("{} expires=2026-08-18", pk(9));
        let stripped = build_line(&with_exp, Expiry::Unset);
        assert!(!stripped.contains("expires="));
        // The pubkey body must survive.
        assert!(stripped.starts_with("ssh-ed25519"));
    }

    #[test]
    fn expiry_from_ttl_days_lands_on_midnight() {
        let t = expiry_from_ttl_days(1_747_700_000, 90);
        assert_eq!(t % SECS_PER_DAY, 0);
        assert!(t >= 1_747_700_000 + 90 * SECS_PER_DAY);
    }

    #[test]
    fn parse_file_preserves_blank_and_comment_lines() {
        let s = format!("# header\n\n{}\n# trailing\n", pk(10));
        let v = parse_file(&s);
        assert_eq!(v.len(), 4); // including the blank
        assert!(v[0].raw == "# header");
        assert!(v[1].raw.is_empty());
        assert!(v[2].is_key());
        assert!(v[3].raw == "# trailing");
    }

    fn now_after_year_3000() -> u64 {
        // 3000-01-01 in unix seconds — comfortably past any current dates.
        parse_date_ymd_utc("3000-01-01").unwrap()
    }
}
