//! Reproducible `status` / `save` benchmarks (R1.5 P0-6).
//!
//! Generates deterministic fixtures from a fixed seed (no network, no
//! external tools), runs the engine through its library API and prints a
//! Markdown table with p50/p95 wall-clock latencies of `status` with and
//! without the stat cache and of a no-op `save`. Run with
//! `cargo run --release -p concurv-bench [-- --sizes 100,1000,10000 --runs 10]`.
//!
//! Each fixture has `n` tracked files in nested directories (90 % text of
//! 1–4 KiB, 10 % binary of 64 KiB) plus an ignored `target/` tree with `n/2`
//! files that the scanner must skip.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use concurv_core::Result;
use concurv_workspace::{Repo, RepoOptions, StatusOptions, save, status_with};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
    fn text(&mut self, n: usize) -> Vec<u8> {
        const WORDS: [&str; 8] = ["fn ", "let ", "value", " = ", "42;", "\n", "// note", "{}"];
        let mut v = Vec::with_capacity(n + 8);
        while v.len() < n {
            v.extend_from_slice(WORDS[(self.next() % 8) as usize].as_bytes());
        }
        v.truncate(n);
        v
    }
}

fn write(root: &Path, rel: &str, data: &[u8]) -> std::io::Result<()> {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(p, data)
}

/// Build the fixture; returns total bytes of tracked files.
fn fixture(root: &Path, n: usize, seed: u64) -> std::io::Result<u64> {
    let mut rng = Rng(seed | 1);
    write(root, ".gitignore", b"/target\n")?;
    let mut total = 0u64;
    for i in 0..n {
        let rel = format!(
            "src/m{}/s{}/f{i}.{}",
            i % 17,
            i % 5,
            if i % 10 == 0 { "bin" } else { "rs" }
        );
        let data = if i % 10 == 0 {
            rng.bytes(64 * 1024)
        } else {
            let len = 1024 + (rng.next() % 3072) as usize;
            rng.text(len)
        };
        total += data.len() as u64;
        write(root, &rel, &data)?;
    }
    for i in 0..n / 2 {
        write(
            root,
            &format!("target/debug/o{}/x{i}.o", i % 13),
            &rng.bytes(512),
        )?;
    }
    Ok(total)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn time_ms<T>(f: impl FnOnce() -> Result<T>) -> Result<(f64, T)> {
    let t = Instant::now();
    let v = f()?;
    Ok((t.elapsed().as_secs_f64() * 1000.0, v))
}

fn bench(base: &Path, n: usize, runs: usize) -> Result<String> {
    let root: PathBuf = base.join(format!("fixture-{n}"));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).map_err(|e| concurv_core::ConcurvError::io(e, "create", &root))?;
    let bytes = fixture(&root, n, 0x5eed_0000 + n as u64)
        .map_err(|e| concurv_core::ConcurvError::io(e, "write fixture", &root))?;
    // Files are not edited in the second before a save in practice; let the
    // timestamps leave the stat cache's racy window (ADR 0005).
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let opts = RepoOptions {
        author: Some("bench".into()),
        ..RepoOptions::default()
    };
    let repo = Repo::init(&root, opts)?;
    let (first_save, out) = time_ms(|| save(&repo, "initial"))?;
    let tracked = out.changes.len();
    let timed_status = |use_stat_cache: bool| -> Result<Vec<f64>> {
        let mut v = Vec::with_capacity(runs);
        for _ in 0..runs {
            let (ms, s) = time_ms(|| status_with(&repo, StatusOptions { use_stat_cache }))?;
            if !s.changes.is_empty() {
                return Err(concurv_core::ConcurvError::internal(
                    "benchmark tree unexpectedly dirty",
                ));
            }
            v.push(ms);
        }
        v.sort_by(f64::total_cmp);
        Ok(v)
    };
    let full = timed_status(false)?;
    let st = timed_status(true)?;
    let mut noop = Vec::with_capacity(runs.min(5));
    for _ in 0..runs.min(5) {
        noop.push(time_ms(|| save(&repo, "noop"))?.0);
    }
    noop.sort_by(f64::total_cmp);
    let _ = fs::remove_dir_all(&root);
    Ok(format!(
        "| {n} | {tracked} | {:.1} MiB | {first_save:.0} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
        bytes as f64 / (1024.0 * 1024.0),
        percentile(&full, 0.5),
        percentile(&full, 0.95),
        percentile(&st, 0.5),
        percentile(&st, 0.95),
        percentile(&noop, 0.5),
        percentile(&noop, 0.95),
    ))
}

fn main() {
    let mut sizes = vec![100usize, 1000, 10000];
    let mut runs = 10usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sizes" => {
                if let Some(v) = args.next() {
                    sizes = v.split(',').filter_map(|s| s.parse().ok()).collect();
                }
            }
            "--runs" => {
                if let Some(v) = args.next().and_then(|v| v.parse().ok()) {
                    runs = v;
                }
            }
            other => {
                eprintln!(
                    "unknown argument {other:?}; usage: concurv-bench [--sizes 100,1000] [--runs N]"
                );
                std::process::exit(2);
            }
        }
    }
    let base = std::env::temp_dir().join(format!("concurv-bench-{}", std::process::id()));
    println!(
        "| files | tracked | tracked size | initial save ms | status --no-cache p50 ms | p95 | status p50 ms | p95 | no-op save p50 ms | p95 |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for n in sizes {
        match bench(&base, n, runs) {
            Ok(row) => println!("{row}"),
            Err(e) => {
                eprintln!("benchmark for {n} files failed: {e}");
                std::process::exit(1);
            }
        }
    }
    let _ = fs::remove_dir_all(&base);
}
