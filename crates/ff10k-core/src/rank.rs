//! Ranking: candidate generation, hard tiers (spec §6.3) and the in-tier score (spec §6.4).
//!
//! Every query term and every quoted phrase is one AND-ed *unit*. A doc's tier comes from each
//! unit's best field class (Title/Alias → 1, H1–H3 → 2, anything else → 3); docs missing a unit
//! but hitting another in Title/Alias are partial (tier 4). Within a tier the score is
//! `(Σ fields × title factor + proximity + structural + frecency + combo) × penalty`; the title
//! factor shrinks with each title word the query left unmatched (tiers 1–3). A term on
//! `Config::optional_terms` is optional: it never makes or breaks a candidate or its tier, but
//! adds to the score where it hits (a query of only optional terms treats them all as required).
//! Hit details (badge, heading, excerpt, line) are computed only for the returned page.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use smallvec::{smallvec, SmallVec};

use crate::doc::{Doc, Field, Weights};
use crate::frecency::frecency_score;
use crate::markdown::extract;
use crate::index::{DocId, Index};
use crate::matcher::{body_docs, match_term, match_term_scoped, DocSet, FuzzyLevel, MatchKind, Scope, TermHit};
use crate::normalize::{is_whitespace_like, normalize, strip_symbols};
use crate::query::{Query, Term};
use crate::tokenize::tokenize_query_term;

/// Proximity: bonus when all units share a heading section, larger when within this many body tokens.
const PROXIMITY_WINDOW: u32 = 8;
const NEAR_BONUS: f32 = 25.0;
const SECTION_BONUS: f32 = 15.0;
/// Tiers 1–3: fields × `max(FLOOR, 1 − STEP × unmatched non-stopword title words)`.
const TITLE_UNMATCHED_STEP: f32 = 0.15;
const TITLE_UNMATCHED_FLOOR: f32 = 0.5;
const BACKLINK_BONUS: f32 = 2.0;
/// Tier 1, when the title's words are exactly the query's words (any order): `data` → "data"
/// above "DataOps" and "DataFusion", which also count as fully covered (docs/recall.md fix 4).
const WHOLE_TITLE_BONUS: f32 = 15.0;
const SHORT_NOTE_WORDS: u32 = 20;
const SHORT_NOTE_FACTOR: f32 = 0.8;
const FRECENCY_WEIGHT: f32 = 20.0;
const COMBO_BONUS: f32 = 1000.0;
const COMBO_MIN_COUNT: u32 = 3;
/// Empty query: size of the "recent" list.
const RECENT_LIMIT: usize = 20;
/// Excerpt: bytes of context on each side of the first body match.
const EXCERPT_RADIUS: usize = 60;
const NO_FUZZY: FuzzyLevel = FuzzyLevel { enabled: false, loose: false };

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    #[default]
    All,
    FilesOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub enum PartialMode {
    #[default]
    WhenFew,
    Always,
    Never,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub weights: Weights,
    pub fuzzy: FuzzyLevel,
    pub partial: PartialMode,
    pub downranked_folders: Vec<String>,
    pub downrank_factor: f32,
    pub few_threshold: usize,
    /// Query terms (compared normalized) that are never required, only scored when they hit.
    #[serde(default = "default_optional_terms")]
    pub optional_terms: Vec<String>,
    /// Abbreviation groups, one per line: members separated by `,` (the first may end in `:` or
    /// `=`), e.g. `oss: open source, open-source software`. A typed single-word member also
    /// matches every other member (single words as terms, several words as a phrase).
    #[serde(default)]
    pub synonyms: Vec<String>,
}

