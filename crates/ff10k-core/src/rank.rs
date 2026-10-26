//! Ranking: candidate generation, hard tiers (spec §6.3) and the in-tier score (spec §6.4).
//!
//! Every query term and every quoted phrase is one AND-ed *unit*. A doc's tier comes from each
//! unit's best field class (Title/Alias → 1, H1–H3 → 2, anything else → 3); docs missing a unit
//! but hitting another in Title/Alias are partial (tier 4). Within a tier the score is
//! `(Σ fields + proximity + structural + frecency + combo) × penalty`. Hit details (badge,
//! heading, excerpt, line) are computed only for the returned page.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use smallvec::{smallvec, SmallVec};

use crate::doc::{Doc, Field, Weights};
use crate::index::{DocId, Index};
use crate::matcher::{match_term, FuzzyLevel, MatchKind, TermHit};
use crate::normalize::{normalize, strip_symbols};
use crate::query::{Query, Term};
use crate::tokenize::{tokenize, tokenize_query_term, Token};

/// Proximity: bonus when all units share a heading section, larger when within this many body tokens.
const PROXIMITY_WINDOW: u32 = 8;
const NEAR_BONUS: f32 = 25.0;
const SECTION_BONUS: f32 = 15.0;
const SHORT_TITLE_BONUS: f32 = 10.0;
const BACKLINK_BONUS: f32 = 2.0;
const SHORT_NOTE_WORDS: u32 = 20;
const SHORT_NOTE_FACTOR: f32 = 0.8;
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

/// An unresolved link target (filled by a later task).
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Ghost {
    pub target: String,
    pub refs: u32,
}

/// Per-search lookups owned by the engine. Frecency and combo are hooks returning 0 for now.
#[derive(Default)]
pub struct Extras<'a> {
    pub ghosts: &'a [Ghost],
}

impl Extras<'_> {
    pub fn frecency(&self, _path: &str) -> f32 {
        0.0
    }

    pub fn combo(&self, _query: &str, _path: &str) -> f32 {
        0.0
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
}

/// Per unit, the range of its `hits` belonging to one doc (None: the unit misses the doc).
type Slots = SmallVec<[Option<(u32, u32)>; 4]>;

struct Scored<'a> {
    doc: &'a Doc,
    tier: u8,
    score: f32,
    slots: Slots,
    /// fields, proximity, structural, penalty factor, frecency, combo.
    parts: [f32; 6],
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
    let n_words = query.terms.len() + query.phrases.iter().map(Vec::len).sum::<usize>();
    let multi = n_words > 1;
    let keep = |h: &TermHit| !files_only || class(h.field) == 1;
    let mut units = Vec::new();
    for t in &query.terms {
        units.push(Unit {
            label: t.norm.clone(),
            words: t.parts.iter().map(|p| (p.clone(), prefix_ok(p, multi))).collect(),
            hits: match_term(index, t, multi, cfg.fuzzy).into_iter().filter(keep).collect(),
        });
    }
    for p in &query.phrases {
        units.push(Unit {
            label: p.join(" "),
            words: p.iter().map(|w| (w.clone(), prefix_ok(w, multi))).collect(),
            hits: phrase_hits(index, p, multi).into_iter().filter(keep).collect(),
        });
    }
    units
}

