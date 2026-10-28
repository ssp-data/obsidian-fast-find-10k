//! Near-duplicate detection from titles: reordered words, copy suffixes, same title in another folder.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::doc::Field;
use crate::index::{DocId, Index};
use crate::normalize::{normalize, strip_symbols};
use crate::tokenize::tokenize;

const STOPWORDS: [&str; 10] = ["a", "an", "the", "in", "of", "with", "for", "and", "or", "to"];
/// Candidate evaluation cap per source doc.
const MAX_CANDIDATES: usize = 2000;
/// Title tokens with longer posting lists are too common to be informative.
const COMMON_TOKEN_DOCS: usize = 500;

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum SimilarReason {
    SameWordsReordered,
    SimilarTitle(f32),
    CopySuffix,
    SameTitleDifferentFolder,
    Semantic(f32),
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SimilarHit {
    pub path: String,
    pub title: String,
    pub reason: SimilarReason,
    pub score: f32,
}

/// Jaccard similarity of two token lists, compared as sets (duplicates ignored). Stopwords are
/// NOT removed here; callers pass non-stopword tokens (see `title_token_set`).
pub fn title_jaccard(a: &[String], b: &[String]) -> f32 {
    let a: HashSet<&str> = a.iter().map(String::as_str).collect();
    let b: HashSet<&str> = b.iter().map(String::as_str).collect();
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(&b).count() as f32 / union as f32
}

/// Strips one trailing copy suffix, if any.
fn strip_suffix_once(s: &str) -> Option<&str> {
    let s = s.trim_end();
    if let Some(inner) = s.strip_suffix(')') {
        let open = inner.rfind(" (")?;
        let digits = &inner[open + 2..];
        return (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| &s[..open]);
    }
    if let Some(rest) = s.strip_suffix(" copy") {
        return Some(rest);
    }
    let head = s.trim_end_matches(|c: char| c.is_ascii_digit());
    if head.len() == s.len() {
        return None;
    }
    if let Some(rest) = head.strip_suffix(" copy ") {
        return Some(rest);
    }
    let rest = head.strip_suffix('-')?;
    // A digit before the dash is a date or version (`2026-07-01`, `v1.2-3`), not a copy number.
    if rest.ends_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let rest = rest.strip_suffix(' ').unwrap_or(rest);
    (!rest.is_empty()).then_some(rest)
}

fn trim_punct(s: &str) -> &str {
    s.trim_end_matches(['.', ',', ';', ':', '!', '?', ' '])
}

/// Normalized title without one trailing ` -N`, `-N`, ` (N)`, ` copy` or ` copy N`, trailing
/// punctuation and double spaces. At most one suffix is stripped, so `2026-07-01` never
/// collapses to `2026`.
pub fn canonical_title(norm_title: &str) -> String {
    let s = norm_title.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = trim_punct(&s);
    trim_punct(strip_suffix_once(trimmed).unwrap_or(trimmed)).to_string()
}

/// Cached per-title keys (filled at upsert/rename by the index).
#[derive(Default)]
pub struct TitleKeys {
    /// `normalize(strip_symbols(title))`.
    pub norm: String,
    /// `canonical_title(norm)`.
    pub canon: String,
    /// Sorted, deduplicated non-stopword tokens.
    pub tokens: Vec<String>,
    /// `word_spans(norm)`, for the unmatched-title-word factor in ranking.
    pub spans: Vec<(u32, u32)>,
}

impl TitleKeys {
    pub fn of(title: &str) -> TitleKeys {
        let stripped = strip_symbols(title);
        let norm = normalize(&stripped);
        let mut tokens: Vec<String> = tokenize(&stripped)
            .into_iter()
            .map(|t| t.text)
            .filter(|t| !STOPWORDS.contains(&t.as_str()))
            .collect();
        tokens.sort_unstable();
        tokens.dedup();
        TitleKeys { canon: canonical_title(&norm), spans: word_spans(&norm), norm, tokens }
    }
}

/// Byte ranges in `norm` (an already normalized string) of its non-stopword words, in order.
/// Hyphenated and snake_case words count per part (`history-and-state` → `history`, `state`).
pub fn word_spans(norm: &str) -> Vec<(u32, u32)> {
    let toks = tokenize(norm);
    let mut spans = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        // A compound is followed by its parts, the first starting inside it: keep only the parts.
        let compound = toks.get(i + 1).is_some_and(|n| n.byte_start < t.byte_end);
        if !compound && !STOPWORDS.contains(&t.text.as_str()) {
            spans.push((t.byte_start, t.byte_end));
        }
    }
    spans
}

