//! Scope & coexistence (§11). The synced set is an **explicit allowlist**
//! (a subtree / include patterns) minus exclusions; the default failure mode
//! is syncing *too little*, never exfiltrating secrets. `.contextignore`
//! (synced, scope-relative) and `.context/exclude` (node-local, never
//! synced) layer under the allowlist. **HARD INVARIANT:** `.context/` is
//! unconditionally excluded — CSP never replicates, materializes, or commits
//! its own state. Text-only by default; binaries are opt-in (§11/§13.1).
//!
//! Pattern matching is a **hand-rolled gitignore matcher** — deliberately
//! NOT the `regex` crate: `regex` is ~1 MB in wasm and was csp-core's single
//! largest size cost (one-engine-everywhere, §16, runs this in the plugin).
//! The matcher is byte-for-byte equivalent to the prior regex translation —
//! a differential property test (`tests::differential_vs_regex_oracle`)
//! asserts `glob == regex` over a large generated corpus, and the prior
//! regex translation is retained verbatim as the test-only oracle.

/// Engine-footprint dir name. Protocol-anchored & frozen (§9.1 naming
/// principle) — identical in every implementation.
pub const CONTEXT_DIR: &str = ".context";
pub const CONTEXTIGNORE: &str = ".contextignore";

#[derive(Clone, Debug)]
pub struct Scope {
    /// Allowlist include globs (scope-relative). Default: everything.
    pub include: Vec<String>,
    /// Exclusion globs from `.contextignore` (synced) + `.context/exclude`.
    pub ignore: Vec<String>,
    /// Binaries are opt-in; when false a non-text file is out of scope.
    pub allow_binary: bool,
}

impl Default for Scope {
    fn default() -> Self {
        Scope {
            include: vec!["**".into()],
            ignore: Vec::new(),
            allow_binary: false,
        }
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_err() || bytes.contains(&0)
}

/// One token of a compiled pattern body. `Star` = `[^/]*`, `DStar` = `.*`
/// (across `/`), `Any` = `[^/]`, `Lit` = a literal char (regex metachars are
/// matched literally, exactly as the prior `pattern_to_regex` escaped them).
enum Tok {
    Lit(char),
    Any,
    Star,
    DStar,
}

/// A compiled gitignore-ish pattern. Pure — no regex engine.
struct Glob {
    negated: bool,
    anchored: bool,
    toks: Vec<Tok>,
}

/// Compile one pattern. `None` for blank / `#` comment lines (skipped) —
/// identical handling to the prior `pattern_to_regex`. Behaviour is the
/// regex `^ (?:.*/)?[basename]  BODY  (?:/.*)?$` made explicit:
///  - leading `!` → negation; trailing `/` → directory (stripped, same as
///    before — match scope is path strings);
///  - a pattern containing `/` is *anchored* (no `(?:.*/)?` prefix); leading
///    `/` is stripped;
///  - `**` → `DStar` (and, exactly as the regex did, swallows one following
///    `/`); `*` → `Star`; `?` → `Any`; everything else is a literal.
fn compile(pat: &str) -> Option<Glob> {
    let mut p = pat.trim_end_matches('\r').to_string();
    if p.trim().is_empty() || p.trim_start().starts_with('#') {
        return None;
    }
    let negated = p.starts_with('!');
    if negated {
        p = p[1..].to_string();
    }
    let dir_only = p.ends_with('/');
    if dir_only {
        p.pop();
    }
    // NB: `anchored` is computed *after* the trailing-`/` pop and *before*
    // the leading-`/` trim — preserving the prior order exactly.
    let anchored = p.contains('/');
    let body = p.trim_start_matches('/');

    let chars: Vec<char> = body.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    toks.push(Tok::DStar);
                    i += 2;
                    if i < chars.len() && chars[i] == '/' {
                        i += 1;
                    }
                    continue;
                }
                toks.push(Tok::Star);
            }
            '?' => toks.push(Tok::Any),
            c => toks.push(Tok::Lit(c)),
        }
        i += 1;
    }
    Some(Glob { negated, anchored, toks })
}

/// Match `toks` against `t[si..]`. On full token consumption the regex
/// suffix `(?:/.*)?$` requires end-of-text *or* a `/` boundary — replicated
/// exactly here.
fn body_match(toks: &[Tok], t: &[char], si: usize) -> bool {
    match toks.first() {
        None => si == t.len() || t.get(si) == Some(&'/'),
        Some(Tok::Lit(c)) => {
            si < t.len() && t[si] == *c && body_match(&toks[1..], t, si + 1)
        }
        Some(Tok::Any) => {
            si < t.len() && t[si] != '/' && body_match(&toks[1..], t, si + 1)
        }
        Some(Tok::Star) => {
            // `[^/]*`: consume 0..k non-`/` chars (regex backtracking).
            let mut j = si;
            loop {
                if body_match(&toks[1..], t, j) {
                    return true;
                }
                if j < t.len() && t[j] != '/' {
                    j += 1;
                } else {
                    return false;
                }
            }
        }
        Some(Tok::DStar) => {
            // `.*`: consume 0..k of anything (incl. `/`).
            let mut j = si;
            loop {
                if body_match(&toks[1..], t, j) {
                    return true;
                }
                if j < t.len() {
                    j += 1;
                } else {
                    return false;
                }
            }
        }
    }
}

