//! `ff10k`: dev CLI for the Fast Find 10K engine.

mod smart_env;

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ff10k_core::engine::{Config, Engine, Hit, Mode, SearchOptions};
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
        /// Abbreviation group, as one settings line (`"oss: open source, open-source software"`); repeatable.
        #[arg(long = "synonym")]
        synonyms: Vec<String>,
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
    /// Recall check over the vault's own titles: for every note (or every k-th), query its full
    /// title, its first two words and its title with one typo, and report where the note ranks.
    Recall {
        vault: PathBuf,
        /// Check every k-th note (sorted by path) instead of all.
        #[arg(long, default_value_t = 1)]
        every: usize,
        /// Write the sampled notes and their queries as JSON (for running the same set elsewhere).
        #[arg(long)]
        sample_out: Option<PathBuf>,
        /// Print every miss (default: the first 25 per variant).
        #[arg(long)]
        all_misses: bool,
    },
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

/// Global allocator wrapper counting live heap bytes while `COUNTING` is on (`ff10k profile`
/// only; every other command pays one relaxed load per allocation).
struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

fn count(delta: usize) {
    if COUNTING.load(Relaxed) {
        LIVE_BYTES.fetch_add(delta, Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count(l.size());
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        count(l.size().wrapping_neg());
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        count(new.wrapping_sub(l.size()));
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

/// Query variants derived from a note title (shared with the Omnisearch harness via `--sample-out`).
struct RecallQueries {
    full: String,
    /// First two words, when the title has at least three.
    two: Option<String>,
    /// The longest word of 6+ letters with its middle character deleted, when there is one.
    typo: Option<String>,
}

fn recall_queries(title: &str) -> RecallQueries {
    let words: Vec<&str> = title.split_whitespace().collect();
    let two = (words.len() >= 3).then(|| words[..2].join(" "));
    let longest = words.iter().filter(|w| w.chars().count() >= 6 && w.chars().all(char::is_alphanumeric)).max_by_key(|w| w.chars().count());
    let typo = longest.map(|w| {
        let n = w.chars().count();
        let typoed: String = w.chars().enumerate().filter(|(i, _)| *i != n / 2).map(|(_, c)| c).collect();
        words.iter().map(|x| if *x == *w { typoed.as_str() } else { x }).collect::<Vec<_>>().join(" ")
    });
    RecallQueries { full: title.to_string(), two, typo }
}

struct RecallTally {
    name: &'static str,
    queries: usize,
    at: [usize; 3],
    /// Same title, other path, in the top 1 (duplicate notes).
    dup_at1: usize,
    misses: Vec<String>,
    micros: Vec<u64>,
}

impl RecallTally {
    fn new(name: &'static str) -> Self {
        RecallTally { name, queries: 0, at: [0; 3], dup_at1: 0, misses: Vec::new(), micros: Vec::new() }
    }

    fn record(&mut self, engine: &Engine, query: &str, path: &str, title: &str) {
        let opts = SearchOptions { mode: Mode::All, limit: 10, now_secs: now_secs(), debug: false };
        let r = engine.search(query, &opts);
        self.queries += 1;
        self.micros.push(r.elapsed_us);
        let rank = r.hits.iter().position(|h| h.path == path);
        match rank {
            Some(i) => {
                for (k, n) in [1usize, 5, 10].into_iter().enumerate() {
                    if i < n {
                        self.at[k] += 1;
                    }
                }
            }
            None => {
                let top: Vec<String> = r.hits.iter().take(3).map(|h| format!("{}{}", h.title, if h.is_ghost { " [ghost]" } else { "" })).collect();
                self.misses.push(format!("{query:?} → {path:?}: top {top:?}"));
            }
        }
        if rank != Some(0) && r.hits.first().is_some_and(|h| h.title == title && !h.is_ghost) {
            self.dup_at1 += 1;
        }
    }

    fn print(&mut self, all: bool) {
        let q = self.queries.max(1) as f64;
        self.micros.sort_unstable();
        let pct = |p: f64| self.micros.get(((self.micros.len() as f64 * p) as usize).min(self.micros.len().saturating_sub(1))).copied().unwrap_or(0);
        println!(
            "{:<6} {:>5} queries  @1 {:>6.2}%  @5 {:>6.2}%  @10 {:>6.2}%  (same-title duplicate first: {})  p50 {:.1} ms  p95 {:.1} ms",
            self.name,
            self.queries,
            100.0 * self.at[0] as f64 / q,
            100.0 * self.at[1] as f64 / q,
            100.0 * self.at[2] as f64 / q,
            self.dup_at1,
            pct(0.5) as f64 / 1000.0,
            pct(0.95) as f64 / 1000.0
        );
        let shown = if all { self.misses.len() } else { self.misses.len().min(25) };
        for m in &self.misses[..shown] {
            println!("    miss {m}");
        }
        if shown < self.misses.len() {
            println!("    … {} more misses (--all-misses)", self.misses.len() - shown);
        }
    }
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
        Cmd::Query { vault, query, files, debug, limit, smart_env, synonyms } => {
            let ex = exclusion(&vault)?;
            let (mut engine, _) = load(&vault, &*ex)?;
            if smart_env {
                attach_vectors(&mut engine, &vault)?;
            }
            if !synonyms.is_empty() {
                engine.set_config(Config { synonyms, ..Config::default() });
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
        Cmd::Recall { vault, every, sample_out, all_misses } => {
            let ex = exclusion(&vault)?;
            let notes = read_vault_with(&vault, &*ex)?;
            let mut engine = Engine::new();
            for n in &notes {
                engine.upsert(&n.path, n.mtime, &n.text);
            }
            engine.set_ghosts(ghosts(&notes));
            engine.warm_up();
            let every = every.max(1);
            let sample: Vec<(&str, String, RecallQueries)> = notes
                .iter()
                .step_by(every)
                .map(|n| {
                    let title = n.path.rsplit('/').next().unwrap_or(&n.path).trim_end_matches(".md").to_string();
                    let q = recall_queries(&title);
                    (n.path.as_str(), title, q)
                })
                .collect();
            if let Some(out) = sample_out {
                let rows: Vec<serde_json::Value> = sample
                    .iter()
                    .map(|(p, t, q)| serde_json::json!({"path": p, "title": t, "full": q.full, "two": q.two, "typo": q.typo}))
                    .collect();
                std::fs::write(&out, serde_json::to_string(&rows)?)?;
                println!("sample written: {} notes → {}", rows.len(), out.display());
            }
            println!("{} notes indexed, {} checked (every {every})", notes.len(), sample.len());
            let mut full = RecallTally::new("full");
            let mut two = RecallTally::new("two");
            let mut typo = RecallTally::new("typo");
            for (path, title, q) in &sample {
                full.record(&engine, &q.full, path, title);
                if let Some(t) = &q.two {
                    two.record(&engine, t, path, title);
                }
                if let Some(t) = &q.typo {
                    typo.record(&engine, t, path, title);
                }
            }
            println!("full: the whole title; two: its first two words (titles of 3+ words); typo: the longest 6+ letter word minus its middle letter.");
            println!("@k: the note itself is within the first k hits.");
            for t in [&mut full, &mut two, &mut typo] {
                t.print(all_misses);
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
            // Counted from here on: frees of blocks allocated earlier would go negative, but
            // nothing allocated before this point is freed before the final reading.
            COUNTING.store(true, Relaxed);
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
