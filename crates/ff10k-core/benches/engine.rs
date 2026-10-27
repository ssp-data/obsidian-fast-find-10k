//! Criterion benches (spec §12/§14): index build for 10k synthetic notes, 1/2/3-term and
//! fuzzy queries, and a 10k x 384 vector scan.
//!
//! The 10k notes are clones of the fixture vault (`fixtures/vault`) with numbered suffixes on
//! the file name and folder, each text cut to at most 3,000 bytes (at a char boundary). The
//! fixture notes are short, so the corpus is about 3 MB; real-vault numbers (27 MB) come from
//! `ff10k profile` / `ff10k bench` (see docs/perf.md).

use std::path::Path;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use ff10k_core::engine::{Engine, SearchOptions};

const NOTES: usize = 10_000;
const MAX_TEXT: usize = 3_000;
const DIMS: usize = 384;

fn read_dir(dir: &Path, rel: &str, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).expect("fixture vault").flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if e.file_type().unwrap().is_dir() {
            read_dir(&e.path(), &path, out);
        } else if name.ends_with(".md") {
            out.push((path, std::fs::read_to_string(e.path()).unwrap()));
        }
    }
}

fn cut(s: &str, max: usize) -> &str {
    let mut i = s.len().min(max);
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// `NOTES` (path, text) pairs cloned from the fixture vault.
fn corpus() -> Vec<(String, String)> {
    let mut base = Vec::new();
    read_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vault"), "", &mut base);
    assert!(!base.is_empty(), "fixtures/vault is empty");
    (0..NOTES)
        .map(|i| {
            let (path, text) = &base[i % base.len()];
            let k = i / base.len();
            let stem = path.strip_suffix(".md").unwrap_or(path);
            (format!("clone {}/{stem} {k}.md", k % 50), cut(text, MAX_TEXT).to_string())
        })
        .collect()
}

fn engine(notes: &[(String, String)]) -> Engine {
    let mut e = Engine::new();
    for (p, t) in notes {
        e.upsert(p, 0.0, t);
    }
    e
}

fn bench_index(c: &mut Criterion) {
    let notes = corpus();
    let mb = notes.iter().map(|n| n.1.len()).sum::<usize>() as f64 / 1e6;
    let mut g = c.benchmark_group("index");
    g.sample_size(10);
    g.bench_function(format!("10k notes ({mb:.0} MB)"), |b| b.iter(|| black_box(engine(&notes))));
    g.finish();
}

fn bench_queries(c: &mut Criterion) {
    let e = engine(&corpus());
    e.warm_up();
    let opts = SearchOptions::default();
    let mut g = c.benchmark_group("query");
    for q in ["ai", "data engineering", "data engineer ai", "obsidian troubelshoot"] {
        g.bench_function(q, |b| b.iter(|| black_box(e.search(q, &opts))));
    }
    g.finish();
}

fn bench_vectors(c: &mut Criterion) {
    let notes = corpus();
    let mut e = engine(&notes);
    let paths: Vec<String> = notes.iter().map(|n| n.0.clone()).collect();
    let data: Vec<f32> = (0..paths.len() * DIMS).map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5).collect();
    e.set_vectors(DIMS, paths, data).unwrap();
    let q: Vec<f32> = (0..DIMS).map(|i| (i as f32).sin()).collect();
    c.bench_function("vector scan 10k x 384", |b| b.iter(|| black_box(e.semantic(&q, 20, 0.0))));
}

criterion_group!(benches, bench_index, bench_queries, bench_vectors);
criterion_main!(benches);
