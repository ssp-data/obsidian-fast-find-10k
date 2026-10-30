//! Engine: thin facade over the index, the ranking config and the frecency/combo/ghost state.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::canvas;
use crate::index::{Index, UpsertProfile};
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
    /// `similar_count` per (path, with semantic neighbours), reused across keystrokes (a
    /// semantic count is a full matrix scan); cleared by every index or vector mutation.
    similar_counts: RefCell<HashMap<(String, bool), u32>>,
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

impl Engine {
    pub fn new() -> Self {
        Engine::default()
    }

    pub fn set_config(&mut self, c: Config) {
        self.config = c;
        self.similar_counts.borrow_mut().clear(); // the similar floor may have changed
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
        let md;
        let text = if canvas::is_canvas(path) {
            md = canvas::canvas_to_markdown(text);
            md.as_str()
        } else {
            text
        };
        self.index.upsert_note(path, mtime, text);
        self.similar_counts.get_mut().clear();
    }

    /// `upsert`, adding each phase's wall time to `prof` (dev profiling: `ff10k profile`).
    pub fn upsert_profiled(&mut self, path: &str, mtime: f64, text: &str, prof: &mut UpsertProfile) {
        self.index.upsert_note_profiled(path, mtime, text, prof);
        self.similar_counts.get_mut().clear();
    }

    /// Drops every indexed note and the index caches at once (for a rebuild); access history,
    /// combos, ghosts, vectors and the config stay.
    pub fn clear(&mut self) {
        self.index = Index::new();
        self.similar_counts.get_mut().clear();
    }

    /// Builds the lazy per-query caches of the index and the ghost set, so the first query
    /// after a batch of mutations is not the one paying for them (the worker calls this once
    /// it is idle after mutations).
    pub fn warm_up(&self) {
        self.index.warm_up();
        self.ghosts.warm_up();
    }

    pub fn remove(&mut self, path: &str) -> bool {
        self.similar_counts.get_mut().clear();
        self.index.remove(path).is_some()
    }

    pub fn rename(&mut self, old: &str, new: &str) -> bool {
        self.similar_counts.get_mut().clear();
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

    /// Every indexed note (for the golden runner's name checks).
    #[cfg(feature = "golden")]
    pub(crate) fn docs(&self) -> impl Iterator<Item = &crate::doc::Doc> + '_ {
        self.index.ids().filter_map(|id| self.index.get(id))
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
        self.similar_counts.get_mut().clear();
        Ok(())
    }

