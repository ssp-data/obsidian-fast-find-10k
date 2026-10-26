//! `ff10k`: dev CLI for the Fast Find 10K engine.

mod smart_env;

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ff10k_core::engine::{Engine, Hit, Mode, SearchOptions};
use ff10k_core::golden::{ghosts, load_vault_with, read_vault_with, run_golden_with, VaultStats};
use ff10k_core::index::UpsertProfile;
use ff10k_core::similar::SimilarReason;
use regex::Regex;

#[derive(Parser)]
#[command(name = "ff10k", about = "Fast Find 10K dev CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a vault and print docs, bytes, time and RSS.
    Index { vault: PathBuf },
    /// Search a vault.
    Query {
        vault: PathBuf,
        query: String,
        /// Files only (no heading/content/ghost hits).
        #[arg(long)]
        files: bool,
        /// Print per-term and component scores.
        #[arg(long)]
        debug: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Load .smart-env vectors.
        #[arg(long)]
        smart_env: bool,
    },
    /// Notes similar to a note (vault-relative path).
    Similar {
        vault: PathBuf,
        path: String,
        #[arg(long)]
        smart_env: bool,
    },
    /// Run a golden TOML file; exit 1 on failures.
    Golden { vault: PathBuf, golden: PathBuf },
    /// Latency benchmark.
    Bench { vault: PathBuf },
    /// Index a vault and print the time split across read / extract / tokenize / insert / keys.
    Profile { vault: PathBuf },
}

/// Reads `<vault>/.obsidian/app.json` once and builds the exclusion predicate.
fn exclusion(vault: &Path) -> Result<Box<dyn Fn(&str) -> bool>> {
    match std::fs::read_to_string(vault.join(".obsidian/app.json")) {
        Ok(text) => build_exclusion(&text),
        Err(_) => Ok(Box::new(|_| false)),
    }
}

/// Obsidian "Excluded files": `/regex/` entries match the vault-relative path, others are prefixes.
/// Regexes Rust cannot compile (JS lookahead, backreferences) and empty ones are skipped with a warning.
fn build_exclusion(app_json: &str) -> Result<Box<dyn Fn(&str) -> bool>> {
    let json: serde_json::Value = serde_json::from_str(app_json).context("parsing .obsidian/app.json")?;
    let mut prefixes = Vec::new();
    let mut regexes = Vec::new();
    for e in json["userIgnoreFilters"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
        if e.len() >= 2 && e.starts_with('/') && e.ends_with('/') {
            let body = &e[1..e.len() - 1];
            if body.is_empty() {
                eprintln!("warning: skipping empty excluded-files regex {e:?}");
                continue;
            }
            match Regex::new(body) {
                Ok(r) => regexes.push(r),
                Err(err) => eprintln!("warning: skipping excluded-files regex {e:?}: {err}"),
            }
        } else if !e.is_empty() {
            prefixes.push(e.to_string());
        }
    }
    Ok(Box::new(move |p| prefixes.iter().any(|x| p.starts_with(x.as_str())) || regexes.iter().any(|r| r.is_match(p))))
}

fn load(vault: &Path, ex: &dyn Fn(&str) -> bool) -> Result<(Engine, VaultStats)> {
    load_vault_with(vault, ex).with_context(|| format!("loading {}", vault.display()))
}

fn attach_vectors(engine: &mut Engine, vault: &Path) -> Result<()> {
    let Some((dims, paths, data)) = smart_env::load(vault) else {
        bail!("no usable .smart-env in {}", vault.display());
    };
    let n = paths.len();
    engine.set_vectors(dims, paths, data).map_err(|e| anyhow::anyhow!("set_vectors: {e:?}"))?;
    println!("vectors: {n} (dims {dims})");
    Ok(())
}

/// Global allocator wrapper counting live heap bytes, for `ff10k profile`.
struct Counting;

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(l.size(), Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE_BYTES.fetch_sub(l.size(), Relaxed);
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new.wrapping_sub(l.size()), Relaxed);
        System.realloc(p, l, new)
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn live_mb() -> f64 {
    LIVE_BYTES.load(Relaxed) as f64 / (1024.0 * 1024.0)
}

fn rss_kb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok()
}

fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn print_hit(h: &Hit, debug: bool) {
    let extra = if let Some(hd) = &h.heading {
        format!("# {}", hd.text)
    } else if let Some(x) = &h.excerpt {
        x.text.replace('\n', " ")
    } else {
        String::new()
    };
    let ghost = if h.is_ghost { format!(" [ghost x{}]", h.ghost_refs) } else { String::new() };
    println!("{}\t{:.3}\t{:?}\t{}{}\t{}\t{}", h.tier, h.score, h.badge, h.title, ghost, h.path, extra);
    if debug {
        if let Some(d) = &h.debug {
            for (t, f, k, s) in &d.per_term {
                println!("    term {t:?} field {f:?} kind {k:?} score {s:.3}");
            }
            for (n, v) in &d.components {
                println!("    {n}: {v:.3}");
            }
        }
    }
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    sorted[((sorted.len() as f64 * p).ceil() as usize).saturating_sub(1).min(sorted.len() - 1)]
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Index { vault } => {
            let t = Instant::now();
            let ex = exclusion(&vault)?;
            let (_engine, stats) = load(&vault, &*ex)?;
            let el = t.elapsed();
            println!("docs: {}", stats.docs);
            println!("bytes: {}", stats.bytes);
            println!("index time: {:.2?}", el);
            if let Some(kb) = rss_kb() {
                println!("rss: {:.1} MB", kb as f64 / 1024.0);
            }
        }
        Cmd::Query { vault, query, files, debug, limit, smart_env } => {
            let ex = exclusion(&vault)?;
            let (mut engine, _) = load(&vault, &*ex)?;
            if smart_env {
                attach_vectors(&mut engine, &vault)?;
            }
            let opts = SearchOptions {
                mode: if files { Mode::FilesOnly } else { Mode::All },
                limit,
                now_secs: now_secs(),
                debug,
            };
            let r = engine.search(&query, &opts);
            println!("{} hits shown, {} candidates, {} docs, {} us", r.hits.len(), r.candidates, r.total_docs, r.elapsed_us);
            println!("tier\tscore\tbadge\ttitle\tpath\theading/excerpt");
            for h in &r.hits {
                print_hit(h, debug);
            }
        }
        Cmd::Similar { vault, path, smart_env } => {
            let ex = exclusion(&vault)?;
            let (mut engine, _) = load(&vault, &*ex)?;
            if smart_env {
                attach_vectors(&mut engine, &vault)?;
            }
            for s in engine.similar(&path, 20, 0.0) {
                let reason = match s.reason {
                    SimilarReason::SimilarTitle(x) => format!("SimilarTitle({x:.2})"),
                    SimilarReason::Semantic(x) => format!("Semantic({x:.3})"),
                    r => format!("{r:?}"),
                };
                println!("{reason}\t{:.3}\t{}\t{}", s.score, s.title, s.path);
            }
        }
        Cmd::Golden { vault, golden } => {
            let ex = exclusion(&vault)?;
            let failures = run_golden_with(&vault, &golden, &*ex);
            for f in &failures {
                println!("FAIL {f}");
            }
            if !failures.is_empty() {
                std::process::exit(1);
            }
            println!("golden: all passed");
        }
        Cmd::Profile { vault } => {
            let ex = exclusion(&vault)?;
            let heap_before = live_mb();
            let t = Instant::now();
            let notes = read_vault_with(&vault, &*ex).with_context(|| format!("reading {}", vault.display()))?;
            let read = t.elapsed();
            let bytes: usize = notes.iter().map(|n| n.text.len()).sum();
            let mut engine = Engine::new();
            let mut prof = UpsertProfile::default();
            let t = Instant::now();
            for n in &notes {
                engine.upsert_profiled(&n.path, n.mtime, &n.text, &mut prof);
            }
            let upsert = t.elapsed();
            let t = Instant::now();
            engine.set_ghosts(ghosts(&notes));
            let ghost = t.elapsed();
            let t = Instant::now();
            std::hint::black_box(engine.search("a", &SearchOptions::default()));
            let first = t.elapsed();
            let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
            let mb = bytes as f64 / (1024.0 * 1024.0);
            println!("docs: {}, bytes: {} ({mb:.1} MB)", notes.len(), bytes);
            println!("read files       {:>9.1} ms", ms(read));
            println!("upsert total     {:>9.1} ms  ({:.1} MB/s)", ms(upsert), mb / upsert.as_secs_f64());
            println!("  extract        {:>9.1} ms", ms(prof.extract));
            println!("  unindex        {:>9.1} ms", ms(prof.unindex));
            println!("  derive (tok)   {:>9.1} ms", ms(prof.derive));
            println!("  insert         {:>9.1} ms", ms(prof.insert));
            println!("  keys           {:>9.1} ms", ms(prof.keys));
            println!("  other          {:>9.1} ms", ms(upsert.saturating_sub(prof.extract + prof.unindex + prof.derive + prof.insert + prof.keys)));
            println!("ghosts (CLI)     {:>9.1} ms", ms(ghost));
            println!("first search     {:>9.1} ms", ms(first));
            drop(notes);
            println!("engine heap      {:>9.1} MB (live bytes allocated by the engine)", live_mb() - heap_before);
            if let Some(kb) = rss_kb() {
                println!("rss: {:.1} MB", kb as f64 / 1024.0);
            }
        }
        Cmd::Bench { vault } => {
            let ex = exclusion(&vault)?;
            let (engine, stats) = load(&vault, &*ex)?;
            println!("docs: {}, bytes: {}", stats.docs, stats.bytes);
            let opts = SearchOptions { now_secs: now_secs(), ..SearchOptions::default() };
            for q in ["ai", "data engineering", "data engineer ai", "obsidian troubelshoot", "path:resources"] {
                let mut us = Vec::with_capacity(200);
                for _ in 0..200 {
                    let t = Instant::now();
                    std::hint::black_box(engine.search(q, &opts));
                    us.push(t.elapsed().as_micros() as u64);
                }
                us.sort_unstable();
                println!("{q:<24} p50 {:>7} us  p95 {:>7} us", pct(&us, 0.5), pct(&us, 0.95));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusion_entries() {
        let json = r#"{"userIgnoreFilters":["templates/","/\\.tmp$/","/^(?!x)/","//"]}"#;
        let ex = build_exclusion(json).unwrap();
        assert!(ex("templates/a.md"));
        assert!(ex("notes/a.tmp"));
        assert!(!ex("notes/a.md"));
        assert!(!ex("x.md"));
    }
}