/// Stopwords and single letters: `a an the of to in on i`.
pub fn default_optional_terms() -> Vec<String> {
    ["a", "an", "the", "of", "to", "in", "on", "i"].map(String::from).to_vec()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            weights: Weights::default(),
            fuzzy: FuzzyLevel { enabled: true, loose: false },
            partial: PartialMode::WhenFew,
            downranked_folders: Vec::new(),
            downrank_factor: 0.3,
            few_threshold: 5,
            optional_terms: default_optional_terms(),
            synonyms: Vec::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SearchOptions {
    pub mode: Mode,
    pub limit: usize,
    pub now_secs: f64,
    pub debug: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions { mode: Mode::All, limit: 50, now_secs: 0.0, debug: false }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HeadingHit {
    pub text: String,
    pub line: u32,
    pub level: u8,
}

/// `ranges` are byte ranges into `text`.
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Excerpt {
    pub text: String,
    pub ranges: Vec<(u32, u32)>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DebugInfo {
    pub per_term: Vec<(String, Field, MatchKind, f32)>,
    pub components: Vec<(String, f32)>,
}

/// `title_ranges` are byte ranges into `title_norm` (the normalized title, or the matched alias
/// when the badge is `Alias` and no term hit the title).
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub path: String,
    pub title: String,
    pub tier: u8,
    pub score: f32,
    pub badge: Field,
    pub title_norm: String,
    pub title_ranges: Vec<(u32, u32)>,
    pub heading: Option<HeadingHit>,
    pub excerpt: Option<Excerpt>,
    pub line: Option<u32>,
    pub is_ghost: bool,
    pub ghost_refs: u32,
    pub similar_count: u32,
    pub debug: Option<DebugInfo>,
}

/// An unresolved link target.
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Ghost {
    pub target: String,
    pub refs: u32,
}

/// Ghosts plus a private index of empty ghost docs, so they match through the normal matcher.
/// Ghost `i` is `DocId` `i` (ids are handed out sequentially into a fresh index).
#[derive(Default)]
pub struct GhostSet {
    ghosts: Vec<Ghost>,
    index: Index,
}

impl GhostSet {
    /// Builds the lazy caches of the private ghost index (see `Index::warm_up`).
    pub fn warm_up(&self) {
        self.index.warm_up();
    }

    pub fn caches_built(&self) -> bool {
        self.index.caches_built()
    }

    /// Duplicate and empty targets are dropped (first wins).
    pub fn new(ghosts: Vec<Ghost>) -> Self {
        let mut set = GhostSet::default();
        let mut seen = HashSet::new();
        for g in ghosts {
            let target = g.target.trim();
            if target.is_empty() || !seen.insert(target.to_string()) {
                continue;
            }
            let id = set.index.upsert(extract(&format!("\u{0}ghost/{target}.md"), "", 0.0));
            debug_assert_eq!(id as usize, set.ghosts.len());
            set.ghosts.push(Ghost { target: target.to_string(), refs: g.refs });
        }
        set
    }
}

/// Per-search lookups owned by the engine; all keyed by note path.
#[derive(Default)]
pub struct Extras<'a> {
    pub ghosts: Option<&'a GhostSet>,
    pub access: Option<&'a HashMap<String, VecDeque<f64>>>,
    /// normalized query -> path -> number of opens.
    pub combos: Option<&'a HashMap<String, HashMap<String, u32>>>,
}

impl Extras<'_> {
    pub fn frecency(&self, path: &str, now_secs: f64) -> f32 {
        let Some(ts) = self.access.filter(|a| !a.is_empty()).and_then(|a| a.get(path)) else { return 0.0 };
        let ts: Vec<f64> = ts.iter().copied().collect();
        frecency_score(&ts, now_secs)
    }

    pub fn combo_count(&self, query: &str, path: &str) -> u32 {
        self.combos.filter(|c| !c.is_empty()).and_then(|c| c.get(query)?.get(path)).copied().unwrap_or(0)
    }
}

pub struct Ranked {
    pub hits: Vec<Hit>,
    /// Docs that passed the filters and got a tier (before the partial-tier cut).
    pub candidates: usize,
}

/// One AND-ed query unit: a plain term or a quoted phrase.
struct Unit {
    label: String,
    /// Normalized words, each with whether prefix matches count (for excerpt highlighting).
    words: Vec<(String, bool)>,
    /// Sorted by (doc, field), at most one per (doc, field).
    hits: Vec<TermHit>,
    /// Never required (candidates and tiers come from the other units); only scored.
    optional: bool,
    /// Single-word abbreviation alternatives of a term unit (their Body hits come late, like the
    /// term's own); phrase alternatives are complete in `hits` from the start.
    alt_terms: Vec<Term>,
}

/// One member of an abbreviation group as query words (one word → term, several → phrase).
type Member = Vec<String>;

/// `Config::synonyms` parsed: groups of at least two members. Members are split on `,`, `:` and
/// `=`, so `oss: open source, open-source software` and `oss, open source` read the same.
fn synonym_groups(cfg: &Config) -> Vec<Vec<Member>> {
    cfg.synonyms
        .iter()
        .map(|line| {
            let mut members: Vec<Member> = line
                .split([',', ':', '='])
                .map(|m| m.split(is_whitespace_like).filter_map(|w| tokenize_query_term(w).into_iter().next()).collect::<Member>())
                .filter(|m| !m.is_empty())
                .collect();
            members.dedup();
            members
        })
        .filter(|g| g.len() >= 2)
        .collect()
}

/// The other members of every group that lists the term's full compound as a single word.
fn alternatives<'g>(groups: &'g [Vec<Member>], term: &Term) -> Vec<&'g Member> {
    let Some(full) = term.parts.first() else { return Vec::new() };
    let mut out: Vec<&Member> = Vec::new();
    for g in groups.iter().filter(|g| g.iter().any(|m| m.len() == 1 && m[0] == *full)) {
        for m in g {
            if !(m.len() == 1 && m[0] == *full) && !out.contains(&m) {
                out.push(m);
            }
        }
    }
    out
}

/// Restores the unit invariant after merging hit lists: sorted by (doc, field), one hit per
/// (doc, field), keeping the best quality (the first on ties).
fn dedupe_hits(hits: &mut Vec<TermHit>) {
    hits.sort_by(|a, b| (a.doc, a.field as u8).cmp(&(b.doc, b.field as u8)).then(b.quality.total_cmp(&a.quality)));
    hits.dedup_by(|b, a| a.doc == b.doc && a.field == b.field);
}

/// Per unit, the range of its `hits` belonging to one doc (None: the unit misses the doc).
type Slots = SmallVec<[Option<(u32, u32)>; 4]>;

struct Scored<'a> {
    /// The doc's id in its index (the ghost set's private index for ghosts).
    id: DocId,
    doc: &'a Doc,
    tier: u8,
    score: f32,
    slots: Slots,
    /// fields (after the title factor), title factor, proximity, structural, penalty factor,
    /// frecency, combo (the last two as added to the score).
    parts: [f32; 7],
    ghost: Option<&'a Ghost>,
}

fn class(f: Field) -> u8 {
    match f {
        Field::Title | Field::Alias => 1,
        Field::H1 | Field::H2 | Field::H3 => 2,
        _ => 3,
    }
}