fn candidates(index: &Index, units: &[Unit]) -> HashMap<DocId, Slots> {
    let n = units.len();
    let mut cands: HashMap<DocId, Slots> = HashMap::new();
    if n == 0 {
        // Filter-only query: every doc is a candidate.
        cands.extend(index.ids().map(|id| (id, Slots::new())));
    }
    for (ui, u) in units.iter().enumerate() {
        let mut s = 0;
        while s < u.hits.len() {
            let doc = u.hits[s].doc;
            let e = s + u.hits[s..].iter().take_while(|h| h.doc == doc).count();
            cands.entry(doc).or_insert_with(|| smallvec![None; n])[ui] = Some((s as u32, e as u32));
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

fn tier_of(units: &[Unit], slots: &Slots) -> Option<u8> {
    if slots.iter().all(Option::is_some) {
        // Hits are sorted by field, so the first is the unit's lowest class.
        let worst = units.iter().zip(slots).map(|(u, &s)| class(unit_hits(u, s)[0].field)).max();
        return Some(worst.unwrap_or(1));
    }
    let partial = units.iter().zip(slots).any(|(u, &s)| unit_hits(u, s).iter().any(|h| class(h.field) == 1));
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

fn proximity(units: &[Unit], slots: &Slots) -> f32 {
    if units.len() < 2 {
        return 0.0;
    }
    let body: Option<Vec<&[u32]>> = units
        .iter()
        .zip(slots)
        .map(|(u, &s)| {
            unit_hits(u, s)
                .iter()
                .find(|h| h.field == Field::Body && !h.positions.is_empty())
                .map(|h| h.positions.as_slice())
        })
        .collect();
    if body.is_some_and(|lists| within_window(&lists, PROXIMITY_WINDOW)) {
        return NEAR_BONUS;
    }
    let sections: Vec<SmallVec<[u16; 3]>> = units
        .iter()
        .zip(slots)
        .map(|(u, &s)| {
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
    n_words: usize,
    cfg: &Config,
    folders: &[String],
    query: &Query,
    extras: &Extras,
) -> Scored<'a> {
    let w = &cfg.weights;
    let fields: f32 = units
        .iter()
        .zip(&slots)
        .filter_map(|(u, &s)| best_contribution(w, unit_hits(u, s)))
        .map(|h| w.get(h.field) * h.quality)
        .sum();
    let prox = if tier <= 3 { proximity(units, &slots) } else { 0.0 };
    let extra_title = (index.title_tokens(id) as usize).saturating_sub(n_words);
    let structural = SHORT_TITLE_BONUS / (1.0 + extra_title as f32)
        + BACKLINK_BONUS * (1.0 + index.backlinks(id) as f32).ln();
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
    let frecency = extras.frecency(&doc.path);
    let combo = extras.combo(&query.normalized, &doc.path);
    Scored {
        doc,
        tier,
        score: (fields + prox + structural + frecency + combo) * penalty,
        slots,
        parts: [fields, prox, structural, penalty, frecency, combo],
    }
}

fn cmp_scored(a: &Scored, b: &Scored) -> Ordering {
    a.tier
        .cmp(&b.tier)
        .then(b.score.total_cmp(&a.score))
        .then(b.doc.mtime.total_cmp(&a.doc.mtime))
        .then_with(|| a.doc.path.cmp(&b.doc.path))
}

pub fn rank(index: &Index, query: &Query, cfg: &Config, opts: &SearchOptions, extras: &Extras) -> Ranked {
    let units = build_units(index, query, cfg, opts.mode == Mode::FilesOnly);
    if units.is_empty() && !query.has_filters() {
        return Ranked { hits: Vec::new(), candidates: 0 };
    }
    let mut cands = candidates(index, &units);
    apply_filters(index, query, &mut cands);

    let n_words = query.terms.len() + query.phrases.iter().map(Vec::len).sum::<usize>();
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
            Some(score(index, id, doc, tier, slots, &units, n_words, cfg, &folders, query, extras))
        })
        .collect();
    let total = scored.len();

    let show_partial = match cfg.partial {
        PartialMode::Always => true,
        PartialMode::Never => false,
        PartialMode::WhenFew => scored.iter().filter(|s| s.tier <= 3).count() < cfg.few_threshold,
    };
    if !show_partial {
        scored.retain(|s| s.tier <= 3);
    }

    if opts.limit == 0 {
        return Ranked { hits: Vec::new(), candidates: total };
    }
    if scored.len() > opts.limit {
        scored.select_nth_unstable_by(opts.limit - 1, cmp_scored);
        scored.truncate(opts.limit);
    }
    scored.sort_by(cmp_scored);
    let hits = scored.iter().map(|s| page_hit(s, &units, cfg, opts)).collect();
    Ranked { hits, candidates: total }
}

/// Badge: the field that set the tier (the weakest of the units' best fields); for partial hits,
/// the best Title/Alias field.
fn badge(s: &Scored, units: &[Unit]) -> Field {
    let firsts = units.iter().zip(&s.slots).filter_map(|(u, &sl)| unit_hits(u, sl).first().map(|h| h.field));
    if s.tier == 4 {
        let title_alias = units
            .iter()
            .zip(&s.slots)
            .flat_map(|(u, &sl)| unit_hits(u, sl))
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

/// Tokens of `body` up to and including position `pos`, plus `radius` bytes past it. Tokenizes
/// a growing prefix cut at whitespace (a word boundary, so positions match the full tokenization).
fn tokens_around(body: &str, pos: u32, radius: usize) -> Vec<Token> {
    let mut cut = 512;
    loop {
        let end = if cut >= body.len() {
            body.len()
        } else {
            let at = floor_boundary(body, cut);
            body[at..].find(char::is_whitespace).map_or(body.len(), |i| at + i)
        };
        let toks = tokenize(&body[..end]);
        let done = end == body.len()
            || toks.iter().find(|t| t.pos == pos).is_some_and(|t| t.byte_end as usize + radius < end);
        if done {
            return toks;
        }
        cut *= 4;
    }
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
fn body_context(s: &Scored, units: &[Unit]) -> Option<(Excerpt, u32)> {
    let (ui, pos) = units.iter().zip(&s.slots).enumerate().find_map(|(i, (u, &sl))| {
        let h = unit_hits(u, sl).iter().find(|h| h.field == Field::Body)?;
        Some((i, *h.positions.first()?))
    })?;
    let body = &s.doc.body;
    let toks = tokens_around(body, pos, EXCERPT_RADIUS);
    let at = toks.iter().filter(|t| t.pos == pos);
    let tok = at.clone().find(|t| word_matches(&t.text, &units[ui].words)).or_else(|| at.clone().next())?;
    let (bs, be) = (tok.byte_start as usize, tok.byte_end as usize);
    let body_line = s.doc.body_line_starts.partition_point(|&x| x as usize <= bs).saturating_sub(1);
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

fn page_hit(s: &Scored, units: &[Unit], cfg: &Config, opts: &SearchOptions) -> Hit {
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
    let context = body_context(s, units);
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
        let [fields, prox, structural, penalty, frecency, combo] = s.parts;
        let components = [
            ("fields", fields),
            ("proximity", prox),
            ("structural", structural),
            ("penalty", penalty),
            ("frecency", frecency),
            ("combo", combo),
        ];
        DebugInfo { per_term, components: components.iter().map(|(k, v)| (k.to_string(), *v)).collect() }
    });
    Hit {
        path: s.doc.path.clone(),
        title: s.doc.title.clone(),
        tier: s.tier,
        score: s.score,
        badge,
        title_norm,
        title_ranges,
        heading,
        excerpt: context.map(|c| c.0),
        line,
        is_ghost: false,
        ghost_refs: 0,
        similar_count: 0,
        debug,
    }
}
