//! Engine: thin facade over the index, the ranking config and the frecency/combo/ghost state.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::index::{timed, Index, UpsertProfile};
use crate::markdown::extract_uncounted;
use crate::query::Query;
use crate::similar::{similar_count_for, similar_for, similar_ids_for, SimilarHit, SimilarReason};
use crate::vectors::{top, SemanticHit, VectorError, Vectors};
use crate::rank::{rank, Extras, GhostSet};
pub use crate::rank::{
    Config, DebugInfo, Excerpt, Ghost, HeadingHit, Hit, Mode, PartialMode, SearchOptions,
};

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub hits: Vec<Hit>,
    pub total_docs: usize,
    pub candidates: usize,
    pub elapsed_us: u64,
}

#[derive(Default)]
pub struct Engine {
    index: Index,
    config: Config,
    ghosts: GhostSet,
    /// Access timestamps (seconds, ascending) per note path, newest last.
    access: HashMap<String, VecDeque<f64>>,
    /// normalized query -> note path -> times opened from that query.
    combos: HashMap<String, HashMap<String, u32>>,
    /// Optional embedding matrix (keyed by path).
    vectors: Option<Vectors>,
}

/// Most access timestamps kept per path.
const MAX_ACCESS: usize = 128;
/// Page hits that get a `similar_count` (bounds the per-keystroke cost).
const SIMILAR_COUNT_HITS: usize = 30;
/// Page hits that also count semantic neighbours: one 10k x 384 scan costs ~0.3 ms in release
/// (30 scans ~9 ms, over the ~3 ms budget), so only the first 10 hits pay for it.
const SEMANTIC_COUNT_HITS: usize = 10;
/// Semantic neighbours merged into `similar`.
const SEMANTIC_NEIGHBOURS: usize = 5;
/// Cosine at or above which a semantic neighbour counts toward `similar_count`.
const SEMANTIC_COUNT_MIN: f32 = 0.80;

impl Engine {
    pub fn new() -> Self {
        Engine::default()
    }

    pub fn set_config(&mut self, c: Config) {
        self.config = c;
    }

    /// Replaces the unresolved link targets shown as ghost hits.
    pub fn set_ghosts(&mut self, ghosts: Vec<Ghost>) {
        self.ghosts = GhostSet::new(ghosts);
    }

    /// Replaces the access history of a path; keeps the newest 128 timestamps.
    pub fn set_access(&mut self, path: &str, mut timestamps_secs: Vec<f64>) {
        timestamps_secs.retain(|t| t.is_finite());
        timestamps_secs.sort_by(f64::total_cmp);
        let from = timestamps_secs.len().saturating_sub(MAX_ACCESS);
        self.access.insert(path.to_string(), timestamps_secs.split_off(from).into());
    }

    pub fn record_access(&mut self, path: &str, now_secs: f64) {
        let ts = self.access.entry(path.to_string()).or_default();
        ts.push_back(now_secs);
        if ts.len() > MAX_ACCESS {
            ts.pop_front();
        }
    }

    /// Forgets every access timestamp and combo count ("Clear frecency").
    pub fn clear_frecency(&mut self) {
        self.access.clear();
        self.combos.clear();
    }

    /// Counts one open of `path` from the query with this normalized form (the combo key).
    pub fn record_query_open(&mut self, normalized_query: &str, path: &str) {
        *self.combos.entry(normalized_query.to_string()).or_default().entry(path.to_string()).or_insert(0) += 1;
    }

    pub fn upsert(&mut self, path: &str, mtime: f64, text: &str) {
        self.index.upsert_with_text(extract_uncounted(path, text, mtime), text.to_owned());
    }

    /// `upsert`, adding each phase's wall time to `prof` (dev profiling: `ff10k profile`).
    pub fn upsert_profiled(&mut self, path: &str, mtime: f64, text: &str, prof: &mut UpsertProfile) {
        let doc = timed(Some(&mut prof.extract), || extract_uncounted(path, text, mtime));
        self.index.upsert_profiled(doc, text.to_owned(), prof);
    }

    pub fn remove(&mut self, path: &str) -> bool {
        self.index.remove(path).is_some()
    }

    pub fn rename(&mut self, old: &str, new: &str) -> bool {
        let renamed = self.index.rename(old, new);
        if renamed && old != new {
            if let Some(ts) = self.access.remove(old) {
                self.access.insert(new.to_string(), ts);
            }
            for paths in self.combos.values_mut() {
                if let Some(n) = paths.remove(old) {
                    paths.insert(new.to_string(), n);
                }
            }
        }
        renamed
    }

    pub fn doc_count(&self) -> usize {
        self.index.len()
    }