fn is_heading(f: Field) -> bool {
    class(f) == 2
}

fn kind_rank(k: MatchKind) -> u8 {
    match k {
        MatchKind::Exact => 0,
        MatchKind::Prefix => 1,
        MatchKind::Fuzzy => 2,
    }
}

fn synth_term(word: &str) -> Option<Term> {
    let parts = tokenize_query_term(word);
    (!parts.is_empty()).then(|| Term {
        raw: word.to_string(),
        norm: normalize(word),
        parts,
        has_upper: false,
    })
}

fn prefix_ok(word: &str, multi: bool) -> bool {
    multi || word.chars().count() >= 4
}

fn merge_ranges(mut r: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    r.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(r.len());
    for (s, e) in r {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Phrase hits: one per (doc, field) where the words sit at consecutive token positions.
/// Positions of the result are the phrase start positions.
fn phrase_hits(index: &Index, words: &[String], multi: bool) -> Vec<TermHit> {
    let Some(terms) = words.iter().map(|w| synth_term(w)).collect::<Option<Vec<_>>>() else {
        return Vec::new();
    };
    let lists: Vec<Vec<TermHit>> = terms.iter().map(|t| match_term(index, t, multi, NO_FUZZY)).collect();
    let mut out = Vec::new();
    'hits: for h0 in &lists[0] {
        // Tag "positions" are tag indices, not token positions.
        if h0.field == Field::Tag {
            continue;
        }
        let mut rest: SmallVec<[&TermHit; 4]> = SmallVec::new();
        for l in &lists[1..] {
            match l.binary_search_by_key(&(h0.doc, h0.field as u8), |h| (h.doc, h.field as u8)) {
                Ok(i) => rest.push(&l[i]),
                Err(_) => continue 'hits,
            }
        }
        let starts: SmallVec<[u32; 4]> = h0
            .positions
            .iter()
            .copied()
            .filter(|&p| {
                rest.iter()
                    .enumerate()
                    .all(|(i, h)| h.positions.binary_search(&(p + i as u32 + 1)).is_ok())
            })
            .collect();
        if starts.is_empty() {
            continue;
        }
        let all = std::iter::once(h0).chain(rest.iter().copied());
        let quality = all.clone().map(|h| h.quality).sum::<f32>() / words.len() as f32;
        let kind = all.clone().map(|h| h.kind).max_by_key(|k| kind_rank(*k)).unwrap();
        let ranges = merge_ranges(all.flat_map(|h| h.char_ranges.iter().copied()).collect());
        out.push(TermHit {
            doc: h0.doc,
            field: h0.field,
            kind,
            quality,
            positions: starts,
            heading_idx: h0.heading_idx,
            char_ranges: ranges.into_iter().collect(),
        });
    }
    out
}

fn build_units(index: &Index, query: &Query, cfg: &Config, files_only: bool) -> Vec<Unit> {
    let optional: Vec<bool> = {
        let list: HashSet<String> = cfg.optional_terms.iter().map(|w| normalize(w.trim())).collect();
        let marked: Vec<bool> = query.terms.iter().map(|t| list.contains(&t.norm)).collect();
        // A query of only optional terms keeps them all required (behaves as without the list).
        let all = query.phrases.is_empty() && marked.iter().all(|&m| m);
        marked.into_iter().map(|m| m && !all).collect()
    };
    // Short-term rule: only required words count, since optional ones do not narrow the
    // candidates (`ai the` keeps `ai` exact-only).
    let required_terms = optional.iter().filter(|&&o| !o).count();
    let n_words = required_terms + query.phrases.iter().map(Vec::len).sum::<usize>();
    let multi = n_words > 1;
    // A field weighted 0 in the settings is not searched at all (not merely scored 0).
    let keep = |h: &TermHit| (!files_only || class(h.field) == 1) && cfg.weights.get(h.field) > 0.0;
    // Body hits are filtered out by `keep` here, or (several units) computed last, only for
    // docs that can still get a tier: see `tierable`.
    let body_kept = !files_only && cfg.weights.body > 0.0;
    let body_later = body_kept && query.terms.len() + query.phrases.len() > 1;
    let scope = if body_kept && !body_later { Scope::All } else { Scope::NoBody };
    let groups = synonym_groups(cfg);
    let mut units = Vec::new();
    for (t, &optional) in query.terms.iter().zip(&optional) {
        let mut words: Vec<(String, bool)> = t.parts.iter().map(|p| (p.clone(), prefix_ok(p, multi))).collect();
        let mut hits: Vec<TermHit> = match_term_scoped(index, t, multi, cfg.fuzzy, scope).into_iter().filter(keep).collect();
        let mut alt_terms = Vec::new();
        // Abbreviations: the other members of the term's groups match as if typed too. Single
        // words follow the term rules without fuzzy (the owner spelled them), several words the
        // phrase rules (adjacent; phrase hits include Body for every doc, as phrase units do).
        let alts = alternatives(&groups, t);
        for m in &alts {
            if let [word] = m.as_slice() {
                if let Some(at) = synth_term(word) {
                    hits.extend(match_term_scoped(index, &at, multi, NO_FUZZY, scope).into_iter().filter(keep));
                    words.extend(at.parts.iter().map(|p| (p.clone(), prefix_ok(p, multi))));
                    alt_terms.push(at);
                }
            } else {
                hits.extend(phrase_hits(index, m, multi).into_iter().filter(keep));
                words.extend(m.iter().map(|w| (w.clone(), prefix_ok(w, multi))));
            }
        }
        if !alts.is_empty() {
            dedupe_hits(&mut hits);
        }
        units.push(Unit { label: t.norm.clone(), words, hits, optional, alt_terms });
    }
    for p in &query.phrases {
        units.push(Unit {
            label: p.join(" "),
            words: p.iter().map(|w| (w.clone(), prefix_ok(w, multi))).collect(),
            hits: phrase_hits(index, p, multi).into_iter().filter(keep).collect(),
            optional: false,
            alt_terms: Vec::new(),
        });
    }
    if body_later {
        let allowed = tierable(index, query, &units, multi);
        for (u, t) in units.iter_mut().zip(&query.terms) {
            let mut body = match_term_scoped(index, t, multi, NO_FUZZY, Scope::BodyOnly(&allowed));
            for at in &u.alt_terms {
                body.extend(match_term_scoped(index, at, multi, NO_FUZZY, Scope::BodyOnly(&allowed)));
            }
            if !body.is_empty() {
                u.hits.extend(body);
                dedupe_hits(&mut u.hits);
            }
        }
    }
    units
}

/// Docs that can get a tier once Body hits are added (term units still without them, phrase
/// units complete): every required unit hits the doc somewhere (counting any Body token of a
/// term), or some required unit hits its Title/Alias (partial tier). Body hits of other docs
/// cannot change any result, so they are never built.
fn tierable(index: &Index, query: &Query, units: &[Unit], multi: bool) -> DocSet {
    let mut titled = DocSet::default();
    let mut all: Option<DocSet> = None;
    for (i, u) in units.iter().enumerate().filter(|(_, u)| !u.optional) {
        let mut docs = query.terms.get(i).map_or_else(DocSet::default, |t| body_docs(index, t, multi));
        for at in &u.alt_terms {
            docs.union_with(&body_docs(index, at, multi));
        }
        for h in &u.hits {
            docs.insert(h.doc);
            if class(h.field) == 1 {
                titled.insert(h.doc);
            }
        }
        all = Some(match all {
            None => docs,
            Some(mut a) => {
                a.intersect_with(&docs);
                a
            }
        });
    }
    let mut allowed = all.unwrap_or_default();
    allowed.union_with(&titled);
    allowed
}

/// Docs hit by a required unit, with every unit's hit range per doc (optional units only fill
/// in docs a required unit brought in).
fn candidates(index: &Index, units: &[Unit]) -> HashMap<DocId, Slots> {
    let n = units.len();
    let mut cands: HashMap<DocId, Slots> = HashMap::new();
    if n == 0 {
        // Filter-only query: every doc is a candidate.
        cands.extend(index.ids().map(|id| (id, Slots::new())));
    }
    let (required, optional): (Vec<_>, Vec<_>) = units.iter().enumerate().partition(|(_, u)| !u.optional);
    for (ui, u) in required.into_iter().chain(optional) {
        let mut s = 0;
        while s < u.hits.len() {
            let doc = u.hits[s].doc;
            let e = s + u.hits[s..].iter().take_while(|h| h.doc == doc).count();
            let slot = Some((s as u32, e as u32));
            if !u.optional {
                cands.entry(doc).or_insert_with(|| smallvec![None; n])[ui] = slot;
            } else if let Some(slots) = cands.get_mut(&doc) {
                slots[ui] = slot;
            }
            s = e;
        }
    }
    cands
}

/// `#tag`, `path:` and `-exclude` narrow the candidates before any scoring.
fn apply_filters(index: &Index, query: &Query, cands: &mut HashMap<DocId, Slots>) {
    for tag in &query.tags {
        let with: HashSet<DocId> = index.docs_with_tag(tag).into_iter().collect();
        cands.retain(|d, _| with.contains(d));
    }
    if !query.paths.is_empty() {
        cands.retain(|d, _| {
            index.path_norm(*d).is_some_and(|p| query.paths.iter().all(|f| p.contains(f.as_str())))
        });
    }
    for ex in &query.excludes {
        let Some(t) = synth_term(ex) else { continue };
        // Excludes obey the short-term rule: `-ai` removes the token `ai`, never `aim`.
        for h in match_term(index, &t, false, NO_FUZZY) {
            cands.remove(&h.doc);
        }
    }
}

fn unit_hits(u: &Unit, slot: Option<(u32, u32)>) -> &[TermHit] {
    slot.map_or(&[], |(s, e)| &u.hits[s as usize..e as usize])
}

/// The required units with their slots.
fn required<'u>(units: &'u [Unit], slots: &'u Slots) -> impl Iterator<Item = (&'u Unit, Option<(u32, u32)>)> + Clone {
    units.iter().zip(slots.iter().copied()).filter(|(u, _)| !u.optional)
}

