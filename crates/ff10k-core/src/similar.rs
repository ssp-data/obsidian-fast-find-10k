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
    let rest = rest.strip_suffix(' ').unwrap_or(rest);
    (!rest.is_empty()).then_some(rest)
}

/// Normalized title without trailing ` -N`, `-N`, ` (N)`, ` copy`, ` copy N`, trailing
/// punctuation and double spaces.
pub fn canonical_title(norm_title: &str) -> String {
    let mut s = norm_title.split_whitespace().collect::<Vec<_>>().join(" ");
    loop {
        let trimmed = s.trim_end_matches(['.', ',', ';', ':', '!', '?', ' ']);
        let next = strip_suffix_once(trimmed).unwrap_or(trimmed);
        if next.len() == s.len() {
            return s;
        }
        s = next.trim_end().to_string();
    }
}

/// Deduplicated non-stopword tokens of a raw title.
fn title_token_set(title: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    tokenize(&strip_symbols(title))
        .into_iter()
        .map(|t| t.text)
        .filter(|t| !STOPWORDS.contains(&t.as_str()) && seen.insert(t.clone()))
        .collect()
}

fn folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(f, _)| f)
}

/// Lexical (title-based) neighbours of `id`, best first, at most `limit`.
pub fn similar_for(index: &Index, id: DocId, limit: usize) -> Vec<SimilarHit> {
    let Some(doc) = index.get(id) else { return Vec::new() };
    let tokens = title_token_set(&doc.title);
    if tokens.is_empty() || limit == 0 {
        return Vec::new();
    }
    let lists: Vec<_> = tokens.iter().map(|t| index.exact(Field::Title, t)).collect();
    let all_common = lists.iter().all(|l| l.len() > COMMON_TOKEN_DOCS);
    let mut shared: HashMap<DocId, u32> = HashMap::new();
    for l in &lists {
        if l.len() > COMMON_TOKEN_DOCS && !all_common {
            continue;
        }
        for p in l.iter() {
            if p.doc != id {
                *shared.entry(p.doc).or_insert(0) += 1;
            }
        }
    }
    let min_shared = if tokens.len() <= 2 { 1 } else { 2 };
    let mut cands: Vec<(DocId, u32)> = shared.into_iter().filter(|c| c.1 >= min_shared).collect();
    cands.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    cands.truncate(MAX_CANDIDATES);

    let norm = normalize(&strip_symbols(&doc.title));
    let canon = canonical_title(&norm);
    let mut out: Vec<SimilarHit> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (cid, _) in cands {
        let Some(other) = index.get(cid) else { continue };
        if other.path == doc.path || !seen.insert(other.path.as_str()) {
            continue;
        }
        let other_norm = normalize(&strip_symbols(&other.title));
        let j = title_jaccard(&tokens, &title_token_set(&other.title));
        let (reason, score) = if canonical_title(&other_norm) == canon && other.title != doc.title {
            (SimilarReason::CopySuffix, 1.0)
        } else if other_norm == norm && folder(&other.path) != folder(&doc.path) {
            (SimilarReason::SameTitleDifferentFolder, 1.0)
        } else if j >= 0.8 {
            (SimilarReason::SameWordsReordered, j)
        } else if j >= 0.6 {
            (SimilarReason::SimilarTitle(j), j)
        } else {
            continue;
        };
        out.push(SimilarHit { path: other.path.clone(), title: other.title.clone(), reason, score });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, SearchOptions};

    fn engine(paths: &[&str]) -> Engine {
        let mut e = Engine::new();
        for p in paths {
            e.upsert(p, 0.0, "");
        }
        e
    }

    fn reasons(e: &Engine, path: &str) -> Vec<(String, String)> {
        e.similar(path, 10).into_iter().map(|h| (h.path, format!("{:?}", h.reason))).collect()
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
    fn same_title_different_folder() {
        let e = engine(&["a/Index.md", "b/Index.md"]);
        assert_eq!(reasons(&e, "a/Index.md"), [("b/Index.md".to_string(), "SameTitleDifferentFolder".to_string())]);
    }

    #[test]
    fn unrelated_not_similar() {
        let e = engine(&["Obsidian Troubleshooting.md", "Neovim Troubleshooting.md"]);
        assert!(e.similar("Obsidian Troubleshooting.md", 10).is_empty());
        assert!(e.similar("missing.md", 10).is_empty());
    }

    #[test]
    fn similar_title_band_and_limit() {
        // 3 of 4 shared tokens: 0.75.
        let e = engine(&["alpha beta gamma.md", "alpha beta gamma delta.md", "alpha beta gamma delta 2.md"]);
        let r = e.similar("alpha beta gamma.md", 10);
        assert!(r.iter().any(|h| matches!(h.reason, SimilarReason::SimilarTitle(j) if (j - 0.75).abs() < 1e-6)));
        assert_eq!(e.similar("alpha beta gamma.md", 1).len(), 1);
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