    /// Raw note text, for previews.
    pub fn doc_text(&self, path: &str) -> Option<&str> {
        self.index.text(self.index.id_of(path)?)
    }

    /// Replaces the embedding matrix. On error the old matrix is kept.
    pub fn set_vectors(&mut self, dims: usize, paths: Vec<String>, data: Vec<f32>) -> Result<(), VectorError> {
        self.vectors = Some(Vectors::new(dims, paths, data)?);
        Ok(())
    }

    pub fn clear_vectors(&mut self) {
        self.vectors = None;
    }

    pub fn has_vectors(&self) -> bool {
        self.vectors.is_some()
    }

    /// Top `limit` indexed notes by cosine to `query_vec` with `score >= min_score`. Empty
    /// without vectors or on a dimension mismatch.
    pub fn semantic(&self, query_vec: &[f32], limit: usize, min_score: f32) -> Vec<SemanticHit> {
        let Some(v) = &self.vectors else { return Vec::new() };
        let found = v.scan(query_vec, min_score, |p| self.index.id_of(p).is_some());
        top(found, limit)
            .into_iter()
            .filter_map(|(p, score)| {
                let d = self.index.get(self.index.id_of(p)?)?;
                Some(SemanticHit { path: p.to_string(), title: d.title.clone(), score })
            })
            .collect()
    }