fn folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(f, _)| f)
}

/// Size of the intersection of two sorted, deduplicated lists.
fn sorted_intersection(a: &[String], b: &[String]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

/// Candidate docs for `id` with their shared-token counts, capped at `MAX_CANDIDATES` (most
/// shared first is kept; order within the result is unspecified).
fn candidates(index: &Index, id: DocId, keys: &TitleKeys) -> Vec<(DocId, u32)> {
    let lists: Vec<_> = keys.tokens.iter().map(|t| index.exact(Field::Title, t)).collect();
    let all_common = lists.iter().all(|l| l.len() > COMMON_TOKEN_DOCS);
    let mut shared: HashMap<DocId, u32> = HashMap::new();
    let mut used = 0;
    for l in &lists {
        if l.len() > COMMON_TOKEN_DOCS && !all_common {
            continue;
        }
        used += 1;
        for p in l.iter() {
            if p.doc != id {
                *shared.entry(p.doc).or_insert(0) += 1;
            }
        }
    }
    // Based on the tokens actually looked up: skipped common tokens never add to `shared`.
    let min_shared = if used <= 2 { 1 } else { 2 };
    let mut cands: Vec<(DocId, u32)> = shared.into_iter().filter(|c| c.1 >= min_shared).collect();
    if cands.len() > MAX_CANDIDATES {
        cands.select_nth_unstable_by(MAX_CANDIDATES, |a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        cands.truncate(MAX_CANDIDATES);
    }
    cands
}

/// Calls `emit(candidate, reason, score)` for every lexical neighbour of `id`. No strings are
/// allocated per candidate.
fn scan(index: &Index, id: DocId, mut emit: impl FnMut(DocId, SimilarReason, f32)) {
    let (Some(doc), Some(keys)) = (index.get(id), index.title_keys(id)) else { return };
    if keys.tokens.is_empty() {
        return;
    }
    let cands = candidates(index, id, keys);
    let na = keys.tokens.len();
    for (cid, _) in cands {
        let (Some(other), Some(ok)) = (index.get(cid), index.title_keys(cid)) else { continue };
        if other.path == doc.path {
            continue;
        }
        if ok.canon == keys.canon && other.title != doc.title {
            emit(cid, SimilarReason::CopySuffix, 1.0);
        } else if ok.norm == keys.norm && folder(&other.path) != folder(&doc.path) {
            emit(cid, SimilarReason::SameTitleDifferentFolder, 1.0);
        } else {
            let nb = ok.tokens.len();
            // Even a full overlap of the smaller set cannot reach 0.6.
            if (na.min(nb) as f32) < 0.6 * na.max(nb) as f32 {
                continue;
            }
            let s = sorted_intersection(&keys.tokens, &ok.tokens);
            let j = s as f32 / (na + nb - s) as f32;
            if j >= 0.8 {
                emit(cid, SimilarReason::SameWordsReordered, j);
            } else if j >= 0.6 {
                emit(cid, SimilarReason::SimilarTitle(j), j);
            }
        }
    }
}

/// Number of lexical neighbours of `id` (no hits are built).
pub fn similar_count_for(index: &Index, id: DocId) -> u32 {
    let mut n = 0;
    scan(index, id, |_, _, _| n += 1);
    n
}

/// Docs that are lexical neighbours of `id`.
pub fn similar_ids_for(index: &Index, id: DocId) -> Vec<DocId> {
    let mut out = Vec::new();
    scan(index, id, |cid, _, _| out.push(cid));
    out
}

/// Lexical (title-based) neighbours of `id`, best first, at most `limit`.
pub fn similar_for(index: &Index, id: DocId, limit: usize) -> Vec<SimilarHit> {
    let mut out = Vec::new();
    if limit == 0 {
        return out;
    }
    scan(index, id, |cid, reason, score| {
        if let Some(d) = index.get(cid) {
            out.push(SimilarHit { path: d.path.clone(), title: d.title.clone(), reason, score });
        }
    });
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, SearchOptions};
    use crate::index::Index;

    #[test]
    fn word_spans_skip_stopwords_and_split_compounds() {
        let norm = "the history-and-state of data";
        let words: Vec<&str> = word_spans(norm).iter().map(|&(s, e)| &norm[s as usize..e as usize]).collect();
        assert_eq!(words, ["history", "state", "data"]);
    }

    fn engine(paths: &[&str]) -> Engine {
        let mut e = Engine::new();
        for p in paths {
            e.upsert(p, 0.0, "");
        }
        e
    }

    fn reasons(e: &Engine, path: &str) -> Vec<(String, String)> {
        e.similar(path, 10, 0.70).into_iter().map(|h| (h.path, format!("{:?}", h.reason))).collect()
    }

    #[test]
    fn jaccard_reordered() {
        let e = engine(&["AI in Data Engineering.md", "Data Engineering with AI.md"]);
        assert_eq!(
            reasons(&e, "AI in Data Engineering.md"),
            [("Data Engineering with AI.md".to_string(), "SameWordsReordered".to_string())]
        );
        let a = vec!["ai".to_string(), "data".into(), "data".into()];
        let b = vec!["data".to_string(), "ai".into()];
        assert_eq!(title_jaccard(&a, &b), 1.0);
        assert_eq!(title_jaccard(&[], &[]), 0.0);
    }

    #[test]
    fn copy_suffix() {
        let e = engine(&[
            "RW MLOps is mostly data engineering.md",
            "RW MLOps Is Mostly Data Engineering..md",
            "RW Data Engineer.md",
            "RW Data Engineer-2.md",
        ]);
        let r = reasons(&e, "RW MLOps is mostly data engineering.md");
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].1, "CopySuffix");
        let r = reasons(&e, "RW Data Engineer.md");
        assert_eq!(r, [("RW Data Engineer-2.md".to_string(), "CopySuffix".to_string())]);
        assert_eq!(canonical_title("note (2)"), "note");
        assert_eq!(canonical_title("note copy 3"), "note");
        assert_eq!(canonical_title("note -2"), "note");
        assert_eq!(canonical_title("a  b copy."), "a b");
    }

    #[test]
    fn daily_notes_are_not_copies() {
        for t in ["2026-07-01", "2026-07", "2026", "meeting 2026-07-01", "v1.2-3"] {
            assert_eq!(canonical_title(t), t);
        }
        // One suffix at most.
        assert_eq!(canonical_title("note (1) (2)"), "note (1)");
        let e = engine(&["Daily/2026-07-01.md", "Daily/2026-07-02.md", "Daily/2026-07-10.md"]);
        let r = reasons(&e, "Daily/2026-07-01.md");
        assert!(r.is_empty(), "{r:?}");
        let r = e.search("2026-07-01", &SearchOptions::default());
        assert!(r.hits.iter().all(|h| h.similar_count == 0), "{:?}", r.hits.iter().map(|h| (&h.title, h.similar_count)).collect::<Vec<_>>());
        // Same prefix, different date: not a copy (title overlap may still apply).
        let e = engine(&["Meeting 2026-07-01.md", "Meeting 2026-07-02.md"]);
        let r = reasons(&e, "Meeting 2026-07-01.md");
        assert!(r.iter().all(|(_, why)| why != "CopySuffix"), "{r:?}");
    }

    #[test]
    fn same_title_different_folder() {
        let e = engine(&["a/Index.md", "b/Index.md"]);
        assert_eq!(reasons(&e, "a/Index.md"), [("b/Index.md".to_string(), "SameTitleDifferentFolder".to_string())]);
    }

    #[test]
    fn unrelated_not_similar() {
        let e = engine(&["Obsidian Troubleshooting.md", "Neovim Troubleshooting.md"]);
        assert!(e.similar("Obsidian Troubleshooting.md", 10, 0.70).is_empty());
        assert!(e.similar("missing.md", 10, 0.70).is_empty());
    }

    #[test]
    fn similar_title_band_and_limit() {
        // 3 of 4 shared tokens: 0.75.
        let e = engine(&["alpha beta gamma.md", "alpha beta gamma delta.md", "alpha beta gamma delta 2.md"]);
        let r = e.similar("alpha beta gamma.md", 10, 0.70);
        assert!(r.iter().any(|h| matches!(h.reason, SimilarReason::SimilarTitle(j) if (j - 0.75).abs() < 1e-6)));
        assert_eq!(e.similar("alpha beta gamma.md", 1, 0.70).len(), 1);
    }

    fn big_vault(n: usize) -> Engine {
        let mut e = Engine::new();
        for i in 0..n {
            e.upsert(&format!("Data Engineering {i}.md"), 0.0, "");
        }
        e.upsert("Data Engineering Roadmap.md", 0.0, "");
        e.upsert("Roadmap for Data Engineering.md", 0.0, "");
        e
    }

    #[test]
    fn common_tokens_skipped_still_finds_reordered() {
        let e = big_vault(600);
        for (a, b) in [
            ("Data Engineering Roadmap.md", "Roadmap for Data Engineering.md"),
            ("Roadmap for Data Engineering.md", "Data Engineering Roadmap.md"),
        ] {
            let r = e.similar(a, 10, 0.70);
            assert_eq!(r.len(), 1, "{r:?}");
            assert_eq!(r[0].path, b);
            assert!(matches!(r[0].reason, SimilarReason::SameWordsReordered));
        }
    }

    /// `n` titles built only from a small common vocabulary (every token has > 500 postings, no
    /// unique token), spread over folders so titles repeat.
    fn common_block(n: usize) -> Vec<String> {
        const V: [&str; 4] = ["Data", "Engineering", "Topic", "Notes"];
        (0..n).map(|i| {
            let (a, b, c) = (i % 4, (i / 4) % 4, (i / 16) % 4);
            let t = if a == b || b == c || a == c {
                format!("{} {}", V[a], V[b])
            } else {
                format!("{} {} {}", V[a], V[b], V[c])
            };
            format!("Folder {}/{t}.md", i % 97)
        })
        .collect()
    }

    #[test]
    fn candidate_cap_applies_when_all_tokens_common() {
        let mut index = Index::new();
        for p in common_block(3000) {
            index.upsert(crate::markdown::extract(&p, "", 0.0));
        }
        let id = index.ids().find(|&i| index.title_keys(i).unwrap().tokens.len() == 2).unwrap();
        let keys = index.title_keys(id).unwrap();
        assert!(keys.tokens.iter().all(|t| index.exact(Field::Title, t).len() > COMMON_TOKEN_DOCS));
        assert_eq!(candidates(&index, id, keys).len(), MAX_CANDIDATES);
        assert!(!similar_for(&index, id, 5).is_empty());
    }

    /// 7,000 unrelated notes plus the worst case: 3,000 titles made only of common tokens.
    fn timed_vault() -> Engine {
        let mut e = Engine::new();
        for i in 0..7000 {
            e.upsert(&format!("Folder {}/Other Note {i}.md", i % 20), i as f64, "");
        }
        for p in common_block(3000) {
            e.upsert(&p, 0.0, "");
        }
        e
    }

    fn best_search_us(e: &Engine) -> u64 {
        let o = SearchOptions::default();
        let _ = e.search("data engineering", &o);
        (0..5).map(|_| e.search("data engineering", &o).elapsed_us).min().unwrap()
    }

    #[test]
    fn similar_count_timing_smoke() {
        let e = timed_vault();
        let us = best_search_us(&e);
        eprintln!("search with similar_count: {us} us");
        assert!((us as u128) < 2_000_000 * crate::perf_slack(), "{us}");
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_budget_similar_count_10k() {
        let us = best_search_us(&timed_vault());
        eprintln!("search with similar_count: {us} us");
        assert!(us < 10_000, "{us}");
    }

    fn mlops() -> Engine {
        engine(&[
            "RW MLOps is mostly data engineering.md",
            "RW MLOps Is Mostly Data Engineering.md.md",
            "Obsidian Troubleshooting.md",
        ])
    }

    #[test]
    fn hits_carry_similar_count() {
        let mut e = mlops();
        e.upsert("Zzz Mlops Data Engineering Unrelated Words.md", 0.0, "");
        let r = e.search("mlops data engineering", &SearchOptions::default());
        let counts: Vec<(&str, u32)> = r.hits.iter().map(|h| (h.path.as_str(), h.similar_count)).collect();
        for (p, c) in &counts {
            if p.starts_with("RW MLOps") {
                assert_eq!(*c, 1, "{counts:?}");
            }
        }
        assert_eq!(counts.iter().filter(|c| c.0.starts_with("RW MLOps")).count(), 2);
    }

    #[test]
    fn similar_count_only_for_page() {
        let e = mlops();
        let r = e.search("obsidian", &SearchOptions::default());
        assert_eq!(r.hits[0].similar_count, 0);
        let r = e.search("", &SearchOptions::default());
        assert!(r.hits.iter().all(|h| h.similar_count == 0));
    }
}