/// Tier from the required units only: 1–3 when each hits the doc, 4 when one hits its Title/Alias.
fn tier_of(units: &[Unit], slots: &Slots) -> Option<u8> {
    let req = required(units, slots);
    if req.clone().all(|(_, s)| s.is_some()) {
        // Hits are sorted by field, so the first is the unit's lowest class.
        let worst = req.map(|(u, s)| class(unit_hits(u, s)[0].field)).max();
        return Some(worst.unwrap_or(1));
    }
    let partial = req.clone().any(|(u, s)| unit_hits(u, s).iter().any(|h| class(h.field) == 1));
    partial.then_some(4)
}

/// The hit contributing most to the score (`weight × quality`), first one on ties.
fn best_contribution<'h>(w: &Weights, hits: &'h [TermHit]) -> Option<&'h TermHit> {
    hits.iter().fold(None, |best: Option<&TermHit>, h| match best {
        Some(b) if w.get(b.field) * b.quality >= w.get(h.field) * h.quality => Some(b),
        _ => Some(h),
    })
}

/// True when one position from each list lies within a window of `w` tokens.
fn within_window(lists: &[&[u32]], w: u32) -> bool {
    let mut idx = vec![0usize; lists.len()];
    loop {
        let (mut lo, mut lo_i, mut hi) = (u32::MAX, 0, 0);
        for (i, l) in lists.iter().enumerate() {
            let v = l[idx[i]];
            if v < lo {
                lo = v;
                lo_i = i;
            }
            hi = hi.max(v);
        }
        if hi - lo <= w {
            return true;
        }
        idx[lo_i] += 1;
        if idx[lo_i] == lists[lo_i].len() {
            return false;
        }
    }
}