impl Glob {
    /// Equivalent to the prior regex `is_match`. For a basename (non-
    /// anchored) pattern the regex prefix `(?:.*/)?` means the body may
    /// start at offset 0 or immediately after any `/`.
    fn is_match(&self, text: &str) -> bool {
        let t: Vec<char> = text.chars().collect();
        if self.anchored {
            return body_match(&self.toks, &t, 0);
        }
        if body_match(&self.toks, &t, 0) {
            return true;
        }
        for i in 0..t.len() {
            if t[i] == '/' && body_match(&self.toks, &t, i + 1) {
                return true;
            }
        }
        false
    }
}

fn matches_any(globs: &[String], rel: &str) -> bool {
    // Last matching pattern wins (gitignore semantics); a negated pattern
    // un-ignores.
    let mut decision = false;
    for g in globs {
        if let Some(gl) = compile(g) {
            if gl.is_match(rel) {
                decision = !gl.negated;
            }
        }
    }
    decision
}

impl Scope {
    /// Is this scope-relative path eligible for sync? (path uses `/`.)
    pub fn path_in_scope(&self, rel: &str) -> bool {
        // HARD INVARIANT (§11): `.context/` is unconditionally excluded.
        if rel == CONTEXT_DIR || rel.starts_with(&format!("{CONTEXT_DIR}/")) {
            return false;
        }
        let included = self
            .include
            .iter()
            .any(|g| compile(g).map(|gl| gl.is_match(rel)).unwrap_or(false));
        if !included {
            return false;
        }
        !matches_any(&self.ignore, rel)
    }

