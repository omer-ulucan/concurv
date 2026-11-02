//! Line-level three-way merge of text files (R2.5, ADR 0006:
//! `docs/adr/0006-hunk-merge.md`).
//!
//! Conservative diff3: each side's changes against the base are grouped into
//! hunks; hunks from the two sides that overlap **or touch** (no unchanged
//! base line between them) are a conflict unless they are identical. Lines
//! keep their terminators, so CRLF and a missing final newline are merged
//! byte-exactly. Anything that is not UTF-8 text without NUL, or is larger
//! than [`MAX_MERGE_BYTES`], is not merged (the caller keeps the conflict).

use similar::{Algorithm, DiffTag, capture_diff_slices};

/// Largest input (each side) merged by content.
pub const MAX_MERGE_BYTES: usize = 8 << 20;

/// One side's replacement of base lines `start..end`.
#[derive(Debug, PartialEq, Eq)]
struct Hunk<'a> {
    start: usize,
    end: usize,
    lines: Vec<&'a str>,
}

fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes.len() > MAX_MERGE_BYTES || bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

fn lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

fn hunks<'a>(base: &[&'a str], side: &[&'a str]) -> Vec<Hunk<'a>> {
    let mut out: Vec<Hunk<'a>> = Vec::new();
    let mut cur: Option<Hunk<'a>> = None;
    for op in capture_diff_slices(Algorithm::Myers, base, side) {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            out.extend(cur.take());
            continue;
        }
        let h = cur.get_or_insert_with(|| Hunk {
            start: old.start,
            end: old.start,
            lines: Vec::new(),
        });
        h.end = old.end;
        h.lines.extend_from_slice(&side[new]);
    }
    out.extend(cur);
    out
}

/// True if two hunks overlap or touch in the base.
fn clash(a: &Hunk<'_>, b: &Hunk<'_>) -> bool {
    a.start <= b.end && b.start <= a.end
}

/// Merge `ours` and `theirs` relative to `base`. `None` if any input is not
/// mergeable text or the changes clash; never a partial or guessed result.
pub fn merge3(base: &[u8], ours: &[u8], theirs: &[u8]) -> Option<Vec<u8>> {
    let (b, o, t) = (as_text(base)?, as_text(ours)?, as_text(theirs)?);
    let (bl, ol, tl) = (lines(b), lines(o), lines(t));
    let (oh, th) = (hunks(&bl, &ol), hunks(&bl, &tl));
    let mut out: Vec<&str> = Vec::with_capacity(bl.len().max(ol.len()).max(tl.len()));
    let (mut i, mut j, mut pos) = (0, 0, 0);
    loop {
        let next = match (oh.get(i), th.get(j)) {
            (None, None) => break,
            (Some(x), Some(y)) if clash(x, y) => {
                if x != y {
                    return None;
                }
                i += 1;
                j += 1;
                x
            }
            (Some(x), Some(y)) if x.start <= y.start => {
                i += 1;
                x
            }
            (Some(_), Some(y)) | (None, Some(y)) => {
                j += 1;
                y
            }
            (Some(x), None) => {
                i += 1;
                x
            }
        };
        out.extend_from_slice(bl.get(pos..next.start)?);
        out.extend_from_slice(&next.lines);
        pos = next.end;
    }
    out.extend_from_slice(bl.get(pos..)?);
    Some(out.concat().into_bytes())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use proptest::prelude::*;

    fn m(b: &str, o: &str, t: &str) -> Option<String> {
        merge3(b.as_bytes(), o.as_bytes(), t.as_bytes()).map(|v| String::from_utf8(v).unwrap())
    }

    const BASE: &str = "1\n2\n3\n4\n5\n6\n7\n";

    #[test]
    fn disjoint_edits_merge() {
        assert_eq!(
            m(BASE, "1\nTWO\n3\n4\n5\n6\n7\n", "1\n2\n3\n4\n5\nSIX\n7\n").as_deref(),
            Some("1\nTWO\n3\n4\n5\nSIX\n7\n")
        );
    }

    #[test]
    fn insertions_and_deletions_merge() {
        assert_eq!(
            m(BASE, "0\n1\n2\n3\n4\n5\n6\n7\n", "1\n2\n3\n4\n5\n6\n").as_deref(),
            Some("0\n1\n2\n3\n4\n5\n6\n")
        );
    }

    #[test]
    fn overlapping_edits_conflict() {
        assert_eq!(
            m(BASE, "1\nA\n3\n4\n5\n6\n7\n", "1\nB\n3\n4\n5\n6\n7\n"),
            None
        );
    }

    #[test]
    fn adjacent_edits_conflict() {
        // Lines 2 and 3 changed on different sides with nothing between.
        assert_eq!(
            m(BASE, "1\nA\n3\n4\n5\n6\n7\n", "1\n2\nB\n4\n5\n6\n7\n"),
            None
        );
        // Two insertions at the same point.
        assert_eq!(m("a\nb\n", "a\nX\nb\n", "a\nY\nb\n"), None);
    }

    #[test]
    fn identical_hunks_are_taken_once() {
        assert_eq!(
            m(BASE, "1\nX\n3\n4\n5\n6\n7\n", "1\nX\n3\n4\n5\n6\nSEVEN\n").as_deref(),
            Some("1\nX\n3\n4\n5\n6\nSEVEN\n")
        );
    }

    #[test]
    fn line_endings_and_final_newline_are_preserved() {
        let b = "a\r\nb\r\nc\r\nd\r\ne";
        assert_eq!(
            m(b, "A\r\nb\r\nc\r\nd\r\ne", "a\r\nb\r\nc\r\nd\r\nE").as_deref(),
            Some("A\r\nb\r\nc\r\nd\r\nE")
        );
        // Adding a final newline on one side, an edit far away on the other.
        assert_eq!(
            m("a\nb\nc\nd\ne", "A\nb\nc\nd\ne", "a\nb\nc\nd\ne\n").as_deref(),
            Some("A\nb\nc\nd\ne\n")
        );
        // A CRLF→LF change on a line the other side edits: conflict.
        assert_eq!(m("a\r\nb\r\n", "a\nb\r\n", "x\r\nb\r\n"), None);
    }

    #[test]
    fn binary_and_invalid_utf8_are_not_merged() {
        assert_eq!(merge3(b"a\0\n", b"b\0\n", b"a\0\nc\n"), None);
        assert_eq!(merge3(b"\xff\n", b"\xff\nx\n", b"y\n\xff\n"), None);
    }

    fn arb_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("a\n"),
                Just("b\n"),
                Just("c\n"),
                Just("d\r\n"),
                Just("e")
            ],
            0..12,
        )
        .prop_map(|v| v.concat())
    }

    proptest! {
        #[test]
        fn merge_laws(b in arb_text(), o in arb_text(), t in arb_text()) {
            // One side unchanged → the other side, exactly.
            prop_assert_eq!(m(&b, &o, &b), Some(o.clone()));
            prop_assert_eq!(m(&b, &b, &t), Some(t.clone()));
            // Same change on both sides → that change.
            prop_assert_eq!(m(&b, &o, &o), Some(o.clone()));
            // Symmetric.
            prop_assert_eq!(m(&b, &o, &t), m(&b, &t, &o));
        }
    }
}