/// Over the units that hit the doc (in tiers 1–3 every required one; optional ones that miss
/// are left out).
fn proximity(units: &[Unit], slots: &Slots) -> f32 {
    let hit: Vec<(&Unit, Option<(u32, u32)>)> = units.iter().zip(slots.iter().copied()).filter(|(_, s)| s.is_some()).collect();
    if hit.len() < 2 {
        return 0.0;
    }
    let body: Option<Vec<&[u32]>> = hit
        .iter()
        .map(|&(u, s)| {
            unit_hits(u, s)
                .iter()
                .find(|h| h.field == Field::Body && !h.positions.is_empty())
                .map(|h| h.positions.as_slice())
        })
        .collect();
    if body.is_some_and(|lists| within_window(&lists, PROXIMITY_WINDOW)) {
        return NEAR_BONUS;
    }
    let sections: Vec<SmallVec<[u16; 3]>> = hit
        .iter()
        .map(|&(u, s)| {
            unit_hits(u, s).iter().filter(|h| is_heading(h.field)).filter_map(|h| h.heading_idx).collect()
        })
        .collect();
    let shared = sections[0].iter().any(|h| sections[1..].iter().all(|o| o.contains(h)));
    if shared {
        SECTION_BONUS
    } else {
        0.0
    }
}

/// Title words (`spans` into the normalized title) that no unit hit in Title, by range overlap
/// (so fuzzy hits count too).
fn unmatched_in_title(spans: &[(u32, u32)], units: &[Unit], slots: &Slots) -> usize {
    let covered = |s: u32, e: u32| {
        units.iter().zip(slots).any(|(u, &sl)| {
            unit_hits(u, sl)
                .iter()
                .any(|h| h.field == Field::Title && h.char_ranges.iter().any(|&(a, b)| a < e && s < b))
        })
    };
    spans.iter().filter(|&&(s, e)| !covered(s, e)).count()
}

/// `(covered, uncovered)` words of one alias: a word is covered when a unit that hit Alias has a
/// word equal to it (or a prefix of it, where prefixes count). Hit ranges cannot be used here:
/// they refer to one alias of the doc, not necessarily this one.
fn alias_coverage(norm: &str, spans: &[(u32, u32)], units: &[Unit], slots: &Slots) -> (usize, usize) {
    let covered = spans
        .iter()
        .filter(|&&(s, e)| {
            let word = &norm[s as usize..e as usize];
            units.iter().zip(slots).any(|(u, &sl)| {
                unit_hits(u, sl).iter().any(|h| h.field == Field::Alias) && word_matches(word, &u.words)
            })
        })
        .count();
    (covered, spans.len() - covered)
}

/// Non-stopword title words no unit matched; a matched alias stands in for the title when it is
/// better covered.
fn unmatched_title_words(index: &Index, id: DocId, units: &[Unit], slots: &Slots) -> usize {
    let title = index.title_keys(id).map_or(0, |k| unmatched_in_title(&k.spans, units, slots));
    index
        .alias_words(id)
        .iter()
        .map(|(norm, spans)| alias_coverage(norm, spans, units, slots))
        .filter(|&(covered, _)| covered > 0)
        .map(|(_, uncovered)| uncovered)
        .fold(title, usize::min)
}

/// The normalized title's words are exactly the query's term words, in any order (no phrases).
fn whole_title_equal(title_norm: &str, query: &Query) -> bool {
    if !query.phrases.is_empty() || query.terms.is_empty() {
        return false;
    }
    if title_norm == query.normalized {
        return true;
    }
    let mut a: SmallVec<[&str; 8]> = title_norm.split(is_whitespace_like).filter(|w| !w.is_empty()).collect();
    if a.len() != query.terms.len() {
        return false;
    }
    let mut b: SmallVec<[&str; 8]> = query.terms.iter().map(|t| t.norm.as_str()).collect();
    a.sort_unstable();
    b.sort_unstable();
    a == b
}

/// `path_norm` is the doc's cached normalized path; `folders` are pre-normalized. No allocation.
fn downranked(path_norm: &str, folders: &[String]) -> bool {
    if folders.is_empty() {
        return false;
    }
    let dirs = path_norm.rsplit_once('/').map_or("", |(d, _)| d);
    folders.iter().any(|f| {
        dirs.strip_prefix(f.as_str()).is_some_and(|r| r.is_empty() || r.starts_with('/'))
            || dirs.split('/').any(|seg| seg.trim() == f)
    })
}

