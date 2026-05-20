//! `<scope>/.context/config` — tool/launcher-anchored vault config (§9.1).
//!
//! The config *model* + its TOML (de)serialization is always-on / wasm-safe
//! (a plugin runs the identical engine and shares the exact same
//! `.context/config` bytes with `ctx`, §9.1). Only the on-disk file
//! read/write is native (`cfg`-gated).
//!
//! The codec is **hand-rolled** — deliberately NOT the `toml` crate, which
//! drags `toml_edit` + `winnow` (~460 KB in wasm; §16 one engine
//! everywhere). `VaultConfig` is a flat table of string / bool /
//! `Vec<String>` / `Option<String>` keys, so the needed TOML is tiny.
//!
//! Contract (proven by `tests::differential_vs_toml` against the real
//! `toml` crate, kept dev-only). Byte-identical formatting is explicitly
//! NOT a goal — it would couple us to `toml_edit`'s version-specific
//! literal-vs-basic-string + array heuristics, which aren't TOML-spec and
//! aren't a requirement (one core: `ctx` and the SDK run *this* codec; no
//! backward-compat needed, pre-release). What we guarantee:
//!  1. **Round-trip:** `from(to(cfg)) == cfg` for all configs.
//!  2. **Valid TOML:** the real `toml` crate parses our output back to the
//!     same value (so `ctx git` / a human / any external tool stays happy).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultConfig {
    /// Opaque protocol identity (a UUID by default). Both replicas must
    /// agree — it is the handshake's "same vault?" guard, not a label.
    pub vault_id: String,
    /// Optional human label (derived from the init directory, may be empty).
    /// Travels in config + `Hello` for display / clone-folder naming; never
    /// a uniqueness guarantee — that is `vault_id`.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub peers: Vec<String>,
    #[serde(default)]
    pub listen: Option<String>,
    #[serde(default)]
    pub no_tofu: bool,
    /// Serve a plaintext ws:// listener instead of the default self-signed
    /// wss://.
    #[serde(default)]
    pub no_tls: bool,
    /// Log level / filter; the launcher's built-in default applies when None.
    #[serde(default)]
    pub log: Option<String>,
    /// Auto-commit debounce, in milliseconds.
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default)]
    pub allow_binary: bool,
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    /// Auth-key enrollment secrets (§10). Comma-separated form on the CLI;
    /// stored as a list. When non-empty, TOFU is implicitly disabled.
    #[serde(default)]
    pub auth_keys: Vec<String>,
    /// Default `expires=` TTL applied to new `authorized_keys` entries
    /// (enrollment, manual `ctx authorize`, listen-start migration). Days.
    /// `None` → built-in 90-day default. Set to `0` for "no default TTL"
    /// (entries are written without `expires=`, treated as non-expiring
    /// until manually edited).
    #[serde(default)]
    pub default_key_ttl_days: Option<u64>,
}

fn default_debounce_ms() -> u64 {
    1000
}
fn default_include() -> Vec<String> {
    vec!["**".into()]
}

/// The built-in default TTL applied when `default_key_ttl_days` is `None`.
/// Spec §10: 90 days. `Some(0)` opts out (writes entries without `expires=`).
pub const BUILTIN_DEFAULT_TTL_DAYS: u64 = 90;

/// TOML basic-string escaping — matches the `toml` crate exactly: `\` `"`
/// `\n` `\t` `\r` get short escapes, other controls (< 0x20) `\u00XX`.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04X}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Unescape a TOML basic string body (between the quotes).
fn unesc(s: &str) -> Result<String, String> {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('n') => o.push('\n'),
            Some('t') => o.push('\t'),
            Some('r') => o.push('\r'),
            Some('"') => o.push('"'),
            Some('\\') => o.push('\\'),
            Some('b') => o.push('\u{08}'),
            Some('f') => o.push('\u{0C}'),
            Some('u') => {
                let hex: String = it.by_ref().take(4).collect();
                let n = u32::from_str_radix(&hex, 16).map_err(|_| "bad \\u escape")?;
                o.push(char::from_u32(n).ok_or("bad \\u scalar")?);
            }
            Some('U') => {
                let hex: String = it.by_ref().take(8).collect();
                let n = u32::from_str_radix(&hex, 16).map_err(|_| "bad \\U escape")?;
                o.push(char::from_u32(n).ok_or("bad \\U scalar")?);
            }
            Some(o2) => return Err(format!("invalid escape \\{o2}")),
            None => return Err("trailing backslash".into()),
        }
    }
    Ok(o)
}

