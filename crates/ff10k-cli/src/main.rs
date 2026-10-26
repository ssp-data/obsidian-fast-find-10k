//! `ff10k`: dev CLI for the Fast Find 10K engine.

mod smart_env;

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ff10k_core::engine::{Engine, Hit, Mode, SearchOptions};
use ff10k_core::golden::{load_vault_with, run_golden_with};
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
}

/// Obsidian "Excluded files": `/regex/` entries match the vault-relative path, others are prefixes.
fn exclusion(vault: &Path) -> Result<Box<dyn Fn(&str) -> bool>> {
    let Ok(text) = std::fs::read_to_string(vault.join(".obsidian/app.json")) else {
        return Ok(Box::new(|_| false));
    };
    let json: serde_json::Value = serde_json::from_str(&text).context("parsing .obsidian/app.json")?;
    let mut prefixes = Vec::new();
    let mut regexes = Vec::new();
    for e in json["userIgnoreFilters"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
        if e.len() >= 2 && e.starts_with('/') && e.ends_with('/') {
            regexes.push(Regex::new(&e[1..e.len() - 1]).with_context(|| format!("bad ignore regex {e}"))?);
        } else if !e.is_empty() {
            prefixes.push(e.to_string());
        }
    }
    Ok(Box::new(move |p| prefixes.iter().any(|x| p.starts_with(x.as_str())) || regexes.iter().any(|r| r.is_match(p))))
}

fn load(vault: &Path) -> Result<Engine> {
    let ex = exclusion(vault)?;
    load_vault_with(vault, &*ex).with_context(|| format!("loading {}", vault.display()))
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

fn rss_kb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok()
}

/// Total size of the `.md` files the indexer sees (same walk rules and exclusions).
fn total_bytes(vault: &Path) -> u64 {
    fn walk(dir: &Path, root: &Path, ex: &dyn Fn(&str) -> bool, sum: &mut u64) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if p.is_dir() {
                walk(&p, root, ex, sum);
            } else if p.extension().is_some_and(|x| x == "md") {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                if !ex(&rel) {
                    *sum += e.metadata().map_or(0, |m| m.len());
                }
            }
        }
    }
    let mut sum = 0;
    if let Ok(ex) = exclusion(vault) {
        walk(vault, vault, &*ex, &mut sum);
    }
    sum
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
            let engine = load(&vault)?;
            let el = t.elapsed();
            println!("docs: {}", engine.doc_count());
            println!("bytes: {}", total_bytes(&vault));
            println!("index time: {:.2?}", el);
            if let Some(kb) = rss_kb() {
                println!("rss: {:.1} MB", kb as f64 / 1024.0);
            }
        }
        Cmd::Query { vault, query, files, debug, limit, smart_env } => {
            let mut engine = load(&vault)?;
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
            let mut engine = load(&vault)?;
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
        Cmd::Bench { vault } => {
            let engine = load(&vault)?;
            println!("docs: {}, bytes: {}", engine.doc_count(), total_bytes(&vault));
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