    pub fn clear_vectors(&mut self) {
        self.vectors = None;
        self.similar_counts.get_mut().clear();
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

    /// What `similar(path, …, config.similar_min_score)` would list: lexical neighbours plus the
    /// top `SEMANTIC_NEIGHBOURS` semantic ones above the floor that are not already counted.
    fn similar_count(&self, id: crate::index::DocId, path: &str, semantic: bool) -> u32 {
        let vq = self.vectors.as_ref().filter(|_| semantic).and_then(|v| Some((v, v.get(path)?)));
        let Some((v, q)) = vq else {
            return similar_count_for(&self.index, id);
        };
        let lexical: Vec<&str> = similar_ids_for(&self.index, id)
            .into_iter()
            .filter_map(|i| self.index.get(i).map(|d| d.path.as_str()))
            .collect();
        let sem = v.scan(q, self.config.similar_min_score, |p| p != path && self.index.id_of(p).is_some());
        (lexical.len() + top(sem, SEMANTIC_NEIGHBOURS).iter().filter(|(p, _)| !lexical.contains(p)).count()) as u32
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
                let key = (h.path.clone(), n < SEMANTIC_COUNT_HITS);
                let cached = self.similar_counts.borrow().get(&key).copied();
                h.similar_count = cached.unwrap_or_else(|| {
                    let c = self.similar_count(id, &h.path, key.1);
                    self.similar_counts.borrow_mut().insert(key, c);
                    c
                });
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
    use crate::doc::Weights;
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
        assert!(dt.as_millis() < 100 * crate::perf_slack(), "{dt:?}");
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
        assert!(times.iter().all(|t| (t.1 as u128) < 10_000 * crate::perf_slack()), "{times:?}");
    }

    // ---- vectors ----

    /// Unit vector in 3 dims from an angle.
    fn unit(deg: f32) -> [f32; 3] {
        let r = deg.to_radians();
        [r.cos(), r.sin(), 0.0]
    }

    #[test]
    fn warm_up_primes_index_and_ghost_caches() {
        let mut e = corpus();
        e.set_ghosts(vec![Ghost { target: "Smart Note Taking".into(), refs: 2 }]);
        e.warm_up();
        assert!(e.index.caches_built());
        assert!(e.ghosts.caches_built());
        let r = e.search("data engineering", &opts());
        assert_eq!(r.hits[0].tier, 1);
    }

    #[test]
    fn clear_drops_docs_keeps_frecency_combos_and_vectors() {
        let mut e = with_vectors(&[("RW Pandas AI.md", 0.0)]);
        e.record_access("RW Pandas AI.md", 1_000.0);
        e.record_query_open("pandas", "RW Pandas AI.md");
        e.clear();
        assert_eq!(e.doc_count(), 0);
        assert_eq!(e.doc_text("RW Pandas AI.md"), None);
        assert!(e.search("pandas", &opts()).hits.iter().all(|h| h.is_ghost));
        assert!(e.has_vectors());
        assert_eq!(e.access["RW Pandas AI.md"].len(), 1);
        assert_eq!(e.combos["pandas"]["RW Pandas AI.md"], 1);
        // Re-indexing after a clear works like a fresh engine.
        e.upsert("RW Pandas AI.md", 0.0, "every data engineer will need");
        assert_eq!(e.search("pandas", &opts()).hits[0].path, "RW Pandas AI.md");
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
    fn similar_count_cache_follows_mutations() {
        let mut e = with_vectors(&[("AI in Data Engineering.md", 0.0), ("RW Pandas AI.md", 20.0)]);
        let count = |e: &Engine| hit(&e.search("ai in data engineering", &opts()), "AI in Data Engineering").unwrap().similar_count;
        let first = count(&e);
        assert_eq!(e.similar_counts.borrow().len() > 0, true, "counts cached");
        assert_eq!(count(&e), first);
        e.upsert("AI in Data Engineering (1).md", 0.0, "");
        assert_eq!(count(&e), first + 1, "an upsert invalidates the cache");
        assert!(e.remove("AI in Data Engineering (1).md"));
        assert_eq!(count(&e), first);
        e.clear_vectors();
        assert_eq!(count(&e), first - 1, "dropping vectors invalidates the cache");
    }

    #[test]
    fn similar_count_includes_uncounted_semantic() {
        let mut e = with_vectors(&[
            ("AI in Data Engineering.md", 0.0),
            ("Data Engineering with AI.md", 10.0), // lexical, also >= 0.80: counted once
            ("RW Pandas AI.md", 20.0),             // semantic only >= 0.80
            ("Airflow Basics.md", 40.0),           // 0.77: not counted at a 0.80 floor
        ]);
        e.set_config(Config { similar_min_score: 0.80, ..Config::default() });
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
        assert!(us < 2_000 * crate::perf_slack(), "{us}");
        // similar_count semantic path for 30 hits = 10 scans (timing note, not asserted).
        let us30 = best_us(|| {
            for i in 0..10 {
                std::hint::black_box(e.similar_count(e.index.id_of(&format!("Folder {}/Note {i}.md", i % 50)).unwrap(), &format!("Folder {}/Note {i}.md", i % 50), true));
            }
        });
        eprintln!("similar_count x10 with vectors: {us30} us");
    }

    fn optional_corpus() -> Engine {
        let mut e = Engine::new();
        for path in ["Should I Change My Writing because of AI.md", "Change Should Happen.md", "Should I Change.md", "Should Change.md"] {
            e.upsert(path, 0.0, "");
        }
        e
    }

    #[test]
    fn optional_term_not_required() {
        let r = optional_corpus().search("should i change", &opts());
        assert_eq!(hit(&r, "Should I Change My Writing because of AI").unwrap().tier, 1);
        assert_eq!(hit(&r, "Change Should Happen").unwrap().tier, 1);
        // Without `i` on the list it is required again: the note lacking it drops out of tier 1.
        let mut e = optional_corpus();
        e.set_config(Config { optional_terms: Vec::new(), ..Config::default() });
        let r = e.search("should i change", &opts());
        assert!(hit(&r, "Change Should Happen").is_none_or(|h| h.tier > 1));
    }

    #[test]
    fn optional_term_still_scores() {
        let r = optional_corpus().search("should i change", &SearchOptions { debug: true, ..opts() });
        let (with, without) = (hit(&r, "Should I Change").unwrap(), hit(&r, "Should Change").unwrap());
        assert!(with.score > without.score, "{} vs {}", with.score, without.score);
        let terms = |h: &Hit| h.debug.as_ref().unwrap().per_term.iter().map(|t| t.0.clone()).collect::<Vec<_>>();
        assert_eq!(terms(with), ["should", "i", "change"]);
        assert_eq!(terms(without), ["should", "change"]);
    }

    #[test]
    fn optional_terms_do_not_make_a_query_multi_term() {
        let mut e = Engine::new();
        e.upsert("Airflow Basics.md", 0.0, "data pipelines and the scheduler");
        e.upsert("AI Notes.md", 0.0, "data and the rest");
        let r = e.search("ai the", &opts());
        assert!(hit(&r, "Airflow Basics").is_none(), "{:?}", titles(&r));
        assert!(hit(&r, "AI Notes").is_some());
        // A required second term still lets `ai` prefix-match.
        assert!(hit(&e.search("ai data", &opts()), "Airflow Basics").is_some());
    }

    #[test]
    fn optional_only_query_behaves_as_today() {
        let mut e = corpus();
        let with = titles(&e.search("in", &opts())).iter().map(|t| t.to_string()).collect::<Vec<_>>();
        e.set_config(Config { optional_terms: Vec::new(), ..Config::default() });
        let without = titles(&e.search("in", &opts())).iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert!(!with.is_empty());
        assert_eq!(with, without);
    }

    #[test]
    fn config_without_optional_terms_uses_the_defaults() {
        let c: Config = serde_json::from_str(r#"{"weights":{"title":100,"alias":90,"h1":60,"h2":50,"h3":40,"tag":30,"link":20,"path":15,"body":10},"fuzzy":{"enabled":true,"loose":false},"partial":"whenFew","downrankedFolders":[],"downrankFactor":0.3,"fewThreshold":5}"#).unwrap();
        assert_eq!(c.optional_terms, ["a", "an", "the", "of", "to", "in", "on", "i"]);
        let c: Config = serde_json::from_str(&serde_json::to_string(&Config { optional_terms: vec!["I".into()], ..Config::default() }).unwrap()).unwrap();
        assert_eq!(c.optional_terms, ["I"]);
    }

    fn synonym_corpus() -> Engine {
        let mut e = Engine::new();
        e.set_config(Config {
            synonyms: vec!["oss: open source, open-source software".into(), "k8s = kubernetes".into()],
            ..Config::default()
        });
        for (path, text) in [
            ("Open Source.md", "licenses and communities, a long enough body to avoid the short-note factor, words words words words words words words"),
            ("Open Source Software.md", "communities and foundations, a long enough body to avoid the short-note factor, words words words words words words"),
            ("Why I Love Open Source.md", "communities everywhere, a long enough body to avoid the short-note factor, words words words words words words words"),
            ("OSS Licenses.md", "mit and apache, a long enough body to avoid the short-note factor, words words words words words words words words words"),
            ("Source Code Open Day.md", "source first, then open: not the phrase; a long enough body to avoid the short-note factor, words words words words"),
            ("Boss Fight.md", "game notes, a long enough body to avoid the short-note factor, words words words words words words words words words"),
            ("Kubernetes Basics.md", "pods, a long enough body to avoid the short-note factor, words words words words words words words words words words"),
            ("K8s Cheatsheet.md", "kubectl, a long enough body to avoid the short-note factor, words words words words words words words words words"),
            ("Open Hardware.md", "the body says open-source software once; a long enough body to avoid the short-note factor, words words words words"),
        ] {
            e.upsert(path, 0.0, text);
        }
        e
    }

    fn component(h: &Hit, name: &str) -> f32 {
        h.debug.as_ref().unwrap().components.iter().find(|(k, _)| k == name).unwrap().1
    }

    #[test]
    fn abbreviation_expands_to_its_phrases() {
        let r = synonym_corpus().search("oss", &SearchOptions { debug: true, ..opts() });
        for t in ["Open Source", "Open Source Software", "Why I Love Open Source", "OSS Licenses"] {
            assert_eq!(hit(&r, t).map(|h| h.tier), Some(1), "{t}: {:?}", titles(&r));
        }
        // The phrase must be adjacent, and the typed short word keeps its exact-only rule.
        assert!(hit(&r, "Source Code Open Day").is_none(), "{:?}", titles(&r));
        assert!(hit(&r, "Boss Fight").is_none(), "{:?}", titles(&r));
        // The body-only alternative `open-source software` gives tier 3.
        assert_eq!(hit(&r, "Open Hardware").map(|h| h.tier), Some(3));
        // Expansion words cover the title for the unmatched-word factor: "Open Source" is fully
        // covered and ranks first; "OSS Licenses" has one unmatched word.
        assert_eq!(component(hit(&r, "Open Source").unwrap(), "titleFactor"), 1.0);
        assert_eq!(component(hit(&r, "OSS Licenses").unwrap(), "titleFactor"), 0.85);
        assert_eq!(titles(&r)[0], "Open Source");
        // Debug keeps the typed word as the label.
        assert_eq!(hit(&r, "Open Source").unwrap().debug.as_ref().unwrap().per_term[0].0, "oss");
    }

    #[test]
    fn single_word_members_expand_both_ways() {
        let e = synonym_corpus();
        assert_eq!(hit(&e.search("k8s", &opts()), "Kubernetes Basics").map(|h| h.tier), Some(1));
        assert_eq!(hit(&e.search("kubernetes", &opts()), "K8s Cheatsheet").map(|h| h.tier), Some(1));
        // Case and punctuation of the setting do not matter.
        let mut e = synonym_corpus();
        e.set_config(Config { synonyms: vec!["  K8S ,  Kubernetes ".into()], ..Config::default() });
        assert_eq!(hit(&e.search("k8s", &opts()), "Kubernetes Basics").map(|h| h.tier), Some(1));
    }

    #[test]
    fn multi_word_members_are_targets_only() {
        let r = synonym_corpus().search("open source", &opts());
        assert!(hit(&r, "OSS Licenses").is_none_or(|h| h.tier > 1), "{:?}", titles(&r));
        assert_eq!(hit(&r, "Source Code Open Day").map(|h| h.tier), Some(1));
    }

    #[test]
    fn abbreviation_combines_with_other_terms() {
        let e = synonym_corpus();
        let r = e.search("oss licenses", &opts());
        assert_eq!(titles(&r)[0], "OSS Licenses");
        assert_eq!(hit(&r, "Open Source").map(|h| h.tier), Some(3));
        let r = e.search("oss communities", &opts());
        assert_eq!(hit(&r, "Open Source Software").map(|h| h.tier), Some(3), "{:?}", titles(&r));
        assert!(hit(&r, "Boss Fight").is_none());
        // Files-only mode: the body alternative does not count.
        let r = e.search("oss", &SearchOptions { mode: Mode::FilesOnly, ..opts() });
        assert!(hit(&r, "Open Hardware").is_none());
        assert_eq!(hit(&r, "Open Source Software").map(|h| h.tier), Some(1));
    }

    #[test]
    fn abbreviation_results_are_identical_without_the_setting_when_unused() {
        let mut e = synonym_corpus();
        let with: Vec<String> = titles(&e.search("boss fight", &opts())).iter().map(|t| t.to_string()).collect();
        e.set_config(Config::default());
        let without: Vec<String> = titles(&e.search("boss fight", &opts())).iter().map(|t| t.to_string()).collect();
        assert_eq!(with, without);
        assert!(hit(&e.search("oss", &opts()), "Open Source").is_none());
    }

    #[test]
    fn config_without_synonyms_is_empty() {
        let c: Config = serde_json::from_str(r#"{"weights":{"title":100,"alias":90,"h1":60,"h2":50,"h3":40,"tag":30,"link":20,"path":15,"body":10},"fuzzy":{"enabled":true,"loose":false},"partial":"whenFew","downrankedFolders":[],"downrankFactor":0.3,"fewThreshold":5}"#).unwrap();
        assert!(c.synonyms.is_empty());
        let c: Config = serde_json::from_str(&serde_json::to_string(&Config { synonyms: vec!["oss: open source".into()], ..Config::default() }).unwrap()).unwrap();
        assert_eq!(c.synonyms, ["oss: open source"]);
    }

    // ---- recall fixes (docs/recall.md, owner-approved 2026-10-27) ----

    #[test]
    fn hash_number_in_title_is_a_word_when_no_such_tag_exists() {
        let mut e = Engine::new();
        e.upsert("Newsletter #115.md", 0.0, "issue one hundred fifteen of the newsletter, with enough words to pass the short note rule here");
        e.upsert("Newsletter #116.md", 0.0, "issue one hundred sixteen of the newsletter, with enough words to pass the short note rule here");
        e.upsert("Tagged.md", 0.0, "a note with the tag #ai and some more words so it is not short, words words words words words");
        let r = e.search("newsletter #115", &opts());
        assert_eq!(titles(&r).first().copied(), Some("Newsletter #115"), "{:?}", titles(&r));
        // `116` is not `115`: the other issue is at most a partial (tier 4) hit on `newsletter`.
        assert!(hit(&r, "Newsletter #116").is_none_or(|h| h.tier == 4), "{:?}", titles(&r));
        // A tag that exists keeps filtering.
        let r = e.search("#ai", &opts());
        assert_eq!(titles(&r), ["Tagged"]);
        // A tag that does not exist, alone, is a word that matches nothing (not "every note").
        assert!(e.search("#zzz", &opts()).hits.is_empty());
    }

    #[test]
    fn dash_word_is_a_word_when_excluding_leaves_nothing() {
        let mut e = Engine::new();
        e.upsert("KW13 -Marktleistung.md", 0.0, "Kalenderwoche dreizehn, with enough words to pass the short note rule, words words words words words");
        e.upsert("Plan.md", 0.0, "this is still a draft plan, with enough words to pass the short note rule, words words words words words");
        e.upsert("Plan B.md", 0.0, "the final plan, with enough words to pass the short note rule, words words words words words words words");
        let r = e.search("KW13 -Marktleistung", &opts());
        assert_eq!(titles(&r).first().copied(), Some("KW13 -Marktleistung"), "{:?}", titles(&r));
        // An exclusion that leaves results keeps excluding.
        let r = e.search("plan -draft", &opts());
        assert_eq!(titles(&r), ["Plan B"]);
    }

    #[test]
    fn dotted_name_is_found_by_its_own_title() {
        let mut e = Engine::new();
        e.upsert("jasper.ai.md", 0.0, "The future of writing with [[Artificial intelligence (AI)]], with enough words to pass the short note rule words");
        e.upsert("Artificial intelligence (AI).md", 0.0, "ai everywhere, with enough words to pass the short note rule, words words words words words words");
        e.upsert("OpenAI.md", 0.0, "company notes, ai, with enough words to pass the short note rule, words words words words words words");
        e.upsert("Jasper AI.md", 0.0, "the same product written as two words, with enough words to pass the short note rule, words words words");
        let r = e.search("jasper.ai", &SearchOptions { debug: true, ..opts() });
        assert_eq!(titles(&r).first().copied(), Some("jasper.ai"), "{:?}", titles(&r));
        assert_eq!(component(hit(&r, "jasper.ai").unwrap(), "titleFactor"), 1.0);
        // Notes that have only `ai` do not match the compound at all.
        assert!(hit(&r, "OpenAI").is_none(), "{:?}", titles(&r));
        // The two-word spelling matches through the parts and is fully covered.
        assert_eq!(hit(&r, "Jasper AI").map(|h| h.tier), Some(1));
        assert_eq!(component(hit(&r, "Jasper AI").unwrap(), "titleFactor"), 1.0);
        // The other direction: the two-word query finds the dotted title in tier 1.
        assert_eq!(hit(&e.search("jasper ai", &opts()), "jasper.ai").map(|h| h.tier), Some(1));
    }

    #[test]
    fn five_letter_query_gets_one_typo_in_title_and_alias_only() {
        let mut e = Engine::new();
        e.upsert("GitHub.md", 0.0, "code hosting, with enough words to pass the short note rule, words words words words words words words");
        e.upsert("Ollama.md", 0.0, "local models, with enough words to pass the short note rule, words words words words words words words");
        e.upsert("Tools.md", 0.0, "---\naliases: [Mentor]\n---\nwe use github daily, with enough words to pass the short note rule, words words words");
        e.upsert("Alfred MacOS.md", 0.0, "launcher, with enough words to pass the short note rule, words words words words words words words");
        e.upsert("Heads.md", 0.0, "## GitHub\nheading only, with enough words to pass the short note rule, words words words words words");
        let r = e.search("gitub", &opts());
        assert_eq!(hit(&r, "GitHub").map(|h| h.tier), Some(1), "{:?}", titles(&r));
        // Body and heading occurrences are not fuzzy-matched at 5 letters.
        assert!(hit(&r, "Tools").is_none(), "{:?}", titles(&r));
        assert!(hit(&r, "Heads").is_none(), "{:?}", titles(&r));
        assert_eq!(hit(&e.search("ollma", &opts()), "Ollama").map(|h| h.tier), Some(1));
        // Aliases count as titles.
        assert_eq!(hit(&e.search("menor", &opts()), "Tools").map(|h| h.tier), Some(1));
        // Also inside a multi-word query.
        assert_eq!(hit(&e.search("alfed macos", &opts()), "Alfred MacOS").map(|h| h.tier), Some(1));
        // Fuzzy off: nothing.
        e.set_config(Config { fuzzy: crate::matcher::FuzzyLevel { enabled: false, loose: false }, ..Config::default() });
        assert!(hit(&e.search("gitub", &opts()), "GitHub").is_none());
    }

    #[test]
    fn whole_title_equal_to_the_query_wins_the_tie() {
        let mut e = Engine::new();
        e.upsert("data.md", 0.0, "");
        e.upsert("DataOps.md", 0.0, "dataops is a practice, with enough words to pass the short note rule, words words words words words words");
        e.upsert("DataFusion.md", 0.0, "query engine, with enough words to pass the short note rule, words words words words words words words");
        for i in 0..6 {
            e.upsert(&format!("Link {i}.md"), 0.0, "see [[DataOps]] and [[DataFusion]] for more, with enough words to pass the short note rule, words words");
        }
        let r = e.search("data", &SearchOptions { debug: true, ..opts() });
        assert_eq!(titles(&r).first().copied(), Some("data"), "{:?}", titles(&r));
        // Order-free: "AI Writing" equals the query `writing ai`.
        e.upsert("AI Writing.md", 0.0, "writing with ai, with enough words to pass the short note rule, words words words words words words");
        e.upsert("How to detect AI Writing.md", 0.0, "detecting, with enough words to pass the short note rule, words words words words words words words");
        let r = e.search("writing ai", &opts());
        assert_eq!(titles(&r).first().copied(), Some("AI Writing"), "{:?}", titles(&r));
    }

    #[test]
    fn german_dictionary_expands_english_and_german_words_when_enabled() {
        let mut e = Engine::new();
        for (path, text) in [
            ("Empfangsstruktur (Schule Madretsch).md", "Notizen zur Empfangsstruktur der Schule, genug Wörter um die Kurznotiz-Regel zu umgehen, Wörter Wörter Wörter"),
            ("School Notes.md", "notes about the school, with enough words to pass the short note rule, words words words words words"),
            ("Schulen im Kanton.md", "Liste der Schulen, genug Wörter um die Kurznotiz-Regel zu umgehen, Wörter Wörter Wörter Wörter Wörter"),
            ("Gift Ideas.md", "presents for the family, with enough words to pass the short note rule, words words words words words"),
            ("Kinder und Familie.md", "Notizen zur Familie, genug Wörter um die Kurznotiz-Regel zu umgehen, Wörter Wörter Wörter Wörter Wörter"),
        ] {
            e.upsert(path, 0.0, text);
        }
        // Off by default: `school` is English only.
        let r = e.search("school", &opts());
        assert_eq!(titles(&r), ["School Notes"]);
        e.set_config(Config { languages: vec!["de".into()], ..Config::default() });
        let r = e.search("school", &opts());
        assert_eq!(hit(&r, "Empfangsstruktur (Schule Madretsch)").map(|h| h.tier), Some(1), "{:?}", titles(&r));
        // Prefix: `schule` → "Schulen".
        assert_eq!(hit(&r, "Schulen im Kanton").map(|h| h.tier), Some(1), "{:?}", titles(&r));
        assert_eq!(hit(&r, "School Notes").map(|h| h.tier), Some(1));
        // German → English too, and umlauts fold (`Schüler` is on the list as a different word).
        assert_eq!(hit(&e.search("schule", &opts()), "School Notes").map(|h| h.tier), Some(1));
        assert_eq!(hit(&e.search("children", &opts()), "Kinder und Familie").map(|h| h.tier), Some(1));
        // False friends are not on the list: `poison` must not find "Gift Ideas".
        assert!(e.search("poison", &opts()).hits.is_empty());
        // Unknown languages are ignored; the owner's own lines still apply next to the dictionary.
        e.set_config(Config { languages: vec!["xx".into(), "German".into()], synonyms: vec!["empfang: reception".into()], ..Config::default() });
        assert_eq!(hit(&e.search("reception", &opts()), "Empfangsstruktur (Schule Madretsch)").map(|h| h.tier), Some(1));
        assert_eq!(hit(&e.search("school", &opts()), "Schulen im Kanton").map(|h| h.tier), Some(1));
    }

    #[test]
    fn german_dictionary_parses_and_serializes() {
        let text = crate::rank::dictionary("de").unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).collect();
        assert!(lines.len() > 300, "{}", lines.len());
        for l in &lines {
            assert!(l.contains(':'), "{l:?}");
            assert!(!l.contains("gift") || !l.starts_with("poison"), "false friend: {l:?}");
        }
        assert!(crate::rank::dictionary("fr").is_none());
        let c: Config = serde_json::from_str(&serde_json::to_string(&Config { languages: vec!["de".into()], ..Config::default() }).unwrap()).unwrap();
        assert_eq!(c.languages, ["de"]);
        let c: Config = serde_json::from_str(r#"{"weights":{"title":100,"alias":90,"h1":60,"h2":50,"h3":40,"tag":30,"link":20,"path":15,"body":10},"fuzzy":{"enabled":true,"loose":false},"partial":"whenFew","downrankedFolders":[],"downrankFactor":0.3,"fewThreshold":5}"#).unwrap();
        assert!(c.languages.is_empty());
    }

    #[test]
    fn repeated_title_word_counts_as_covered() {
        let mut e = Engine::new();
        e.upsert("1 Chr 1.md", 0.0, "first chapter, with enough words to pass the short note rule, words words words words words words words");
        e.upsert("1 Chr 11.md", 0.0, "eleventh chapter, with enough words to pass the short note rule, words words words words words words");
        e.upsert("1 Chr 10.md", 0.0, "tenth chapter, with enough words to pass the short note rule, words words words words words words words");
        let r = e.search("1 chr 1", &SearchOptions { debug: true, ..opts() });
        assert_eq!(component(hit(&r, "1 Chr 1").unwrap(), "titleFactor"), 1.0);
        assert_eq!(titles(&r)[0], "1 Chr 1", "{:?}", titles(&r));
    }

    #[test]
    fn exact_and_prefix_hits_rank_above_fuzzy_hits_in_the_same_tier() {
        let mut e = Engine::new();
        e.upsert("Invoce Template.md", 0.0, "a typo in the title, with enough words to pass the short note rule, words words words words words");
        e.upsert("Brand Voice.md", 0.0, "tone of voice guidelines, with enough words to pass the short note rule, words words words words");
        e.upsert("Sales Invoice Process and Cashflow Notes.md", 0.0, "how invoices are sent, with enough words to pass the short note rule, words words words words");
        e.upsert("Invoices Overview.md", 0.0, "all invoices, with enough words to pass the short note rule, words words words words words words");
        let r = e.search("invoice", &SearchOptions { debug: true, ..opts() });
        let t = titles(&r);
        let pos = |name: &str| t.iter().position(|x| *x == name).unwrap_or(usize::MAX);
        // Without the rule the short fuzzy title scores higher (one unmatched word vs four).
        let typo = hit(&r, "Invoce Template").unwrap();
        assert_eq!(typo.debug.as_ref().unwrap().per_term[0].2, crate::matcher::MatchKind::Fuzzy);
        assert!(pos("Sales Invoice Process and Cashflow Notes") < pos("Invoce Template"), "{t:?}");
        assert!(pos("Invoices Overview") < pos("Invoce Template"), "{t:?}");
        assert_eq!(typo.tier, 1);
        // `n` + `voice` is not a typo of `invoice`: no hit at all.
        assert!(hit(&r, "Brand Voice").is_none(), "{t:?}");
        // A typo in the query leaves every hit fuzzy: the score order is untouched.
        let r = e.search("invoce", &opts());
        assert!(hit(&r, "Invoce Template").is_some() || hit(&r, "Invoices Overview").is_some());
    }

    #[test]
    fn hits_through_an_alternative_are_flagged_expanded() {
        let r = synonym_corpus().search("oss", &opts());
        assert!(hit(&r, "Open Source").unwrap().expanded);
        assert!(hit(&r, "Open Hardware").unwrap().expanded);
        assert!(!hit(&r, "OSS Licenses").unwrap().expanded);
        // The typed word's own hit wins over an equal alternative hit on the same field.
        let r = synonym_corpus().search("kubernetes", &opts());
        assert!(!hit(&r, "Kubernetes Basics").unwrap().expanded);
        assert!(hit(&r, "K8s Cheatsheet").unwrap().expanded);
        // Without groups nothing is expanded.
        let mut e = synonym_corpus();
        e.set_config(Config::default());
        assert!(e.search("open source", &opts()).hits.iter().all(|h| !h.expanded));
    }

    #[test]
    fn similar_count_pill_matches_the_panel_list() {
        // "AI in Data Engineering" and "RW Pandas AI" are near in vector space (cos ≈ 0.98).
        let mut e = with_vectors(&[("AI in Data Engineering.md", 0.0), ("RW Pandas AI.md", 0.2), ("Airflow Basics.md", 30.0)]);
        let check = |e: &Engine, min: f32| {
            let r = e.search("ai in data engineering", &opts());
            let h = hit(&r, "AI in Data Engineering").unwrap();
            let panel = e.similar(&h.path, 100, min);
            assert_eq!(h.similar_count as usize, panel.len(), "floor {min}: pill {} vs panel {:?}", h.similar_count, panel.iter().map(|x| &x.title).collect::<Vec<_>>());
            h.similar_count
        };
        let low = check(&e, 0.5);
        e.set_config(Config { similar_min_score: 0.99, ..Config::default() });
        let high = check(&e, 0.99);
        assert!(low > high, "{low} > {high}");
    }

    // ---- properties (frontmatter) ----

    fn coffee_corpus() -> Engine {
        let mut e = Engine::new();
        e.upsert("Coffee/KAFOJ Espresso.md", 0.0, "---\nCreated: 2025-02-08\nRösterei: KAFOJ\nHerkunftsland:\n  - India\nRating: 5\nPrice: 4\n---\n# KAFOJ Espresso\nBiel coffee from the shop, with enough words to pass the short note rule, words words words words words");
        e.upsert("Coffee/Adrianos Espresso.md", 0.0, "---\nRösterei: Adrianos\nHerkunftsland: [Brazil, Colombia]\nPrice: 6\n---\n# Adrianos Espresso\nBern coffee, with enough words to pass the short note rule, words words words words words words words words");
        e.upsert("Tea Notes.md", 0.0, "Darjeeling from India, with enough words to pass the short note rule, words words words words words words words");
        e.upsert("Espresso Machines.md", 0.0, "Machines overview, with enough words to pass the short note rule, words words words words words words words");
        e
    }

    #[test]
    fn property_values_match_in_the_content_tier_with_their_own_badge() {
        let e = coffee_corpus();
        let r = e.search("india espresso", &SearchOptions { debug: true, ..opts() });
        let h = hit(&r, "KAFOJ Espresso").unwrap();
        assert_eq!(h.tier, 3, "{:?}", titles(&r));
        assert_eq!(h.badge, Field::Property);
        let ex = h.excerpt.as_ref().unwrap();
        assert_eq!(ex.text, "Herkunftsland: India");
        assert_eq!(ex.ranges, vec![(15, 20)]);
        assert!(hit(&r, "Tea Notes").is_none(), "{:?}", titles(&r));
        // keys match too
        assert_eq!(hit(&e.search("rösterei kafoj", &opts()), "KAFOJ Espresso").map(|h| h.tier), Some(3));
        // weight 0 turns the field off
        let mut off = coffee_corpus();
        let mut w = Weights::default();
        w.property = 0.0;
        off.set_config(Config { weights: w, ..Config::default() });
        // only the partial tier (espresso in the title) remains: india no longer matches anywhere
        assert!(hit(&off.search("india espresso", &opts()), "KAFOJ Espresso").is_none_or(|h| h.tier == 4));
    }

    #[test]
    fn property_filters_narrow_like_tags_and_fall_back_to_words() {
        let e = coffee_corpus();
        assert_eq!(titles(&e.search("[herkunftsland:india]", &opts())), ["KAFOJ Espresso"]);
        assert_eq!(titles(&e.search("[Herkunftsland:Colombia] espresso", &opts())), ["Adrianos Espresso"]);
        let r = e.search("[price]", &opts());
        let both = titles(&r);
        assert_eq!(both.len(), 2, "{both:?}");
        assert!(both.contains(&"KAFOJ Espresso") && both.contains(&"Adrianos Espresso"));
        assert!(e.search("[price:5]", &opts()).hits.is_empty());
        // reserved keys are not properties: `[created:…]` has no key to filter on and becomes words
        assert!(e.search("[created:2025-02-08]", &opts()).hits.is_empty());
        // an unknown key is searched as words: `[machines]` → `machines` matches the title
        assert_eq!(hit(&e.search("[machines]", &opts()), "Espresso Machines").map(|h| h.tier), Some(1));
        // `[machines:overview]` → both words; `overview` is body-only, so the content tier
        assert_eq!(hit(&e.search("[machines:overview]", &opts()), "Espresso Machines").map(|h| h.tier), Some(3));
    }

    #[test]
    fn property_keys_follow_updates_and_removal() {
        let mut e = coffee_corpus();
        assert_eq!(e.search("[rating]", &opts()).hits.len(), 1);
        e.upsert("Coffee/KAFOJ Espresso.md", 1.0, "---\nRösterei: KAFOJ\n---\n# KAFOJ Espresso\nno rating any more, with enough words to pass the short note rule, words words words words");
        // the key is gone from every note: `[rating]` is now the word `rating`
        assert_eq!(titles(&e.search("[rating]", &opts())), ["KAFOJ Espresso"]);
        e.remove("Coffee/Adrianos Espresso.md");
        assert!(e.search("[herkunftsland:colombia]", &opts()).hits.is_empty());
    }

    #[test]
    fn canvas_files_are_indexed_as_markdown_and_preview_that_text() {
        let mut e = Engine::new();
        e.upsert("Writing/Book Plan.canvas", 0.0, r#"{"nodes":[{"id":"a","type":"group","label":"Brainstorming","x":0,"y":0,"width":1,"height":1},{"id":"b","type":"text","text":"Chapter about zettelkasten workflows and why folders fail, with enough words to pass the short note rule","x":0,"y":0,"width":1,"height":1},{"id":"c","type":"file","file":"Notes/Outline.md","x":0,"y":0,"width":1,"height":1}],"edges":[]}"#);
        e.upsert("Notes/Outline.md", 0.0, "the outline, with enough words to pass the short note rule, words words words words words words words words");
        let r = e.search("zettelkasten workflows", &opts());
        assert_eq!(hit(&r, "Book Plan").map(|h| h.tier), Some(3), "{:?}", titles(&r));
        assert_eq!(hit(&e.search("book plan", &opts()), "Book Plan").map(|h| h.tier), Some(1));
        assert_eq!(hit(&e.search("brainstorming", &opts()), "Book Plan").map(|h| h.tier), Some(2));
        let text = e.doc_text("Writing/Book Plan.canvas").unwrap();
        assert!(text.starts_with("## Brainstorming\n\nChapter about"), "{text}");
        assert!(text.contains("[[Notes/Outline.md]]"));
        // the file node is a link: the outline gets a backlink from the canvas
        assert_eq!(e.search("outline", &SearchOptions { debug: true, ..opts() }).hits[0].title, "Outline");
    }

    #[test]
    fn type_filter_keeps_one_file_kind() {
        let mut e = Engine::new();
        e.upsert("Coffee/Coffee Beans Base.base", 0.0, "description: |\n  Coffee beans list\nviews:\n  - type: table\n");
        e.upsert("Coffee/Coffee Beans.md", 0.0, "coffee beans notes, with enough words to pass the short note rule, words words words words words words");
        e.upsert("Coffee/Coffee Map.canvas", 0.0, r#"{"nodes":[{"id":"a","type":"text","text":"coffee beans by region","x":0,"y":0,"width":1,"height":1}]}"#);
        e.upsert("Coffee/Coffee Sketch.excalidraw", 0.0, "# Excalidraw Data\n## Text Elements\ncoffee beans ^ab12cd34\n");
        let kinds = |q: &str| {
            let mut t = titles(&e.search(q, &opts())).iter().map(|t| t.to_string()).collect::<Vec<_>>();
            t.sort();
            t
        };
        assert_eq!(kinds("type:base coffee"), ["Coffee Beans Base"]);
        assert_eq!(kinds("type:canvas coffee"), ["Coffee Map"]);
        assert_eq!(kinds("type:drawing coffee"), ["Coffee Sketch"]);
        assert_eq!(kinds("type:note coffee"), ["Coffee Beans"]);
        assert_eq!(kinds("type:base type:canvas coffee"), ["Coffee Beans Base", "Coffee Map"]);
        assert_eq!(kinds("type:base"), ["Coffee Beans Base"]);
        assert_eq!(kinds("coffee").len(), 4);
    }
}