fn fmt_str(key: &str, v: &str) -> String {
    format!("{key} = \"{}\"\n", esc(v))
}

/// `toml::to_string_pretty` array shape (toml 0.8): empty → `[]`; exactly
/// one element → inline `["x"]`; two or more → multi-line, 4-space indent,
/// trailing comma, closing `]` at column 0.
fn fmt_arr(key: &str, v: &[String]) -> String {
    match v.len() {
        0 => format!("{key} = []\n"),
        1 => format!("{key} = [\"{}\"]\n", esc(&v[0])),
        _ => {
            let mut s = format!("{key} = [\n");
            for e in v {
                s.push_str(&format!("    \"{}\",\n", esc(e)));
            }
            s.push_str("]\n");
            s
        }
    }
}

impl VaultConfig {
    /// Host-managed persistence. Emits clean, valid TOML (basic strings;
    /// field order = struct order; `listen` omitted when `None`, matching
    /// serde/toml semantics). Not byte-identical to `toml::to_string_pretty`
    /// by design — see the module contract — but the real `toml` parser
    /// reads it back identically (test guarantee #2), and `ctx` + the SDK
    /// share *this* codec (one core, §9.1).
    pub fn to_toml_string(&self) -> crate::error::CspResult<String> {
        let mut s = String::new();
        s.push_str(&fmt_str("vault_id", &self.vault_id));
        s.push_str(&fmt_str("name", &self.name));
        s.push_str(&fmt_arr("peers", &self.peers));
        if let Some(l) = &self.listen {
            s.push_str(&fmt_str("listen", l));
        }
        s.push_str(&format!("no_tofu = {}\n", self.no_tofu));
        s.push_str(&format!("no_tls = {}\n", self.no_tls));
        if let Some(l) = &self.log {
            s.push_str(&fmt_str("log", l));
        }
        s.push_str(&format!("debounce_ms = {}\n", self.debounce_ms));
        s.push_str(&format!("allow_binary = {}\n", self.allow_binary));
        s.push_str(&fmt_arr("include", &self.include));
        // `auth_keys` is omitted entirely when empty (matches serde/toml
        // semantics for a defaulted empty vec) so existing configs round-
        // trip unchanged.
        if !self.auth_keys.is_empty() {
            s.push_str(&fmt_arr("auth_keys", &self.auth_keys));
        }
        if let Some(d) = self.default_key_ttl_days {
            s.push_str(&format!("default_key_ttl_days = {d}\n"));
        }
        Ok(s)
    }

    /// Tolerant flat-table parser: accepts `toml::to_string_pretty` output
    /// (multi-line arrays) and reasonable hand-edited variants (inline
    /// arrays, `#` comments, blank lines, CRLF, an ignored leading `[table]`
    /// header). Applies the same defaults serde did; `vault_id` is required.
    pub fn from_toml_str(s: &str) -> crate::error::CspResult<VaultConfig> {
        use crate::error::CspError::Config as C;
        let mut vault_id: Option<String> = None;
        let mut name = String::new();
        let mut peers: Vec<String> = Vec::new();
        let mut listen: Option<String> = None;
        let mut no_tofu = false;
        let mut no_tls = false;
        let mut log: Option<String> = None;
        let mut debounce_ms = default_debounce_ms();
        let mut allow_binary = false;
        let mut include: Option<Vec<String>> = None;
        let mut auth_keys: Vec<String> = Vec::new();
        let mut default_key_ttl_days: Option<u64> = None;

        let bytes = s.replace("\r\n", "\n");
        let mut lines = bytes.lines().peekable();
        while let Some(raw) = lines.next() {
            let line = strip_comment(raw).trim();
            if line.is_empty() || (line.starts_with('[') && line.ends_with(']')) {
                continue; // blank, comment, or an ignored table header
            }
            let eq = line.find('=').ok_or_else(|| C(format!("malformed: {raw}")))?;
            let key = line[..eq].trim();
            let mut rhs = line[eq + 1..].trim().to_string();
            // Multi-line array: accumulate continuation lines until the `[`
            // is balanced by its `]` *at array depth 0, outside strings* —
            // so a `]` inside an element (e.g. a gitignore `[ab]` class in
            // an `include` glob) doesn't truncate it early.
            if rhs.starts_with('[') && !arr_complete(&rhs) {
                while !arr_complete(&rhs) {
                    let nxt = lines.next().ok_or_else(|| C("unterminated array".into()))?;
                    rhs.push(' ');
                    rhs.push_str(strip_comment(nxt).trim());
                }
            }
            match key {
                "vault_id" => vault_id = Some(parse_str(&rhs).map_err(C)?),
                "name" => name = parse_str(&rhs).map_err(C)?,
                "peers" => peers = parse_arr(&rhs).map_err(C)?,
                "listen" => listen = Some(parse_str(&rhs).map_err(C)?),
                "no_tofu" => no_tofu = rhs == "true",
                "no_tls" => no_tls = rhs == "true",
                "log" => log = Some(parse_str(&rhs).map_err(C)?),
                "debounce_ms" => {
                    debounce_ms =
                        rhs.parse().map_err(|_| C(format!("bad debounce_ms: {rhs}")))?
                }
                "allow_binary" => allow_binary = rhs == "true",
                "include" => include = Some(parse_arr(&rhs).map_err(C)?),
                "auth_keys" => auth_keys = parse_arr(&rhs).map_err(C)?,
                "default_key_ttl_days" => {
                    default_key_ttl_days = Some(
                        rhs.parse()
                            .map_err(|_| C(format!("bad default_key_ttl_days: {rhs}")))?,
                    )
                }
                _ => {} // unknown key tolerated (forward-compat)
            }
        }
        Ok(VaultConfig {
            vault_id: vault_id.ok_or_else(|| C("missing field `vault_id`".into()))?,
            name,
            peers,
            listen,
            no_tofu,
            no_tls,
            log,
            debounce_ms,
            allow_binary,
            include: include.unwrap_or_else(default_include),
            auth_keys,
            default_key_ttl_days,
        })
    }
}