#[allow(clippy::too_many_arguments)]
fn score<'a>(
    index: &Index,
    id: DocId,
    doc: &'a Doc,
    tier: u8,
    slots: Slots,
    units: &[Unit],
    cfg: &Config,
    folders: &[String],
    query: &Query,
    extras: &Extras,
    now_secs: f64,
) -> Scored<'a> {
    let w = &cfg.weights;
    let raw_fields: f32 = units
        .iter()
        .zip(&slots)
        .filter_map(|(u, &s)| best_contribution(w, unit_hits(u, s)))
        .map(|h| w.get(h.field) * h.quality)
        .sum();
    // Fully covered titles keep their score; each unmatched title word costs STEP (spec §6.4).
    let title_factor = if tier <= 3 {
        let unmatched = unmatched_title_words(index, id, units, &slots);
        (1.0 - TITLE_UNMATCHED_STEP * unmatched as f32).max(TITLE_UNMATCHED_FLOOR)
    } else {
        1.0
    };
    let fields = raw_fields * title_factor;
    let prox = if tier <= 3 { proximity(units, &slots) } else { 0.0 };
    let whole = tier == 1 && index.title_keys(id).is_some_and(|k| whole_title_equal(&k.norm, query));
    let structural = BACKLINK_BONUS * (1.0 + index.backlinks(id) as f32).ln() + if whole { WHOLE_TITLE_BONUS } else { 0.0 };
    let exact_title = units
        .iter()
        .zip(&slots)
        .any(|(u, &s)| unit_hits(u, s).iter().any(|h| h.field == Field::Title && h.kind == MatchKind::Exact));
    let mut penalty = 1.0;
    if downranked(index.path_norm(id).unwrap_or(""), folders) {
        penalty *= cfg.downrank_factor;
    }
    if doc.word_count < SHORT_NOTE_WORDS && !exact_title {
        penalty *= SHORT_NOTE_FACTOR;
    }
    let frecency = extras.frecency(&doc.path, now_secs) * FRECENCY_WEIGHT;
    let combo = if extras.combo_count(&query.normalized, &doc.path) >= COMBO_MIN_COUNT { COMBO_BONUS } else { 0.0 };
    Scored {
        id,
        doc,
        tier,
        score: (fields + prox + structural + frecency + combo) * penalty,
        slots,
        parts: [fields, title_factor, prox, structural, penalty, frecency, combo],
        ghost: None,
    }
}

fn cmp_scored(a: &Scored, b: &Scored) -> Ordering {
    a.tier
        .cmp(&b.tier)
        .then(b.score.total_cmp(&a.score))
        .then(a.ghost.is_some().cmp(&b.ghost.is_some()))
        .then(b.doc.mtime.total_cmp(&a.doc.mtime))
        .then_with(|| a.doc.path.cmp(&b.doc.path))
}

pub fn rank(index: &Index, query: &Query, cfg: &Config, opts: &SearchOptions, extras: &Extras) -> Ranked {
    if query.is_empty() {
        return recent(index, opts, extras);
    }
    // `#115` is a tag filter only when some note carries that tag; otherwise it is the word
    // `115` (titles such as "Newsletter #115"; docs/recall.md fix 1).
    let as_words = query.with_tags_as_words(|t| !index.docs_with_tag(t).is_empty());
    let query = as_words.as_ref().unwrap_or(query);
    let (ranked, full) = rank_query(index, query, cfg, opts, extras);
    if full {
        return ranked;
    }
    // `-NET` excludes only when something is left; a query whose exclusions leave no tier 1–3
    // hit is retried with them as words ("KW13 -Marktleistung"; docs/recall.md fix 1).
    if let Some(q) = query.with_excludes_as_words() {
        let (retry, full) = rank_query(index, &q, cfg, opts, extras);
        if full {
            return retry;
        }
    }
    ranked
}

