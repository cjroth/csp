//! Real 3-way merge (§5.3). Disjoint regions both survive; an overlapping
//! hunk resolves to **theirs** — which, in every binary fold step, is the
//! operand strictly later in the total order (§5.3, §5.1). **No conflict
//! markers, ever** (§3). Binary / non-mergeable files fall back to
//! whole-file total-order selection (theirs).
//!
//! `theirs` is always `Tₖ`; `ours` is always `accₖ₋₁`. Because the fold sorts
//! the frontier ascending and folds left, `Tₖ` is later in the strict total
//! order than everything already in `accₖ₋₁`, so "theirs wins" *is* the
//! total-order tiebreak (§5.4 part 3).

use std::collections::BTreeMap;

type Lines<'a> = Vec<&'a str>;

fn is_binary(b: &[u8]) -> bool {
    std::str::from_utf8(b).is_err() || b.contains(&0)
}

fn split_lines(s: &str) -> Lines<'_> {
    // Keep terminators so the join is byte-exact.
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'\n' {
            out.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Equal-line anchor pairs `(base_idx, x_idx)` from an LCS diff. `old` = base,
/// `new` = the other side.
fn equal_pairs(base: &[&str], x: &[&str]) -> Vec<(usize, usize)> {
    use similar::{capture_diff_slices, Algorithm, DiffOp};
    let ops = capture_diff_slices(Algorithm::Myers, base, x);
    let mut pairs = Vec::new();
    for op in ops {
        if let DiffOp::Equal { old_index, new_index, len } = op {
            for k in 0..len {
                pairs.push((old_index + k, new_index + k));
            }
        }
    }
    pairs
}

/// Classic deterministic diff3. Anchors are base lines that are equal-matched
/// in *both* base↔ours and base↔theirs (so all three agree there); chunks
/// between anchors are resolved with the theirs-wins rule. No markers.
fn diff3(base: &str, ours: &str, theirs: &str) -> String {
    let b = split_lines(base);
    let o = split_lines(ours);
    let t = split_lines(theirs);

    let ma: BTreeMap<usize, usize> = equal_pairs(&b, &o).into_iter().collect();
    let mb: BTreeMap<usize, usize> = equal_pairs(&b, &t).into_iter().collect();

    // Anchors: base indices stable in both, ascending. (base_i, o_i, t_i)
    let mut anchors: Vec<(usize, usize, usize)> = Vec::new();
    for (&bi, &oi) in &ma {
        if let Some(&ti) = mb.get(&bi) {
            anchors.push((bi, oi, ti));
        }
    }
    anchors.sort_unstable();

    let mut out = String::new();
    // Virtual head anchor; iterate chunk-by-chunk, then the tail chunk.
    let (mut pb, mut po, mut pt) = (0isize, 0isize, 0isize); // next index to consume
    let push_chunk = |out: &mut String,
                      bc: &[&str],
                      oc: &[&str],
                      tc: &[&str]| {
        let bs: String = bc.concat();
        let os: String = oc.concat();
        let ts: String = tc.concat();
        if os == ts {
            out.push_str(&os);
        } else if os == bs {
            out.push_str(&ts); // ours unchanged → take theirs (incl. deletion)
        } else if ts == bs {
            out.push_str(&os); // theirs unchanged → take ours
        } else {
            out.push_str(&ts); // true conflict → theirs wins (§5.3), no markers
        }
    };

    for &(bi, oi, ti) in &anchors {
        let bc = &b[pb as usize..bi];
        let oc = &o[po as usize..oi];
        let tc = &t[pt as usize..ti];
        push_chunk(&mut out, bc, oc, tc);
        out.push_str(b[bi]); // the agreed anchor line
        pb = bi as isize + 1;
        po = oi as isize + 1;
        pt = ti as isize + 1;
    }
    // Tail chunk after the last anchor.
    let bc = &b[pb as usize..];
    let oc = &o[po as usize..];
    let tc = &t[pt as usize..];
    push_chunk(&mut out, bc, oc, tc);
    out
}

/// 3-way merge of a single file's bytes. Binary / non-utf8 → whole-file
/// theirs (total-order selection, §5.3/§11).
pub fn merge_blob(base: &[u8], ours: &[u8], theirs: &[u8]) -> Vec<u8> {
    if is_binary(base) || is_binary(ours) || is_binary(theirs) {
        return theirs.to_vec();
    }
    let merged = diff3(
        std::str::from_utf8(base).unwrap(),
        std::str::from_utf8(ours).unwrap(),
        std::str::from_utf8(theirs).unwrap(),
    );
    merged.into_bytes()
}

/// 3-way merge of two working trees against their base, expressed as flat
/// `path -> bytes` maps. Disjoint paths both survive; a contended path is
/// resolved deterministically by the theirs-wins rule (§5.3, §12).
pub fn merge_trees(
    base: &BTreeMap<String, Vec<u8>>,
    ours: &BTreeMap<String, Vec<u8>>,
    theirs: &BTreeMap<String, Vec<u8>>,
) -> BTreeMap<String, Vec<u8>> {
    let mut paths: Vec<&String> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .collect();
    paths.sort_unstable();
    paths.dedup();

    let mut out = BTreeMap::new();
    for p in paths {
        let b = base.get(p);
        let o = ours.get(p);
        let t = theirs.get(p);
        let chosen: Option<Vec<u8>> = if o == t {
            o.cloned()
        } else if o == b {
            t.cloned() // ours unchanged → take theirs (incl. delete)
        } else if t == b {
            o.cloned() // theirs unchanged → take ours
        } else {
            // Both diverged from base.
            match (b, o, t) {
                (Some(bb), Some(oo), Some(tt)) => Some(merge_blob(bb, oo, tt)),
                // add/add (no base) different, or delete-vs-modify: theirs wins.
                (_, _, t) => t.cloned(),
            }
        };
        if let Some(c) = chosen {
            out.insert(p.clone(), c);
        }
    }
    out
}
