//! Engine: thin facade over the index, the ranking config and the frecency/combo/ghost state.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::index::Index;
use crate::markdown::extract;
use crate::query::Query;
use crate::similar::{similar_for, SimilarHit};
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
}

/// Most access timestamps kept per path.
const MAX_ACCESS: usize = 128;
/// Page hits that get a `similar_count` (bounds the per-keystroke cost).
const SIMILAR_COUNT_HITS: usize = 30;

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

    /// Counts one open of `path` from the query with this normalized form (the combo key).
    pub fn record_query_open(&mut self, normalized_query: &str, path: &str) {
        *self.combos.entry(normalized_query.to_string()).or_default().entry(path.to_string()).or_insert(0) += 1;
    }

    pub fn upsert(&mut self, path: &str, mtime: f64, text: &str) {
        self.index.upsert_with_text(extract(path, text, mtime), text.to_owned());
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

    /// Title-based near-duplicates of the note at `path`, best first.
    pub fn similar(&self, path: &str, limit: usize) -> Vec<SimilarHit> {
        self.index.id_of(path).map_or_else(Vec::new, |id| similar_for(&self.index, id, limit))
    }

    pub fn search(&self, raw_query: &str, opts: &SearchOptions) -> SearchResponse {
        #[cfg(not(target_arch = "wasm32"))]
        let start = std::time::Instant::now();
        let query = Query::parse(raw_query);
        let extras = Extras { ghosts: Some(&self.ghosts), access: Some(&self.access), combos: Some(&self.combos) };
        let ranked = rank(&self.index, &query, &self.config, opts, &extras);
        let (mut hits, candidates) = (ranked.hits, ranked.candidates);
        // Neighbour counts only for the first page hits; the recent list (tier 0) stays instant.
        for h in hits.iter_mut().take(SIMILAR_COUNT_HITS).filter(|h| !h.is_ghost && h.tier != 0) {
            if let Some(id) = self.index.id_of(&h.path) {
                h.similar_count = similar_for(&self.index, id, usize::MAX).len() as u32;
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
        e.upsert("Meeting Notes A.md", 5.0, "");
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
}