/// `rank` for one parsed query; the flag says whether any candidate reached tiers 1–3.
fn rank_query(index: &Index, query: &Query, cfg: &Config, opts: &SearchOptions, extras: &Extras) -> (Ranked, bool) {
    let units = build_units(index, query, cfg, opts.mode == Mode::FilesOnly);
    if units.is_empty() && !query.has_filters() {
        return (Ranked { hits: Vec::new(), candidates: 0 }, false);
    }
    let mut cands = candidates(index, &units);
    apply_filters(index, query, &mut cands);

    let folders: Vec<String> = cfg
        .downranked_folders
        .iter()
        .map(|f| normalize(&strip_symbols(f.trim_matches('/'))).trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    let mut scored: Vec<Scored> = cands
        .into_iter()
        .filter_map(|(id, slots)| {
            let doc = index.get(id)?;
            let tier = tier_of(&units, &slots)?;
            Some(score(index, id, doc, tier, slots, &units, cfg, &folders, query, extras, opts.now_secs))
        })
        .collect();
    // Ghosts are tier-1 candidates; `ghost_units` index into the ghost set's private index.
    let mut ghost_units: Vec<Unit> = Vec::new();
    if let Some(gs) = extras.ghosts.filter(|_| ghosts_apply(query, &units)) {
        ghost_units = build_units(&gs.index, query, cfg, true);
        let ctx = (cfg, &folders[..], query, extras, opts.now_secs);
        scored.extend(ghost_scored(index, gs, &ghost_units, ctx));
    }
    let total = scored.len();
    let any_full = scored.iter().any(|s| s.tier <= 3);

    let show_partial = match cfg.partial {
        PartialMode::Always => true,
        PartialMode::Never => false,
        PartialMode::WhenFew => scored.iter().filter(|s| s.tier <= 3).count() < cfg.few_threshold,
    };
    if !show_partial {
        scored.retain(|s| s.tier <= 3);
    }

    if opts.limit == 0 {
        return (Ranked { hits: Vec::new(), candidates: total }, any_full);
    }
    if scored.len() > opts.limit {
        scored.select_nth_unstable_by(opts.limit - 1, cmp_scored);
        scored.truncate(opts.limit);
    }
    scored.sort_by(cmp_scored);
    let hits = scored
        .iter()
        .map(|s| match (s.ghost, extras.ghosts) {
            (Some(_), Some(gs)) => page_hit(&gs.index, s, &ghost_units, cfg, opts),
            _ => page_hit(index, s, &units, cfg, opts),
        })
        .collect();
    (Ranked { hits, candidates: total }, any_full)
}

/// Ghosts take part in plain term/phrase queries only: tag and path filters never match them.
fn ghosts_apply(query: &Query, units: &[Unit]) -> bool {
    !units.is_empty() && query.tags.is_empty() && query.paths.is_empty()
}

/// Normalized keys under which an existing note would resolve a link to `target`.
fn ghost_keys(target: &str) -> (String, String) {
    (normalize(&strip_symbols(target)), normalize(&strip_symbols(&format!("{target}.md"))))
}

/// Ghost candidates that match every unit in Title (tier 1) and have no real note of that name.
fn ghost_scored<'a>(
    index: &Index,
    gs: &'a GhostSet,
    units: &[Unit],
    (cfg, folders, query, extras, now_secs): (&Config, &[String], &Query, &Extras, f64),
) -> Vec<Scored<'a>> {
    let mut cands = candidates(&gs.index, units);
    apply_filters(&gs.index, query, &mut cands);
    let mut matched: Vec<(DocId, Slots)> =
        cands.into_iter().filter(|(_, slots)| required(units, slots).all(|(_, s)| s.is_some())).collect();
    if matched.is_empty() {
        return Vec::new();
    }
    // Suppress ghosts whose target now exists as a note (by title or by path without `.md`).
    let keys: Vec<(String, String)> =
        matched.iter().map(|(id, _)| ghost_keys(&gs.ghosts[*id as usize].target)).collect();
    let wanted: HashSet<&str> = keys.iter().flat_map(|(t, p)| [t.as_str(), p.as_str()]).collect();
    let mut found: HashSet<&str> = HashSet::new();
    found.extend(index.field_strings(Field::Title).iter().map(|(_, _, t)| t.as_str()).filter(|t| wanted.contains(t)));
    found.extend(index.ids().filter_map(|id| index.path_norm(id)).filter(|p| wanted.contains(p)));
    let mut i = 0;
    matched.retain(|_| {
        let (t, p) = &keys[i];
        i += 1;
        !(found.contains(t.as_str()) || found.contains(p.as_str()))
    });
    matched
        .into_iter()
        .filter_map(|(id, slots)| {
            let doc = gs.index.get(id)?;
            let tier = tier_of(units, &slots).filter(|&t| t == 1)?;
            let mut sc = score(&gs.index, id, doc, tier, slots, units, cfg, folders, query, extras, now_secs);
            sc.ghost = gs.ghosts.get(id as usize);
            Some(sc)
        })
        .collect()
}

/// Empty query: the most recently used notes (frecency, then mtime), then the newest others.
fn recent(index: &Index, opts: &SearchOptions, extras: &Extras) -> Ranked {
    let mut docs: Vec<(&Doc, f32)> = index
        .ids()
        .filter_map(|id| index.get(id))
        .map(|d| (d, extras.frecency(&d.path, opts.now_secs)))
        .collect();
    docs.sort_by(|a, b| {
        b.1.total_cmp(&a.1).then(b.0.mtime.total_cmp(&a.0.mtime)).then_with(|| a.0.path.cmp(&b.0.path))
    });
    let total = docs.len();
    let hits = docs
        .into_iter()
        .take(RECENT_LIMIT.min(opts.limit))
        .map(|(d, f)| Hit {
            path: d.path.clone(),
            title: d.title.clone(),
            tier: 0,
            score: f,
            badge: Field::Title,
            title_norm: normalize(&strip_symbols(&d.title)),
            title_ranges: Vec::new(),
            heading: None,
            excerpt: None,
            line: None,
            is_ghost: false,
            ghost_refs: 0,
            similar_count: 0,
            debug: None,
        })
        .collect();
    Ranked { hits, candidates: total }
}

/// Badge: the field that set the tier (the weakest of the units' best fields); for partial hits,
/// the best Title/Alias field.
/// Optional units do not set the badge.
fn badge(s: &Scored, units: &[Unit]) -> Field {
    let firsts = required(units, &s.slots).filter_map(|(u, sl)| unit_hits(u, sl).first().map(|h| h.field));
    if s.tier == 4 {
        let title_alias = required(units, &s.slots)
            .flat_map(|(u, sl)| unit_hits(u, sl))
            .map(|h| h.field)
            .filter(|f| class(*f) == 1);
        return title_alias.min_by_key(|f| *f as u8).unwrap_or(Field::Title);
    }
    firsts.max_by_key(|f| *f as u8).unwrap_or(Field::Title)
}