/// Drop a trailing `# comment` not inside a double-quoted string.
fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' if i == 0 || b[i - 1] != b'\\' => in_str = !in_str,
            b'#' if !in_str => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// A TOML basic-string scalar `"…"` (the only string form this codec
/// emits). Escapes are processed.
fn parse_str(rhs: &str) -> Result<String, String> {
    let r = rhs.trim();
    if r.len() >= 2 && r.starts_with('"') && r.ends_with('"') {
        unesc(&r[1..r.len() - 1])
    } else {
        Err(format!("expected quoted string, got: {rhs}"))
    }
}

/// True once `s` contains a complete `[ … ]` array literal: every `[` is
/// matched by a `]` counted at depth 0 with string bodies skipped (same
/// `\X`-aware scan as `parse_arr`). Drives multi-line accumulation.
fn arr_complete(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut depth: i32 = 0;
    let mut started = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                i += 1;
                loop {
                    if i >= chars.len() {
                        return false; // string not yet closed → need more lines
                    }
                    match chars[i] {
                        '\\' => i += 2,
                        '"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                continue;
            }
            '[' => {
                depth += 1;
                started = true;
            }
            ']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    started && depth <= 0
}

fn parse_arr(rhs: &str) -> Result<Vec<String>, String> {
    let r = rhs.trim();
    let inner = r
        .strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .ok_or_else(|| format!("expected array, got: {rhs}"))?;
    let mut out = Vec::new();
    let chars: Vec<char> = inner.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '"' {
            i += 1; // whitespace / commas / newlines between elements
            continue;
        }
        // Basic string: find the terminating quote, skipping `\X` escape
        // pairs (so `\\` and `\"` don't confuse the scan — a quote after an
        // *even* run of backslashes is the real terminator).
        let start = i + 1;
        let mut j = start;
        loop {
            if j >= chars.len() {
                return Err("unterminated string in array".into());
            }
            match chars[j] {
                '\\' => j += 2, // skip the escape pair
                '"' => break,
                _ => j += 1,
            }
        }
        out.push(unesc(&chars[start..j].iter().collect::<String>())?);
        i = j + 1;
    }
    Ok(out)
}

// ---- Native on-disk persistence (the `ctx` full node) ----------------------

#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
mod disk {
    use super::VaultConfig;
    use crate::error::{CspError, CspResult};
    use std::path::{Path, PathBuf};

