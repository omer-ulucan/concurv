#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R5 gate: false-positive and false-negative rates of the overlap
//! warnings (RFC 0006 §4), measured over a seeded corpus.
//!
//! - `path` and `conflict` predictions are compared with the real
//!   three-way merge (`textmerge::merge3`, the rules of ADR 0006).
//! - `dependency` predictions are compared with a constructed ground truth:
//!   does the dependent change really call the function whose signature
//!   changed upstream?
//!
//! Rates are printed (copied into `bench/RESULTS.md`) and bounded here.

use concurv_workspace::coord::{changed_definitions, mentions_in_code, predict_conflict};
use concurv_workspace::textmerge::merge3;

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

#[derive(Default, Debug)]
struct Counts {
    tp: u32,
    fp: u32,
    tn: u32,
    fn_: u32,
}

impl Counts {
    fn add(&mut self, predicted: bool, actual: bool) {
        match (predicted, actual) {
            (true, true) => self.tp += 1,
            (true, false) => self.fp += 1,
            (false, false) => self.tn += 1,
            (false, true) => self.fn_ += 1,
        }
    }
    fn fp_rate(&self) -> f64 {
        f64::from(self.fp) / f64::from((self.fp + self.tn).max(1))
    }
    fn fn_rate(&self) -> f64 {
        f64::from(self.fn_) / f64::from((self.fn_ + self.tp).max(1))
    }
    fn line(&self, what: &str) -> String {
        format!(
            "{what}: {} cases, {} real; FP rate {:.3} ({} of {} clean), FN rate {:.3} ({} of {} real)",
            self.tp + self.fp + self.tn + self.fn_,
            self.tp + self.fn_,
            self.fp_rate(),
            self.fp,
            self.fp + self.tn,
            self.fn_rate(),
            self.fn_,
            self.tp + self.fn_
        )
    }
}

/// One random edit of `lines`: replace, insert or delete 1-3 lines.
fn edit(lines: &[String], rng: &mut Rng, tag: &str) -> Vec<String> {
    let mut out = lines.to_vec();
    let at = rng.next(lines.len() as u64 - 3) as usize;
    let n = 1 + rng.next(3) as usize;
    match rng.next(3) {
        0 => {
            for (k, l) in out.iter_mut().skip(at).take(n).enumerate() {
                *l = format!("{tag} replaced {k}");
            }
        }
        1 => {
            for k in 0..n {
                out.insert(at + k, format!("{tag} inserted {k}"));
            }
        }
        _ => {
            out.drain(at..at + n);
        }
    }
    out
}

fn join(v: &[String]) -> Vec<u8> {
    v.iter()
        .map(|l| format!("{l}\n"))
        .collect::<String>()
        .into_bytes()
}

#[test]
fn conflict_warnings_match_real_merges() {
    let base: Vec<String> = (0..60).map(|i| format!("    let v{i} = {i};")).collect();
    let mut rng = Rng(0x0dd5_eed5_1234_5678);
    let (mut path, mut conflict) = (Counts::default(), Counts::default());
    for case in 0..2000 {
        let a = edit(&base, &mut rng, "A");
        // 1 in 20: both sides make the same edit (merges cleanly).
        let b = if case % 20 == 0 {
            a.clone()
        } else {
            edit(&base, &mut rng, "B")
        };
        let (bb, ab, bbb) = (join(&base), join(&a), join(&b));
        let real = merge3(&bb, &ab, &bbb).is_none();
        path.add(true, real);
        conflict.add(predict_conflict(Some(&bb), Some(&ab), Some(&bbb)), real);
    }
    eprintln!("{}", path.line("path level (same file)"));
    eprintln!("{}", conflict.line("conflict level (line ranges)"));
    assert_eq!(path.fn_, 0);
    assert_eq!(conflict.fn_, 0, "a real conflict is never missed");
    assert!(conflict.fp_rate() < 0.1, "{:?}", conflict);
    assert!(conflict.fp_rate() < path.fp_rate());
}

#[test]
fn dependency_warnings_measured_against_ground_truth() {
    let funcs = 20;
    let lib: Vec<String> = (0..funcs)
        .flat_map(|i| {
            vec![
                format!("pub fn f{i}(x: u32) -> u32 {{"),
                format!("    x + {i}"),
                "}".to_owned(),
            ]
        })
        .collect();
    let mut rng = Rng(0x5eed_0bad_cafe_f00d);
    let mut dep = Counts::default();
    let mut kinds = [0u32; 5];
    for _ in 0..2000 {
        let k = rng.next(funcs) as usize;
        let signature_changed = rng.next(2) == 0;
        let mut up = lib.clone();
        if signature_changed {
            up[3 * k] = format!("pub fn f{k}(x: u32, strict: bool) -> u32 {{");
        } else {
            up[3 * k + 1] = format!("    x * {k}");
        }
        // The dependent change refers to f_j in one of five ways.
        let j = if rng.next(2) == 0 {
            k
        } else {
            rng.next(funcs) as usize
        };
        let style = rng.next(5) as usize;
        kinds[style] += 1;
        let mine = match style {
            0 => format!("fn run() -> u32 {{\n    f{j}(1)\n}}\n"),
            1 => format!("use crate::lib::f{j} as g;\nfn run() -> u32 {{\n    g(1)\n}}\n"),
            2 => format!("// TODO: maybe use f{j} here\nfn run() -> u32 {{ 1 }}\n"),
            3 => format!("fn run() -> &'static str {{ \"f{j}\" }}\n"),
            _ => format!("fn run() -> u32 {{\n    HANDLERS[{j}](1)\n}}\n"),
        };
        // Ground truth: the code really calls f_k (directly, through an
        // alias, or through a table), and f_k's signature changed.
        let calls_fk = j == k && matches!(style, 0 | 1 | 4);
        let real = signature_changed && calls_fk;
        let defs = changed_definitions(Some(&join(&lib)), Some(&join(&up)));
        let predicted = defs.iter().any(|d| mentions_in_code(mine.as_bytes(), d));
        dep.add(predicted, real);
    }
    eprintln!("{}", dep.line("dependency (changed definitions, lexical)"));
    eprintln!("  styles (direct call, alias, comment, string, call through a table): {kinds:?}");
    // Direct and aliased calls are always caught; calls through a table
    // are invisible to a lexical rule (false negatives); mentions in
    // comments and strings are ignored.
    assert!(dep.fn_rate() < 0.4, "{:?}", dep);
    assert!(dep.fp_rate() < 0.02, "{:?}", dep);
    assert!(dep.tp > 0 && dep.fn_ > 0, "the corpus exercises every case");
}