    /// Near-duplicates of the note at `path`: title-based reasons first, then (with vectors) the
    /// top 5 semantic neighbours with `cos >= min_score`. A path with both keeps its lexical
    /// reason and gets `score = max(lexical, cos)`. Sorted by score, at most `limit`.
    pub fn similar(&self, path: &str, limit: usize, min_score: f32) -> Vec<SimilarHit> {
        let Some(id) = self.index.id_of(path) else { return Vec::new() };
        if limit == 0 {
            return Vec::new();
        }
        let mut out = similar_for(&self.index, id, usize::MAX);
        if let Some((v, q)) = self.vectors.as_ref().and_then(|v| Some((v, v.get(path)?))) {
            let found = v.scan(q, min_score, |p| p != path && self.index.id_of(p).is_some());
            for (p, cos) in top(found, SEMANTIC_NEIGHBOURS) {
                if let Some(h) = out.iter_mut().find(|h| h.path == p) {
                    h.score = h.score.max(cos);
                } else if let Some(d) = self.index.id_of(p).and_then(|i| self.index.get(i)) {
                    out.push(SimilarHit {
                        path: p.to_string(),
                        title: d.title.clone(),
                        reason: SimilarReason::Semantic(cos),
                        score: cos,
                    });
                }
            }
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
        out.truncate(limit);
        out
    }

    /// Lexical neighbours plus semantic neighbours (`cos >= 0.80`) not already counted.
    fn similar_count(&self, id: crate::index::DocId, path: &str, semantic: bool) -> u32 {
        let vq = self.vectors.as_ref().filter(|_| semantic).and_then(|v| Some((v, v.get(path)?)));
        let Some((v, q)) = vq else {
            return similar_count_for(&self.index, id);
        };
        let lexical: Vec<&str> = similar_ids_for(&self.index, id)
            .into_iter()
            .filter_map(|i| self.index.get(i).map(|d| d.path.as_str()))
            .collect();
        let sem = v.scan(q, SEMANTIC_COUNT_MIN, |p| p != path && self.index.id_of(p).is_some());
        (lexical.len() + sem.iter().filter(|(p, _)| !lexical.contains(p)).count()) as u32
    }

    pub fn search(&self, raw_query: &str, opts: &SearchOptions) -> SearchResponse {
        #[cfg(not(target_arch = "wasm32"))]
        let start = std::time::Instant::now();
        let query = Query::parse(raw_query);
        let extras = Extras { ghosts: Some(&self.ghosts), access: Some(&self.access), combos: Some(&self.combos) };
        let ranked = rank(&self.index, &query, &self.config, opts, &extras);
        let (mut hits, candidates) = (ranked.hits, ranked.candidates);
        // Neighbour counts only for the first page hits; the recent list (tier 0) stays instant.
        for (n, h) in hits.iter_mut().enumerate().take(SIMILAR_COUNT_HITS).filter(|(_, h)| !h.is_ghost && h.tier != 0) {
            if let Some(id) = self.index.id_of(&h.path) {
                h.similar_count = self.similar_count(id, &h.path, n < SEMANTIC_COUNT_HITS);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let elapsed_us = start.elapsed().as_micros() as u64;
        #[cfg(target_arch = "wasm32")]
        let elapsed_us = 0;
        SearchResponse { hits, total_docs: self.index.len(), candidates, elapsed_us }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Field;

    fn corpus() -> Engine {
        let mut e = Engine::new();
        for (path, text) in [
            ("AI in Data Engineering.md", ""),
            ("Data Engineering with AI.md", ""),
            ("👉🏻 Data Engineering.md", "## AI and the future of the role\n"),
            ("RW Pandas AI.md", "every data engineer will need"),
            ("Airflow Basics.md", ""),
            ("aim high.md", ""),
            ("Obsidian Troubleshooting.md", ""),
            ("Archive/Old AI.md", ""),
        ] {
            e.upsert(path, 0.0, text);
        }
        e
    }

    fn opts() -> SearchOptions {
        SearchOptions::default()
    }

    fn titles(r: &SearchResponse) -> Vec<&str> {
        r.hits.iter().map(|h| h.title.as_str()).collect()
    }

    fn hit<'a>(r: &'a SearchResponse, title: &str) -> Option<&'a Hit> {
        r.hits.iter().find(|h| h.title == title)
    }

    #[test]
    fn title_hits_are_tier1_and_first() {
        let r = corpus().search("data engineer ai", &opts());
        let mut first_two: Vec<&str> = titles(&r)[..2].to_vec();
        first_two.sort();
        assert_eq!(first_two, ["AI in Data Engineering", "Data Engineering with AI"]);
        assert!(r.hits[..2].iter().all(|h| h.tier == 1));
        let h = hit(&r, "👉🏻 Data Engineering").unwrap();
        assert_eq!(h.tier, 2);
        assert_eq!(h.heading.as_ref().unwrap().text, "AI and the future of the role");
        let h = hit(&r, "RW Pandas AI").unwrap();
        assert_eq!(h.tier, 3);
        assert_eq!(h.badge, Field::Body);
        assert!(h.excerpt.as_ref().unwrap().text.contains("data engineer"));
    }

    #[test]
    fn lower_tier_never_above_higher() {
        let e = corpus();
        for q in ["data engineer ai", "ai", "data engineering zzzz", "obsidian trouble", "ai data"] {
            let r = e.search(q, &opts());
            for w in r.hits.windows(2) {
                assert!(w[0].tier <= w[1].tier, "{q}: {:?}", titles(&r));
            }
        }
    }

    #[test]
    fn single_short_term_strict() {
        let r = corpus().search("AI", &opts());
        let t = titles(&r);
        assert!(!t.contains(&"Airflow Basics"), "{t:?}");
        assert!(!t.contains(&"aim high"), "{t:?}");
        assert!(t.contains(&"AI in Data Engineering"));
    }

    #[test]
    fn typo_in_title() {
        let r = corpus().search("obsidian troubelshoot", &opts());
        assert_eq!(r.hits[0].title, "Obsidian Troubleshooting");
    }

    #[test]
    fn files_only_mode_ignores_body() {
        let o = SearchOptions { mode: Mode::FilesOnly, ..opts() };
        let r = corpus().search("pandas data engineer", &o);
        if let Some(h) = hit(&r, "RW Pandas AI") {
            assert_eq!(h.tier, 4);
        }
        assert!(r.hits.iter().all(|h| h.tier == 1 || h.tier == 4));
    }

    #[test]
    fn downranked_folder_multiplier() {
        let mut e = corpus();
        e.set_config(Config { downranked_folders: vec!["Archive".into()], ..Config::default() });
        let r = e.search("ai", &opts());
        let old = hit(&r, "Old AI").unwrap();
        let other = hit(&r, "RW Pandas AI").unwrap();
        assert_eq!((old.tier, other.tier), (1, 1));
        assert!(old.score < other.score, "{} vs {}", old.score, other.score);
        let last_tier1 = r.hits.iter().rfind(|h| h.tier == 1).unwrap();
        assert_eq!(last_tier1.title, "Old AI");
    }

    #[test]
    fn partial_tier_only_when_few() {
        let mut e = corpus();
        let r = e.search("data engineering zzzz", &opts());
        assert!(!r.hits.is_empty());
        assert!(r.hits.iter().all(|h| h.tier == 4));
        e.set_config(Config { partial: PartialMode::Never, ..Config::default() });
        assert!(e.search("data engineering zzzz", &opts()).hits.is_empty());
    }

    #[test]
    fn phrase_requires_adjacency() {
        let mut e = corpus();
        e.upsert("Data Platform Engineering.md", 0.0, "");
        let r = e.search("\"data engineering\"", &opts());
        let t = titles(&r);
        assert!(!t.contains(&"Data Platform Engineering"), "{t:?}");
        assert!(t.contains(&"AI in Data Engineering"), "{t:?}");
    }

    #[test]
    fn filters_applied_before_scoring() {
        let e = corpus();
        let r = e.search("ai path:archive", &opts());
        assert_eq!(titles(&r), ["Old AI"]);
        let r = e.search("ai -archive", &opts());
        assert!(!titles(&r).contains(&"Old AI"));
        assert!(!r.hits.is_empty());
    }

    #[test]
    fn title_ranges_cover_matched_chars() {
        let r = corpus().search("obsidian trouble", &opts());
        let h = &r.hits[0];
        assert_eq!(h.title, "Obsidian Troubleshooting");
        let parts: Vec<&str> =
            h.title_ranges.iter().map(|&(s, e)| &h.title[s as usize..e as usize]).collect();
        assert_eq!(parts, ["Obsidian", "Trouble"]);
        assert_eq!(h.title_norm, "obsidian troubleshooting");
    }

    #[test]
    fn zero_weight_field_is_not_matched() {
        let mut e = corpus();
        assert!(titles(&e.search("engineer", &opts())).contains(&"RW Pandas AI"));
        let mut c = Config::default();
        c.weights.body = 0.0;
        e.set_config(c);
        let r = e.search("engineer", &opts());
        assert!(!titles(&r).contains(&"RW Pandas AI"), "{:?}", titles(&r));
        assert!(r.hits.iter().all(|h| h.tier != 3));
        assert!(titles(&r).contains(&"AI in Data Engineering"));
    }

    #[test]
    fn debug_info_present_when_requested() {
        let e = corpus();
        let r = e.search("obsidian trouble", &opts());
        assert!(r.hits[0].debug.is_none());
        let r = e.search("obsidian trouble", &SearchOptions { debug: true, ..opts() });
        let d = r.hits[0].debug.as_ref().unwrap();
        assert_eq!(d.per_term.len(), 2);
        assert!(d.components.iter().any(|(k, _)| k == "fields"));
    }

    #[test]
    fn fully_covered_title_beats_longer_title() {
        let mut e = Engine::new();
        e.upsert("Obsidian Troubleshoot.md", 0.0, "");
        e.upsert("Obsidian Trouble Tags Plugin.md", 0.0, "");
        let r = e.search("obsidian trouble", &opts());
        let short = hit(&r, "Obsidian Troubleshoot").unwrap();
        let long = hit(&r, "Obsidian Trouble Tags Plugin").unwrap();
        assert_eq!((short.tier, long.tier), (1, 1));
        assert!(short.score > long.score, "{} vs {}", short.score, long.score);
        assert_eq!(r.hits[0].title, "Obsidian Troubleshoot");
    }

    #[test]
    fn distinctive_word_rescues_long_title() {
        let mut e = Engine::new();
        e.upsert("Data Engineering.md", 0.0, "the fundamentals of the field");
        e.upsert("Fundamentals of Data Engineering (Joe Reis, Matt Housley).md", 0.0, "");
        let long = "Fundamentals of Data Engineering (Joe Reis, Matt Housley)";
        let r = e.search("data engineering", &opts());
        assert_eq!(r.hits[0].title, "Data Engineering");
        let before = hit(&r, long).unwrap().score;
        let r = e.search("fundamentals data engineering", &opts());
        assert_eq!((r.hits[0].title.as_str(), r.hits[0].tier), (long, 1));
        assert!(r.hits[0].score > before, "{} vs {before}", r.hits[0].score);
    }

    #[test]
    fn does_not_panic_on_syntax_only_queries() {
        let e = corpus();
        for q in ["#", "path:", "\"\"", "-", "   "] {
            // Empty after parsing: the recent list (tier 0), never an error.
            let r = e.search(q, &opts());
            assert!(r.hits.iter().all(|h| h.tier == 0), "{q:?}");
            assert_eq!(r.total_docs, 8);
        }
    }

    #[test]
    fn line_and_heading_for_page_hits() {
        let mut e = Engine::new();
        e.upsert("Note.md", 0.0, "intro\n\n## Setup\nfirst line\nthe kafka broker\n");
        let r = e.search("kafka broker", &opts());
        let h = &r.hits[0];
        assert_eq!(h.tier, 3);
        assert_eq!(h.line, Some(4));
        let ex = h.excerpt.as_ref().unwrap();
        let marked: Vec<&str> =
            ex.ranges.iter().map(|&(s, e)| &ex.text[s as usize..e as usize]).collect();
        assert_eq!(marked, ["kafka", "broker"]);
        let r = e.search("setup", &opts());
        let h = &r.hits[0];
        assert_eq!(h.badge, Field::H2);
        assert_eq!(h.line, Some(2));
        assert_eq!(h.heading.as_ref().unwrap().level, 2);

        // Frontmatter and fence-marker lines are not in the body; `line` is still the source line.
        e.upsert(
            "Fm.md",
            0.0,
            "---\ntags: [x]\naliases: [y]\nstatus: z\n---\nintro\n```\ncode\n```\nthe kafka zookeeper\n",
        );
        let r = e.search("kafka zookeeper", &opts());
        assert_eq!(r.hits[0].path, "Fm.md");
        assert_eq!(r.hits[0].line, Some(9));
    }

    #[test]
    fn short_exclude_is_exact_only() {
        let mut e = Engine::new();
        e.upsert("Pandas tips.md", 0.0, "we aim for speed");
        e.upsert("Pandas and AI.md", 0.0, "");
        e.upsert("Pandas notes.md", 0.0, "using ai with dataframes");
        let r = e.search("pandas -ai", &opts());
        assert_eq!(titles(&r), ["Pandas tips"]);
    }

    const NOW: f64 = 100_000_000.0;

    fn now_opts() -> SearchOptions {
        SearchOptions { now_secs: NOW, ..opts() }
    }

    #[test]
    fn recent_note_wins_tie() {
        let mut e = Engine::new();
        e.upsert("Meeting Notes C.md", 5.0, "");
        e.upsert("Meeting Notes B.md", 5.0, "");
        e.record_access("Meeting Notes B.md", NOW);
        let r = e.search("meeting notes", &now_opts());
        assert_eq!(r.hits[0].title, "Meeting Notes B");
        assert!(r.hits[0].score > r.hits[1].score);
        let r = e.search("meeting notes", &SearchOptions { debug: true, ..now_opts() });
        let comp = |h: &Hit, k: &str| h.debug.as_ref().unwrap().components.iter().find(|c| c.0 == k).unwrap().1;
        assert!((comp(&r.hits[0], "frecency") - 20.0).abs() < 0.01);
        assert_eq!(comp(&r.hits[1], "frecency"), 0.0);
    }

    #[test]
    fn combo_sticks() {
        let mut e = Engine::new();
        e.upsert("de.md", 0.0, "");
        e.upsert("x/Data Engineering.md", 0.0, "---\naliases: [DE]\n---\n");
        assert_eq!(e.search("de", &now_opts()).hits[0].path, "de.md");
        let key = Query::parse("de").normalized;
        for _ in 0..2 {
            e.record_query_open(&key, "x/Data Engineering.md");
        }
        assert_eq!(e.search("de", &now_opts()).hits[0].path, "de.md");
        e.record_query_open(&key, "x/Data Engineering.md");
        let r = e.search("de", &SearchOptions { debug: true, ..now_opts() });
        assert_eq!(r.hits[0].path, "x/Data Engineering.md");
        let combo = r.hits[0].debug.as_ref().unwrap().components.iter().find(|c| c.0 == "combo").unwrap().1;
        assert_eq!(combo, 1000.0);
    }

    #[test]
    fn clear_frecency_drops_access_and_combos() {
        let mut e = Engine::new();
        e.upsert("de.md", 0.0, "");
        e.upsert("x/Data Engineering.md", 0.0, "---\naliases: [DE]\n---\n");
        let key = Query::parse("de").normalized;
        for _ in 0..3 {
            e.record_query_open(&key, "x/Data Engineering.md");
        }
        e.record_access("de.md", now_opts().now_secs);
        e.clear_frecency();
        let r = e.search("de", &SearchOptions { debug: true, ..now_opts() });
        assert_eq!(r.hits[0].path, "de.md");
        for h in &r.hits {
            let c = &h.debug.as_ref().unwrap().components;
            assert_eq!(c.iter().find(|c| c.0 == "combo").unwrap().1, 0.0);
            assert_eq!(c.iter().find(|c| c.0 == "frecency").unwrap().1, 0.0);
        }
    }

    #[test]
    fn rename_keeps_frecency_and_combo() {
        let mut e = Engine::new();
        e.upsert("de.md", 0.0, "");
        e.upsert("Data Engineering.md", 0.0, "---\naliases: [DE]\n---\n");
        for _ in 0..3 {
            e.record_query_open("de", "Data Engineering.md");
        }
        e.record_access("Data Engineering.md", NOW);
        assert!(e.rename("Data Engineering.md", "Notes/Data Engineering.md"));
        let r = e.search("de", &SearchOptions { debug: true, ..now_opts() });
        assert_eq!(r.hits[0].path, "Notes/Data Engineering.md");
        let comps = &r.hits[0].debug.as_ref().unwrap().components;
        assert!(comps.iter().any(|c| c.0 == "combo" && c.1 == 1000.0));
        assert!(comps.iter().any(|c| c.0 == "frecency" && c.1 > 19.0));
    }

    #[test]
    fn access_history_capped_at_128() {
        let mut e = Engine::new();
        e.set_access("a.md", (0..200).map(|i| i as f64).collect());
        assert_eq!(e.access["a.md"].len(), 128);
        assert_eq!(e.access["a.md"][0], 72.0);
        for i in 0..200 {
            e.record_access("b.md", i as f64);
        }
        assert_eq!(e.access["b.md"].len(), 128);
        assert_eq!(e.access["b.md"][0], 72.0);
    }

    #[test]
    fn empty_query_returns_recent() {
        let mut e = Engine::new();
        for i in 0..30 {
            e.upsert(&format!("Note {i}.md"), i as f64, "");
        }
        e.set_ghosts(vec![Ghost { target: "Ghosty".into(), refs: 1 }]);
        e.record_access("Note 3.md", NOW - 86_400.0);
        e.record_access("Note 4.md", NOW);
        let r = e.search("", &now_opts());
        assert_eq!(r.hits.len(), 20);
        assert!(r.hits.iter().all(|h| h.tier == 0 && !h.is_ghost && h.badge == Field::Title && h.excerpt.is_none()));
        let t = titles(&r);
        assert_eq!(&t[..4], ["Note 4", "Note 3", "Note 29", "Note 28"]);
        let r = e.search("", &SearchOptions { limit: 5, ..now_opts() });
        assert_eq!(r.hits.len(), 5);
        // Filter-only queries keep the regular behaviour.
        assert!(e.search("-note", &opts()).hits.is_empty());
    }

    #[test]
    fn ghosts_appear_in_tier1() {
        let mut e = corpus();
        e.set_ghosts(vec![Ghost { target: "AI Data Engineer Workflows".into(), refs: 2 }]);
        let r = e.search("ai data engineer", &opts());
        let g = r.hits.iter().find(|h| h.is_ghost).unwrap();
        assert_eq!((g.ghost_refs, g.tier, g.badge), (2, 1, Field::Title));
        assert_eq!((g.path.as_str(), g.title.as_str()), ("AI Data Engineer Workflows", "AI Data Engineer Workflows"));
        assert!(!g.title_ranges.is_empty());
        // Real tier-1 docs with a higher score come first; ghosts never rank above equal real docs.
        let files = SearchOptions { mode: Mode::FilesOnly, ..opts() };
        assert!(e.search("ai data engineer", &files).hits.iter().any(|h| h.is_ghost));
        // Filters and excludes.
        assert!(!e.search("ai data engineer -workflows", &opts()).hits.iter().any(|h| h.is_ghost));
        assert!(!e.search("ai data engineer #tag", &opts()).hits.iter().any(|h| h.is_ghost));
        assert!(!e.search("ai data engineer path:ai", &opts()).hits.iter().any(|h| h.is_ghost));
        // Typo, and the short-term rule.
        assert!(e.search("workflos", &opts()).hits.iter().any(|h| h.is_ghost));
        e.set_ghosts(vec![Ghost { target: "Airflow Ghost".into(), refs: 1 }]);
        assert!(!e.search("ai", &opts()).hits.iter().any(|h| h.is_ghost));
        e.set_ghosts(Vec::new());
        assert!(e.search("airflow ghost", &opts()).hits.iter().all(|h| !h.is_ghost));
    }

    #[test]
    fn ghost_sorts_after_equal_real_doc() {
        let mut e = Engine::new();
        e.set_ghosts(vec![Ghost { target: "Kafka Tips Aaa".into(), refs: 1 }]);
        e.upsert("Kafka Tips Zzz.md", 0.0, "");
        let r = e.search("kafka tips", &opts());
        assert_eq!(r.hits.len(), 2);
        assert_eq!(r.hits[0].score, r.hits[1].score);
        assert_eq!((r.hits[0].is_ghost, r.hits[1].is_ghost), (false, true));
    }

    #[test]
    fn ghost_removed_when_note_created() {
        let mut e = corpus();
        e.set_ghosts(vec![Ghost { target: "AI Data Engineer Workflows".into(), refs: 2 }]);
        assert!(e.search("ai data engineer", &opts()).hits.iter().any(|h| h.is_ghost));
        e.upsert("AI Data Engineer Workflows.md", 0.0, "");
        let r = e.search("ai data engineer", &opts());
        assert!(!r.hits.iter().any(|h| h.is_ghost));
        assert!(r.hits.iter().any(|h| h.title == "AI Data Engineer Workflows"));
        // Resolved by path (folder note) as well.
        e.set_ghosts(vec![Ghost { target: "Projects/Roadmap Q3".into(), refs: 1 }]);
        assert!(e.search("roadmap", &opts()).hits.iter().any(|h| h.is_ghost));
        e.upsert("Projects/Roadmap Q3.md", 0.0, "");
        assert!(!e.search("roadmap", &opts()).hits.iter().any(|h| h.is_ghost));
    }

    #[test]
    fn engine_mutations() {
        let mut e = corpus();
        assert_eq!(e.doc_count(), 8);
        assert_eq!(e.doc_text("RW Pandas AI.md"), Some("every data engineer will need"));
        assert!(e.rename("aim high.md", "Goals/aim higher.md"));
        assert!(e.remove("Goals/aim higher.md"));
        assert!(!e.remove("Goals/aim higher.md"));
        assert_eq!(e.doc_count(), 7);
    }

    #[test]
    fn performance_smoke_2000_docs() {
        const LOREM: &str = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do \
            eiusmod tempor incididunt ut labore et dolore magna aliqua enim";
        let body: String = (0..10).map(|_| LOREM).collect::<Vec<_>>().join(" ");
        assert_eq!(body.split_whitespace().count(), 200);
        let mut e = Engine::new();
        for i in 0..2000 {
            e.upsert(&format!("Note {i}.md"), i as f64, &body);
        }
        let _ = e.search("note", &opts()); // warm the field_strings cache
        let t = std::time::Instant::now();
        let r = e.search("note dolor", &opts());
        let dt = t.elapsed();
        assert_eq!(r.hits.len(), 50);
        assert_eq!(r.candidates, 2000);
        assert!(dt.as_millis() < 100, "{dt:?}");
    }

    /// Release-only budget guard (spec §6.6: p95 < 10 ms for 10k notes).
    #[cfg(not(debug_assertions))]
    #[test]
    fn release_budget_10k_docs() {
        const LOREM: &str = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do \
            eiusmod tempor incididunt ut labore et dolore magna aliqua enim";
        let body: String = (0..10).map(|_| LOREM).collect::<Vec<_>>().join(" ");
        let mut e = Engine::new();
        e.set_config(Config { downranked_folders: vec!["Archive".into()], ..Config::default() });
        for i in 0..10_000 {
            let dir = if i % 10 == 0 { "Archive".to_string() } else { format!("Folder {}", i % 20) };
            e.upsert(&format!("{dir}/Note {i}.md"), i as f64, &body);
        }
        let _ = e.search("note", &opts()); // warm the field_strings cache
        let times: Vec<(&str, u64)> = ["note", "dolor", "note dolor", "path:folder"]
            .into_iter()
            .map(|q| (q, (0..5).map(|_| e.search(q, &opts()).elapsed_us).min().unwrap()))
            .collect();
        eprintln!("{times:?}");
        assert!(times.iter().all(|t| t.1 < 10_000), "{times:?}");
    }

    // ---- vectors ----

    /// Unit vector in 3 dims from an angle.
    fn unit(deg: f32) -> [f32; 3] {
        let r = deg.to_radians();
        [r.cos(), r.sin(), 0.0]
    }

    fn with_vectors(rows: &[(&str, f32)]) -> Engine {
        let mut e = corpus();
        let paths = rows.iter().map(|r| r.0.to_string()).collect();
        let data = rows.iter().flat_map(|r| unit(r.1)).collect();
        e.set_vectors(3, paths, data).unwrap();
        e
    }

    #[test]
    fn semantic_filters_to_indexed_paths() {
        let e = with_vectors(&[("AI in Data Engineering.md", 0.0), ("Not In Index.md", 1.0), ("Airflow Basics.md", 30.0)]);
        let r = e.semantic(&unit(0.0), 10, 0.0);
        let paths: Vec<&str> = r.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["AI in Data Engineering.md", "Airflow Basics.md"]);
        assert_eq!(r[0].title, "AI in Data Engineering");
        assert!((r[0].score - 1.0).abs() < 1e-5);
        assert_eq!(e.semantic(&unit(0.0), 1, 0.0).len(), 1);
        assert_eq!(e.semantic(&unit(0.0), 10, 0.95).len(), 1);
    }

    #[test]
    fn similar_merges_semantic() {
        let e = with_vectors(&[
            ("AI in Data Engineering.md", 0.0),
            ("Data Engineering with AI.md", 10.0), // lexical + cos 0.985
            ("RW Pandas AI.md", 20.0),             // semantic only, 0.94
            ("Airflow Basics.md", 80.0),           // below 0.70
        ]);
        let r = e.similar("AI in Data Engineering.md", 10, 0.70);
        let by: Vec<(&str, String)> = r.iter().map(|h| (h.path.as_str(), format!("{:?}", h.reason))).collect();
        assert_eq!(by.len(), 3, "{by:?}"); // + lexical SimilarTitle for the emoji note
        assert_eq!(by[0], ("Data Engineering with AI.md", "SameWordsReordered".to_string()));
        assert!(matches!(r[1].reason, SimilarReason::Semantic(s) if s >= 0.70));
        assert!(r.iter().all(|h| h.path != "Airflow Basics.md"));
        assert!(r[0].score > 0.98 && r[0].score <= 1.0);
        let mut ps: Vec<&str> = r.iter().map(|h| h.path.as_str()).collect();
        ps.sort();
        ps.dedup();
        assert_eq!(ps.len(), r.len());
        assert_eq!(e.similar("AI in Data Engineering.md", 1, 0.70).len(), 1);
    }

    #[test]
    fn similar_count_includes_uncounted_semantic() {
        let e = with_vectors(&[
            ("AI in Data Engineering.md", 0.0),
            ("Data Engineering with AI.md", 10.0), // lexical, also >= 0.80: counted once
            ("RW Pandas AI.md", 20.0),             // semantic only >= 0.80
            ("Airflow Basics.md", 40.0),           // 0.77: not counted
        ]);
        let r = e.search("ai in data engineering", &opts());
        let h = hit(&r, "AI in Data Engineering").unwrap();
        assert_eq!(h.similar_count, 3); // 2 lexical + RW Pandas AI
        let mut e = e;
        e.clear_vectors();
        let r = e.search("ai in data engineering", &opts());
        assert_eq!(hit(&r, "AI in Data Engineering").unwrap().similar_count, 2);
    }

    #[test]
    fn engine_without_vectors_still_searches() {
        let e = corpus();
        assert!(!e.has_vectors());
        assert!(e.semantic(&[1.0, 0.0, 0.0], 5, 0.0).is_empty());
        assert!(!e.search("airflow", &opts()).hits.is_empty());
    }

    #[test]
    fn set_vectors_error_keeps_old_matrix() {
        let mut e = with_vectors(&[("AI in Data Engineering.md", 0.0)]);
        assert_eq!(e.set_vectors(0, vec![], vec![]), Err(VectorError::BadDims));
        assert_eq!(
            e.set_vectors(3, vec!["a.md".into()], vec![1.0]),
            Err(VectorError::DimsMismatch { expected: 3, got_len: 1 })
        );
        assert!(e.has_vectors());
        assert_eq!(e.semantic(&unit(0.0), 5, 0.0).len(), 1);
    }

    #[test]
    fn semantic_dims_mismatch_returns_empty() {
        let e = with_vectors(&[("AI in Data Engineering.md", 0.0)]);
        assert!(e.semantic(&[1.0, 0.0], 5, 0.0).is_empty());
        assert!(e.semantic(&[1.0, 0.0, 0.0, 0.0], 5, 0.0).is_empty());
    }

    /// Synthetic 10k x 384 matrix over a 10k-note index.
    fn big_vector_engine() -> Engine {
        let mut e = Engine::new();
        let mut paths = Vec::new();
        for i in 0..10_000 {
            let p = format!("Folder {}/Note {i}.md", i % 50);
            e.upsert(&p, 0.0, "");
            paths.push(p);
        }
        let mut x: u64 = 0x9E3779B97F4A7C15;
        let mut data = Vec::with_capacity(10_000 * 384);
        for _ in 0..10_000 {
            let row: Vec<f32> = (0..384)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    (x % 2001) as f32 / 1000.0 - 1.0
                })
                .collect();
            let n = row.iter().map(|v| v * v).sum::<f32>().sqrt();
            data.extend(row.iter().map(|v| v / n));
        }
        e.set_vectors(384, paths, data).unwrap();
        e
    }

    #[cfg(not(debug_assertions))]
    fn best_us(mut f: impl FnMut()) -> u128 {
        f();
        (0..7).map(|_| {
            let t = std::time::Instant::now();
            f();
            t.elapsed().as_micros()
        })
        .min()
        .unwrap()
    }

    #[test]
    fn semantic_timing_smoke() {
        let e = big_vector_engine();
        let q: Vec<f32> = (0..384).map(|i| (i % 7) as f32 - 3.0).collect();
        assert_eq!(e.semantic(&q, 10, -1.0).len(), 10);
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_budget_semantic_10k() {
        let e = big_vector_engine();
        let q: Vec<f32> = (0..384).map(|i| (i % 7) as f32 - 3.0).collect();
        let us = best_us(|| {
            std::hint::black_box(e.semantic(&q, 10, 0.3));
        });
        eprintln!("semantic() 10k x 384: {us} us");
        assert!(us < 2_000, "{us}");
        // similar_count semantic path for 30 hits = 10 scans (timing note, not asserted).
        let us30 = best_us(|| {
            for i in 0..10 {
                std::hint::black_box(e.similar_count(e.index.id_of(&format!("Folder {}/Note {i}.md", i % 50)).unwrap(), &format!("Folder {}/Note {i}.md", i % 50), true));
            }
        });
        eprintln!("similar_count x10 with vectors: {us30} us");
    }
}
