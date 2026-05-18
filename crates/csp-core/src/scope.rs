//! Scope & coexistence (§11). The synced set is an **explicit allowlist**
//! (a subtree / include patterns) minus exclusions; the default failure mode
//! is syncing *too little*, never exfiltrating secrets. `.contextignore`
//! (synced, scope-relative) and `.context/exclude` (node-local, never
//! synced) layer under the allowlist. **HARD INVARIANT:** `.context/` is
//! unconditionally excluded — CSP never replicates, materializes, or commits
//! its own state. Text-only by default; binaries are opt-in (§11/§13.1).

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

/// Translate one gitignore-ish pattern to a regex over a `/`-separated
/// scope-relative path. Supports `#` comments, blank lines, leading `!`
/// negation, anchored (`/`) vs basename patterns, trailing `/` (directory),
/// `*` (not `/`), `?`, and `**` (across `/`). Returns `(negated, regex)`.
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
        // Basename pattern: may appear at any depth.
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
    // Match the entry itself and (for directories) anything beneath it.
    re.push_str("(?:/.*)?$");
    Some((negated, re))
}

/// Patterns are anchored both ends by construction (`^…$`); the `regex`
/// crate compiles to wasm32, keeping `scope` usable from the shared core.
fn regex_match(re: &str, text: &str) -> bool {
    regex::Regex::new(re).map(|r| r.is_match(text)).unwrap_or(false)
}

fn matches_any(globs: &[String], rel: &str) -> bool {
    // Last matching pattern wins (gitignore semantics); a negated pattern
    // un-ignores.
    let mut decision = false;
    for g in globs {
        if let Some((neg, re)) = pattern_to_regex(g) {
            if regex_match(&re, rel) {
                decision = !neg;
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
        let included = self.include.iter().any(|g| {
            pattern_to_regex(g)
                .map(|(_, re)| regex_match(&re, rel))
                .unwrap_or(false)
        });
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
}