fn title_display(s: &Scored, units: &[Unit], badge: Field) -> (String, Vec<(u32, u32)>) {
    let all = || units.iter().zip(&s.slots).flat_map(|(u, &sl)| unit_hits(u, sl));
    let title: Vec<(u32, u32)> =
        all().filter(|h| h.field == Field::Title).flat_map(|h| h.char_ranges.iter().copied()).collect();
    let alias_hits: Vec<&TermHit> = all().filter(|h| h.field == Field::Alias).collect();
    if title.is_empty() && badge == Field::Alias && !alias_hits.is_empty() {
        let fits = |a: &str, h: &TermHit| h.char_ranges.iter().all(|&(x, y)| a.get(x as usize..y as usize).is_some());
        let aliases: Vec<String> = s.doc.aliases.iter().map(|a| normalize(a)).collect();
        if let Some(a) = aliases.iter().find(|a| fits(a, alias_hits[0])) {
            let ranges = alias_hits.iter().filter(|h| fits(a, h)).flat_map(|h| h.char_ranges.iter().copied());
            return (a.clone(), merge_ranges(ranges.collect()));
        }
    }
    (normalize(&strip_symbols(&s.doc.title)), merge_ranges(title))
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn word_matches(tok: &str, words: &[(String, bool)]) -> bool {
    words.iter().any(|(w, pre)| tok == w || (*pre && tok.starts_with(w.as_str())))
}

/// Excerpt and body line around the first body match of the first unit with body positions.
/// Works on a window of whole body lines around the match (`Index::body_window`); its margin
/// covers the radius plus a char-boundary step, so the excerpt equals the one cut from the
/// whole body.
fn body_context(index: &Index, s: &Scored, units: &[Unit]) -> Option<(Excerpt, u32)> {
    let (ui, pos) = units.iter().zip(&s.slots).enumerate().find_map(|(i, (u, &sl))| {
        let h = unit_hits(u, sl).iter().find(|h| h.field == Field::Body)?;
        Some((i, *h.positions.first()?))
    })?;
    let w = index.body_window(s.id, pos, EXCERPT_RADIUS + 4)?;
    let (body, toks) = (&w.text, &w.tokens);
    let at = toks.iter().filter(|t| t.pos == pos);
    let tok = at.clone().find(|t| word_matches(&t.text, &units[ui].words)).or_else(|| at.clone().next())?;
    let (bs, be) = (tok.byte_start as usize, tok.byte_end as usize);
    let body_line = w.first_line + body[..bs].bytes().filter(|&b| b == b'\n').count();
    let line = s.doc.body_line_src.get(body_line).copied().unwrap_or(body_line as u32);
    let start = floor_boundary(body, bs.saturating_sub(EXCERPT_RADIUS));
    let end = ceil_boundary(body, be + EXCERPT_RADIUS);
    let ranges = toks
        .iter()
        .filter(|t| t.byte_start as usize >= start && t.byte_end as usize <= end)
        .filter(|t| units.iter().any(|u| word_matches(&t.text, &u.words)))
        .map(|t| (t.byte_start - start as u32, t.byte_end - start as u32))
        .collect();
    Some((Excerpt { text: body[start..end].to_string(), ranges: merge_ranges(ranges) }, line))
}

fn page_hit(index: &Index, s: &Scored, units: &[Unit], cfg: &Config, opts: &SearchOptions) -> Hit {
    let badge = badge(s, units);
    let heading = if is_heading(badge) {
        units
            .iter()
            .zip(&s.slots)
            .filter_map(|(u, &sl)| unit_hits(u, sl).first())
            .find(|h| h.field == badge)
            .and_then(|h| s.doc.headings.get(h.heading_idx? as usize))
            .map(|h| HeadingHit { text: h.text.clone(), line: h.line, level: h.level })
    } else {
        None
    };
    let (title_norm, title_ranges) = title_display(s, units, badge);
    let context = body_context(index, s, units);
    let line = heading.as_ref().map(|h| h.line).or(context.as_ref().map(|c| c.1));
    let debug = opts.debug.then(|| {
        let per_term = units
            .iter()
            .zip(&s.slots)
            .filter_map(|(u, &sl)| {
                let h = best_contribution(&cfg.weights, unit_hits(u, sl))?;
                Some((u.label.clone(), h.field, h.kind, h.quality))
            })
            .collect();
        let [fields, title_factor, prox, structural, penalty, frecency, combo] = s.parts;
        let components = [
            ("fields", fields),
            ("titleFactor", title_factor),
            ("proximity", prox),
            ("structural", structural),
            ("penalty", penalty),
            ("frecency", frecency),
            ("combo", combo),
        ];
        DebugInfo { per_term, components: components.iter().map(|(k, v)| (k.to_string(), *v)).collect() }
    });
    Hit {
        path: s.ghost.map_or_else(|| s.doc.path.clone(), |g| g.target.clone()),
        title: s.ghost.map_or_else(|| s.doc.title.clone(), |g| g.target.clone()),
        tier: s.tier,
        score: s.score,
        badge,
        title_norm,
        title_ranges,
        heading,
        excerpt: context.map(|c| c.0),
        line,
        is_ghost: s.ghost.is_some(),
        ghost_refs: s.ghost.map_or(0, |g| g.refs),
        similar_count: 0,
        debug,
    }
}