    /// Full check including the text-only-by-default rule (§11): a non-text
    /// file is out of scope unless binaries are explicitly opted in.
    pub fn content_in_scope(&self, rel: &str, content: &[u8]) -> bool {
        if !self.path_in_scope(rel) {
            return false;
        }
        if !self.allow_binary && is_binary(content) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Test-only oracle: the PRIOR regex translation, verbatim. The
    // production matcher above must agree with this for every input. `regex`
    // is a dev-dependency only (kept out of the shipped wasm). ----

    fn pattern_to_regex(pat: &str) -> Option<(bool, String)> {
        let mut p = pat.trim_end_matches('\r').to_string();
        if p.trim().is_empty() || p.trim_start().starts_with('#') {
            return None;
        }
        let negated = p.starts_with('!');
        if negated {
            p = p[1..].to_string();
        }
        let dir_only = p.ends_with('/');
        if dir_only {
            p.pop();
        }
        let anchored = p.contains('/');
        let p = p.trim_start_matches('/');

        let mut re = String::from("^");
        if !anchored {
            re.push_str("(?:.*/)?");
        }
        let chars: Vec<char> = p.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '*' => {
                    if i + 1 < chars.len() && chars[i + 1] == '*' {
                        re.push_str(".*");
                        i += 2;
                        if i < chars.len() && chars[i] == '/' {
                            i += 1;
                        }
                        continue;
                    }
                    re.push_str("[^/]*");
                }
                '?' => re.push_str("[^/]"),
                c @ ('.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\') => {
                    re.push('\\');
                    re.push(c);
                }
                c => re.push(c),
            }
            i += 1;
        }
        re.push_str("(?:/.*)?$");
        Some((negated, re))
    }

    fn oracle(pat: &str, text: &str) -> Option<(bool, bool)> {
        pattern_to_regex(pat).map(|(neg, re)| {
            (neg, regex::Regex::new(&re).map(|r| r.is_match(text)).unwrap_or(false))
        })
    }

    fn ours(pat: &str, text: &str) -> Option<(bool, bool)> {
        compile(pat).map(|g| (g.negated, g.is_match(text)))
    }

    #[test]
    fn differential_vs_regex_oracle() {
        // Pattern + path vocabulary chosen to exercise every construct: `*`,
        // `**` (leading/mid/trailing), `?`, literals, regex-metachar
        // literals (`.`/`+`/`(`/`[`), `!` negation, anchored vs basename,
        // trailing-`/` dir, leading-`/`, `#`/blank skips, multi-segment.
        let frags = [
            "*", "**", "?", "a", "ab", "x.log", "*.log", "keep.log", "tmp",
            "build", "secret.txt", "a*b", "?x", "a.b+c", "a(b)", "[x]", "a\\b",
            "sub/*", "a/**/b", "**/x", "x/**", "/secret.txt", "a/b", "**/",
            "build/", "a/**", "*/y", "a?c", "", "   ", "# comment", "a**b",
        ];
        let prefixes = ["", "!", "/"];
        let suffixes = ["", "/"];
        let segs = ["a", "b", "x.log", "keep.log", "tmp", "build", "secret.txt", "y", "ab", "x"];

        // Paths: depth 0..=3 over the segment alphabet.
        let mut paths: Vec<String> = Vec::new();
        for s in segs {
            paths.push(s.to_string());
        }
        for a in segs {
            for b in segs {
                paths.push(format!("{a}/{b}"));
            }
        }
        for a in ["a", "b", "sub", "x"] {
            for b in ["a", "b", "x.log", "tmp"] {
                for c in ["c.md", "y", "secret.txt", "x.log"] {
                    paths.push(format!("{a}/{b}/{c}"));
                }
            }
        }
        paths.push(String::new());
        paths.push("a/".into());
        paths.push("/a".into());

        let mut checked = 0u64;
        for pre in prefixes {
            for f in frags {
                for suf in suffixes {
                    let pat = format!("{pre}{f}{suf}");
                    for path in &paths {
                        let o = oracle(&pat, path);
                        let m = ours(&pat, path);
                        assert_eq!(
                            o, m,
                            "MISMATCH pattern={pat:?} path={path:?}: regex-oracle={o:?} hand-rolled={m:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        // Sanity: we actually exercised a large corpus.
        assert!(checked > 30_000, "only checked {checked} cases");
    }

    // ---- Behavioural tests (unchanged; pin the engine-level semantics) ----

    #[test]
    fn context_dir_is_hard_excluded() {
        let s = Scope::default();
        assert!(!s.path_in_scope(".context/state"));
        assert!(!s.path_in_scope(".context"));
        assert!(s.path_in_scope("notes.md"));
        assert!(s.path_in_scope("a/b/c.md"));
    }

    #[test]
    fn contextignore_excludes_and_negates() {
        let s = Scope {
            include: vec!["**".into()],
            ignore: vec!["*.log".into(), "build/".into(), "!keep.log".into()],
            allow_binary: false,
        };
        assert!(!s.path_in_scope("debug.log"));
        assert!(!s.path_in_scope("sub/trace.log"));
        assert!(!s.path_in_scope("build/out.txt"));
        assert!(s.path_in_scope("keep.log"));
        assert!(s.path_in_scope("src/main.rs"));
    }

    #[test]
    fn binary_is_opt_in() {
        let s = Scope::default();
        assert!(s.content_in_scope("a.md", b"hello"));
        assert!(!s.content_in_scope("a.bin", &[0u8, 1, 2, 255]));
        let s2 = Scope { allow_binary: true, ..Scope::default() };
        assert!(s2.content_in_scope("a.bin", &[0u8, 1, 2, 255]));
    }

    #[test]
    fn anchored_vs_basename() {
        let s = Scope {
            include: vec!["**".into()],
            ignore: vec!["/secret.txt".into(), "tmp".into()],
            allow_binary: false,
        };
        assert!(!s.path_in_scope("secret.txt"));
        assert!(s.path_in_scope("sub/secret.txt")); // anchored, only root
        assert!(!s.path_in_scope("tmp")); // basename anywhere
        assert!(!s.path_in_scope("a/tmp"));
    }

    // Extra edge cases the prior suite didn't pin, now explicit.
    #[test]
    fn glob_edge_cases() {
        let g = |p: &str, t: &str| compile(p).map(|x| x.is_match(t));
        assert_eq!(g("a/**/b", "a/x/y/b"), Some(true));
        assert_eq!(g("a/**/b", "a/b"), Some(true)); // `**` swallows the `/`
        assert_eq!(g("**/x", "deep/dir/x"), Some(true));
        assert_eq!(g("x/**", "x/a/b"), Some(true));
        assert_eq!(g("x/**", "x"), Some(false)); // needs the `/`
        assert_eq!(g("a.b", "a.b"), Some(true)); // `.` is literal
        assert_eq!(g("a.b", "axb"), Some(false));
        assert_eq!(g("a+b", "a+b"), Some(true)); // `+` is literal
        assert_eq!(g("# c", "x"), None); // comment skipped
        assert_eq!(g("   ", "x"), None); // blank skipped
        assert_eq!(g("?", "ab"), Some(false)); // single non-`/`
        assert_eq!(g("?", "a"), Some(true));
    }
}