    impl VaultConfig {
        fn path(context: &Path) -> PathBuf {
            context.join("config")
        }
        pub fn load(context: &Path) -> CspResult<VaultConfig> {
            let s = std::fs::read_to_string(Self::path(context))
                .map_err(|e| CspError::Config(format!("read config: {e}")))?;
            VaultConfig::from_toml_str(&s)
        }
        pub fn save(&self, context: &Path) -> CspResult<()> {
            std::fs::write(Self::path(context), self.to_toml_string()?)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn cfg(
        vault_id: &str,
        name: &str,
        peers: &[&str],
        listen: Option<&str>,
        no_tofu: bool,
        no_tls: bool,
        log: Option<&str>,
        debounce_ms: u64,
        allow_binary: bool,
        include: &[&str],
    ) -> VaultConfig {
        VaultConfig {
            vault_id: vault_id.into(),
            name: name.into(),
            peers: peers.iter().map(|s| s.to_string()).collect(),
            listen: listen.map(|s| s.to_string()),
            no_tofu,
            no_tls,
            log: log.map(|s| s.to_string()),
            debounce_ms,
            allow_binary,
            include: include.iter().map(|s| s.to_string()).collect(),
            auth_keys: Vec::new(),
            default_key_ttl_days: None,
        }
    }

    /// The corpus: every field varied incl. escape-relevant strings, empty
    /// vs multi-element arrays, listen/log None/Some, non-default bools and
    /// debounce.
    fn corpus() -> Vec<VaultConfig> {
        let strs = [
            "v-1",
            "3ba23523-b267-447d-b842-e037fa12fed7",
            "",
            "with space",
            "quote\"inside",
            "back\\slash",
            "tab\there",
            "new\nline",
            "ünïcøde-✓",
            "wss://host:7777/path",
            "a#b", // not a comment inside a string
        ];
        let arrs: [&[&str]; 6] = [
            &[],
            &["**"],
            &["md", "markdown"],
            &["a", "b", "c", "d"],
            &["pat with \"q\"", "back\\\\", "tab\tx"],
            // Brackets inside multi-line string elements: a `]` here must NOT
            // truncate accumulation (gitignore char-class globs are real).
            &["src/[ab]/**", "x]y", "[only", "]z["],
        ];
        let mut v = Vec::new();
        for s in strs {
            for a in arrs {
                v.push(cfg(s, s, a, None, false, false, None, 1000, false, a));
                v.push(cfg(s, "", &["x"], Some(s), true, true, Some(s), 250, true, a));
            }
        }
        v.push(cfg(
            "id", "n", &["p1", "p2"], Some("ls"), true, true, Some("debug"), 500, false, &["**"],
        ));
        v
    }

    #[test]
    fn differential_vs_toml() {
        for c in corpus() {
            let mine = c.to_toml_string().unwrap();

            // 1. Round-trip through the hand-rolled pair.
            assert_eq!(VaultConfig::from_toml_str(&mine).unwrap(), c, "rt {c:?}");

            // 2. Valid TOML: the real `toml` parser reads our output back to
            //    the same value (`ctx git` / humans / external tools).
            let via_toml: VaultConfig = toml::from_str(&mine).unwrap();
            assert_eq!(via_toml, c, "toml-reads-ours {c:?}");
        }
    }

    #[test]
    fn auth_keys_and_default_ttl_round_trip() {
        let mut c = cfg("id", "n", &[], None, false, false, None, 1000, false, &["**"]);
        c.auth_keys = vec!["s3kr1t".into(), "rotation-key".into()];
        c.default_key_ttl_days = Some(30);
        let out = c.to_toml_string().unwrap();
        assert!(out.contains("auth_keys = [\n"));
        assert!(out.contains("default_key_ttl_days = 30"));
        assert_eq!(VaultConfig::from_toml_str(&out).unwrap(), c);
        // Real toml crate parses it identically.
        let via: VaultConfig = toml::from_str(&out).unwrap();
        assert_eq!(via, c);
        // Defaults: empty auth_keys is omitted; None ttl is absent.
        let bare = cfg("id", "n", &[], None, false, false, None, 1000, false, &["**"]);
        let out = bare.to_toml_string().unwrap();
        assert!(!out.contains("auth_keys"));
        assert!(!out.contains("default_key_ttl_days"));
    }

    #[test]
    fn defaults_and_missing_required() {
        // Minimal file → serde-equivalent defaults.
        let c = VaultConfig::from_toml_str("vault_id = \"x\"\n").unwrap();
        assert_eq!(c.debounce_ms, 1000);
        assert!(!c.no_tls && c.log.is_none());
        assert_eq!(c.include, vec!["**".to_string()]);
        assert!(c.name.is_empty() && c.peers.is_empty() && c.listen.is_none());
        // Missing required `vault_id` errors (like serde).
        assert!(VaultConfig::from_toml_str("name = \"x\"\n").is_err());
        // Comments / blanks / CRLF / ignored table header tolerated.
        let c2 = VaultConfig::from_toml_str(
            "# hello\r\n\r\n[ignored]\r\nvault_id = \"y\" # trailing\r\nno_tls = true\r\n",
        )
        .unwrap();
        assert_eq!((c2.vault_id.as_str(), c2.no_tls), ("y", true));
    }
}
