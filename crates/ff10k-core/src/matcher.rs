//! Per-term matcher: finds every (doc, field) a single query term hits.
//!
//! Exact and prefix matches come from the per-field inverted index; fuzzy matches (frizbee,
//! Smith-Waterman with a typo budget) run only over Title, Alias and heading strings, and only
//! for docs that got no exact/prefix hit in that field. A compound term (`data-engineering`)
//! that matches nothing in a doc's field falls back to its parts: the doc matches that field
//! only when every part matches there, with quality = mean part quality × 0.95.

use std::collections::{HashMap, HashSet};

use neo_frizbee::{Config, Matcher, SortStrategy};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::doc::Field;
use crate::index::{DocId, Index, Posting, PostingList};
use crate::query::Term;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum MatchKind {
    Exact,
    Prefix,
    Fuzzy,
}

/// One term's best match in one field of one doc.
///
/// `positions`: token positions (for H1/H2/H3 unpacked to the in-heading `pos`), union over all
/// matching tokens in the field; empty for fuzzy hits. `heading_idx`: for H1/H2/H3, the index
/// into `Doc::headings` of the best match. `char_ranges`: byte ranges into the normalized field
/// string (`Index::field_strings`) for Title/Alias/H1/H2/H3 hits; empty for other fields.
#[derive(Debug, Clone)]
pub struct TermHit {
    pub doc: DocId,
    pub field: Field,
    pub kind: MatchKind,
    pub quality: f32,
    pub positions: SmallVec<[u32; 4]>,
    pub heading_idx: Option<u16>,
    pub char_ranges: SmallVec<[(u32, u32); 4]>,
}

/// Fuzzy typo tolerance: normal allows 1 typo for 6–9 chars and 2 for 10+; `loose` adds 1 to each.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyLevel {
    pub enabled: bool,
    pub loose: bool,
}

const FIELDS: [Field; 9] = [
    Field::Title,
    Field::Alias,
    Field::H1,
    Field::H2,
    Field::H3,
    Field::Tag,
    Field::Link,
    Field::Path,
    Field::Body,
];

/// Fields that have normalized strings: fuzzy targets and the ones that carry `char_ranges`.
fn has_strings(f: Field) -> bool {
    matches!(f, Field::Title | Field::Alias | Field::H1 | Field::H2 | Field::H3)
}

fn is_heading(f: Field) -> bool {
    matches!(f, Field::H1 | Field::H2 | Field::H3)
}

fn kind_rank(k: MatchKind) -> u8 {
    match k {
        MatchKind::Exact => 0,
        MatchKind::Prefix => 1,
        MatchKind::Fuzzy => 2,
    }
}

/// Exact/prefix match accumulator for one (doc, field).
struct Acc<'a> {
    kind: MatchKind,
    quality: f32,
    heading_idx: Option<u16>,
    positions: SmallVec<[u32; 4]>,
    /// Matched tokens with the highlighted byte length (only tracked for string fields).
    tokens: SmallVec<[(&'a str, usize); 2]>,
}

impl<'a> Acc<'a> {
    fn new(kind: MatchKind) -> Self {
        Acc { kind, quality: -1.0, heading_idx: None, positions: SmallVec::new(), tokens: SmallVec::new() }
    }

    fn add(&mut self, field: Field, kind: MatchKind, quality: f32, p: Posting, hl: Option<(&'a str, usize)>) {
        let heading = is_heading(field);
        let better = quality > self.quality
            || (quality == self.quality && kind_rank(kind) < kind_rank(self.kind));
        if better {
            self.kind = kind;
            self.quality = quality;
            self.heading_idx = if heading { p.positions.first().map(|x| (x >> 16) as u16) } else { None };
        }
        if heading {
            self.positions.extend(p.positions.iter().map(|x| x & 0xFFFF));
        } else {
            self.positions.extend_from_slice(p.positions);
        }
        self.tokens.extend(hl);
    }
}

/// A set of doc ids: a bitset over the index's id range.
#[derive(Clone, Debug, Default)]
pub struct DocSet {
    bits: Vec<u64>,
}

impl DocSet {
    pub fn insert(&mut self, doc: DocId) {
        let (w, b) = (doc as usize / 64, doc % 64);
        if w >= self.bits.len() {
            self.bits.resize(w + 1, 0);
        }
        self.bits[w] |= 1 << b;
    }

    pub fn contains(&self, doc: DocId) -> bool {
        self.bits.get(doc as usize / 64).is_some_and(|w| w & (1 << (doc % 64)) != 0)
    }

    pub fn union_with(&mut self, other: &DocSet) {
        if other.bits.len() > self.bits.len() {
            self.bits.resize(other.bits.len(), 0);
        }
        for (a, b) in self.bits.iter_mut().zip(&other.bits) {
            *a |= b;
        }
    }

    pub fn intersect_with(&mut self, other: &DocSet) {
        self.bits.truncate(other.bits.len());
        for (a, b) in self.bits.iter_mut().zip(&other.bits) {
            *a &= b;
        }
    }
}

/// The fields (and, for Body, the docs) `match_term_scoped` matches.
#[derive(Clone, Copy, Debug)]
pub enum Scope<'s> {
    All,
    /// Every field except Body.
    NoBody,
    /// Only Body, and only for docs in the set (others are left out, as if they had none).
    BodyOnly(&'s DocSet),
}

/// Exact (and, when allowed, prefix) matches of `needle` in `field`, merged into `out`; with
/// `only`, postings of other docs are skipped.
fn match_token<'a>(
    index: &'a Index,
    field: Field,
    needle: &'a str,
    allow_prefix: bool,
    only: Option<&DocSet>,
    out: &mut HashMap<DocId, Acc<'a>>,
) {
    let track = has_strings(field);
    let needle_chars = needle.chars().count() as f32;
    let mut add = |tok: &'a str, postings: &PostingList| {
        let (kind, quality) = if tok.len() == needle.len() {
            (MatchKind::Exact, 1.0)
        } else {
            (MatchKind::Prefix, needle_chars / tok.chars().count() as f32)
        };
        let hl = track.then_some((tok, needle.len()));
        for p in postings.iter() {
            if only.is_some_and(|s| !s.contains(p.doc)) {
                continue;
            }
            out.entry(p.doc).or_insert_with(|| Acc::new(kind)).add(field, kind, quality, p, hl);
        }
    };
    if allow_prefix {
        for (tok, postings) in index.prefix(field, needle) {
            add(tok, postings);
        }
    } else {
        add(needle, index.exact(field, needle));
    }
}

/// Byte offset of `tok` in `s`: first occurrence at a word start, else first occurrence anywhere.
fn locate(s: &str, tok: &str) -> Option<usize> {
    let mut first = None;
    for (i, _) in s.match_indices(tok) {
        if s[..i].chars().next_back().map_or(true, |c| !c.is_alphanumeric()) {
            return Some(i);
        }
        first.get_or_insert(i);
    }
    first
}

/// The normalized string a hit's `char_ranges` refer to.
fn target_string<'a>(index: &'a Index, field: Field, doc: DocId, acc: &Acc) -> Option<&'a str> {
    let strings = index.field_strings(field);
    let start = strings.partition_point(|e| e.0 < doc);
    let mut same = strings[start..].iter().take_while(|e| e.0 == doc);
    match field {
        Field::Title => same.next(),
        // The alias holding the best-ranked token; fall back to the first alias.
        Field::Alias => {
            let all: Vec<_> = same.collect();
            let best = acc.tokens.first().map(|t| t.0);
            all.iter().find(|e| best.is_some_and(|t| locate(&e.2, t).is_some())).or(all.first()).copied()
        }
        _ => same.find(|e| Some(e.1) == acc.heading_idx),
    }
    .map(|e| e.2.as_str())
}

/// Sort and merge touching/overlapping ranges.
fn merge(mut r: SmallVec<[(u32, u32); 4]>) -> SmallVec<[(u32, u32); 4]> {
    r.sort_unstable();
    let mut out: SmallVec<[(u32, u32); 4]> = SmallVec::new();
    for (s, e) in r {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

fn finish(index: &Index, field: Field, doc: DocId, mut acc: Acc) -> TermHit {
    acc.positions.sort_unstable();
    acc.positions.dedup();
    let mut char_ranges = SmallVec::new();
    if has_strings(field) {
        if let Some(s) = target_string(index, field, doc, &acc) {
            for &(tok, len) in &acc.tokens {
                if let Some(i) = locate(s, tok) {
                    char_ranges.push((i as u32, (i + len) as u32));
                }
            }
        }
    }
    TermHit {
        doc,
        field,
        kind: acc.kind,
        quality: acc.quality,
        positions: acc.positions,
        heading_idx: acc.heading_idx,
        char_ranges: merge(char_ranges),
    }
}

/// Minimum `score / self_score` of a fuzzy hit (spec §16).
const MIN_FUZZY_RATIO: f32 = 0.65;

/// frizbee match indices are byte offsets of matched bytes; widen each to its whole char.
/// Returns the merged char ranges and the number of distinct matched chars.
fn byte_ranges(s: &str, indices: &[u32]) -> (SmallVec<[(u32, u32); 4]>, usize) {
    let mut r: SmallVec<[(u32, u32); 4]> = SmallVec::new();
    for &i in indices {
        let mut start = (i as usize).min(s.len());
        while !s.is_char_boundary(start) {
            start -= 1;
        }
        let end = start + s[start..].chars().next().map_or(0, char::len_utf8);
        r.push((start as u32, end as u32));
    }
    r.sort_unstable();
    r.dedup();
    let chars = r.len();
    (merge(r), chars)
}

/// Acceptance rules for one fuzzy alignment (see `match_term`).
struct FuzzyRules {
    self_score: f32,
    typos: usize,
    needle_chars: usize,
    max_span: usize,
}

impl FuzzyRules {
    /// `score / self_score` and the char ranges of an alignment that passes; `None` for a
    /// scattered subsequence or a substitution-heavy alignment: a weak score, too few needle
    /// chars matched, or a match spread too wide.
    fn judge(&self, s: &str, score: u16, indices: &[u32]) -> Option<(f32, SmallVec<[(u32, u32); 4]>)> {
        let ratio = score as f32 / self.self_score;
        let (char_ranges, matched_chars) = byte_ranges(s, indices);
        let span = match (char_ranges.first(), char_ranges.last()) {
            (Some(a), Some(b)) => s[a.0 as usize..b.1 as usize].chars().count(),
            _ => return None,
        };
        if ratio < MIN_FUZZY_RATIO || matched_chars + self.typos < self.needle_chars || span > self.max_span {
            return None;
        }
        Some((ratio, char_ranges))
    }

    /// Whether some window of a string whose best whole-string score is `score` can reach the
    /// ratio cut. A window's alignment is an alignment of the whole string; in the window it can
    /// gain at most frizbee's prefix bonus (12, first haystack char) and exact-match bonus (8).
    fn window_can_pass(&self, score: u16) -> bool {
        const WINDOW_BONUS_MAX: f32 = 20.0;
        score as f32 + WINDOW_BONUS_MAX >= MIN_FUZZY_RATIO * self.self_score
    }

    /// The best passing alignment over windows of `s`: one per token start, `max_span` chars
    /// long. Char ranges are offset back into `s`.
    fn best_window(&self, m: &mut Matcher, s: &str) -> Option<(f32, SmallVec<[(u32, u32); 4]>)> {
        let chars: Vec<(usize, char)> = s.char_indices().collect();
        let mut starts: Vec<usize> = Vec::new();
        let mut windows: Vec<&str> = Vec::new();
        for (i, &(b, c)) in chars.iter().enumerate() {
            let at_start = c.is_alphanumeric() && (i == 0 || !chars[i - 1].1.is_alphanumeric());
            if !at_start {
                continue;
            }
            let end = chars.get(i + self.max_span).map_or(s.len(), |x| x.0);
            if b == 0 && end == s.len() {
                continue; // the whole string, already judged
            }
            starts.push(b);
            windows.push(&s[b..end]);
        }
        // Scores first (cheap); indices only for the windows whose score can pass.
        let min_score = MIN_FUZZY_RATIO * self.self_score;
        let keep: Vec<usize> =
            m.match_list(&windows).iter().filter(|r| r.score as f32 >= min_score).map(|r| r.index as usize).collect();
        if keep.is_empty() {
            return None;
        }
        let kept: Vec<&str> = keep.iter().map(|&i| windows[i]).collect();
        let mut best: Option<(f32, SmallVec<[(u32, u32); 4]>)> = None;
        for r in m.match_list_indices(&kept) {
            if r.score == 0 {
                continue;
            }
            let i = keep[r.index as usize];
            let Some((ratio, ranges)) = self.judge(windows[i], r.score, &r.indices) else { continue };
            if best.as_ref().is_some_and(|b| b.0 >= ratio) {
                continue;
            }
            let off = starts[i] as u32;
            best = Some((ratio, ranges.iter().map(|&(a, b)| (a + off, b + off)).collect()));
        }
        best
    }
}

fn typo_budget(len: usize, fuzzy: FuzzyLevel) -> Option<u16> {
    if !fuzzy.enabled || len < 6 {
        return None;
    }
    let base = if len >= 10 { 2 } else { 1 };
    Some(base + fuzzy.loose as u16)
}

/// All hits of one term, at most one per (doc, field), sorted by (doc, field).
///
/// Allowed kinds by the char length of `term.parts[0]`: ≤ 3 → Exact (plus Prefix when
/// `multi_term`); 4–5 → Exact|Prefix; ≥ 6 → Exact|Prefix|Fuzzy. Quality: Exact 1.0, Prefix
/// `term_len / token_len`, Fuzzy `0.9 × score / self_score`. Compound parts apply the short-term
/// rule on their own length. Fuzzy hits need `score / self_score ≥ 0.65`, at least
/// `needle_chars − max_typos` matched needle chars (frizbee's typo budget only counts missing
/// chars, so substitutions would otherwise be free) and a matched span of at most
/// `needle_chars + 2 × max_typos + 1` chars. A string whose best alignment fails these is
/// retried on one window per token start (`max_span` chars), so a word's match does not depend
/// on the words around it.
pub fn match_term(index: &Index, term: &Term, multi_term: bool, fuzzy: FuzzyLevel) -> Vec<TermHit> {
    match_term_scoped(index, term, multi_term, fuzzy, Scope::All)
}

/// Docs with any Body token the term (or one of its compound parts) matches exactly or, where
/// the short-term rule allows, by prefix. A superset of the docs `match_term` gives a Body hit.
pub fn body_docs(index: &Index, term: &Term, multi_term: bool) -> DocSet {
    let mut set = DocSet::default();
    for p in &term.parts {
        let mut add = |list: &crate::index::PostingList| list.iter().for_each(|x| set.insert(x.doc));
        if multi_term || p.chars().count() >= 4 {
            index.prefix(Field::Body, p).into_iter().for_each(|(_, l)| add(l));
        } else {
            add(index.exact(Field::Body, p));
        }
    }
    set
}

/// `match_term` restricted to the fields (and Body docs) of `scope`; a field in scope gets the
/// same hits as from `match_term`.
pub fn match_term_scoped(
    index: &Index,
    term: &Term,
    multi_term: bool,
    fuzzy: FuzzyLevel,
    scope: Scope,
) -> Vec<TermHit> {
    let Some(full) = term.parts.first() else { return Vec::new() };
    let len = full.chars().count();
    // Short-term rule, applied to each part on its own length.
    let allow_prefix = |p: &str| multi_term || p.chars().count() >= 4;
    let needle_chars = term.norm.chars().count();

    let mut matcher = typo_budget(len, fuzzy).map(|typos| {
        let config = Config { max_typos: Some(typos), sort: SortStrategy::Unsorted, ..Default::default() };
        let mut m = Matcher::new(term.norm.as_str(), &config);
        let self_score = m.match_list(&[term.norm.as_str()]).first().map_or(0, |x| x.score);
        (m, self_score as f32, typos)
    });
    if matcher.as_ref().is_some_and(|m| m.1 <= 0.0) {
        matcher = None;
    }

    let mut out = Vec::new();
    for field in FIELDS {
        let only = match (field, scope) {
            (Field::Body, Scope::NoBody) => continue,
            (Field::Body, Scope::BodyOnly(s)) => Some(s),
            (_, Scope::BodyOnly(_)) => continue,
            _ => None,
        };
        let mut hits: HashMap<DocId, Acc> = HashMap::new();
        match_token(index, field, full, allow_prefix(full), only, &mut hits);

        if term.parts.len() > 1 {
            let mut parts: Vec<HashMap<DocId, Acc>> = term.parts[1..]
                .iter()
                .map(|p| {
                    let mut m = HashMap::new();
                    match_token(index, field, p, allow_prefix(p), only, &mut m);
                    m
                })
                .collect();
            let n = parts.len() as f32;
            let (first, rest) = parts.split_first_mut().unwrap();
            'docs: for (doc, mut acc) in first.drain() {
                if hits.contains_key(&doc) {
                    continue;
                }
                let mut sum = acc.quality;
                for m in rest.iter_mut() {
                    let Some(a) = m.remove(&doc) else { continue 'docs };
                    sum += a.quality;
                    if kind_rank(a.kind) > kind_rank(acc.kind) {
                        acc.kind = a.kind;
                    }
                    acc.positions.extend(a.positions);
                    acc.tokens.extend(a.tokens);
                }
                acc.quality = sum / n * 0.95;
                hits.insert(doc, acc);
            }
        }

        let fuzzy_here = has_strings(field).then_some(()).and(matcher.as_mut());
        let matched: HashSet<DocId> =
            if fuzzy_here.is_some() { hits.keys().copied().collect() } else { HashSet::new() };
        out.extend(hits.into_iter().map(|(doc, acc)| finish(index, field, doc, acc)));

        if let Some((m, self_score, typos)) = fuzzy_here {
            let max_span = needle_chars + 2 * *typos as usize + 1;
            let strings = index.field_strings(field);
            let hay: Vec<&str> = strings.iter().map(|e| e.2.as_str()).collect();
            let mut best: HashMap<DocId, TermHit> = HashMap::new();
            let rules = FuzzyRules { self_score: *self_score, typos: *typos as usize, needle_chars, max_span };
            for r in m.match_list_indices(&hay) {
                let (doc, hidx, s) = &strings[r.index as usize];
                if r.score == 0 || r.exact || matched.contains(doc) {
                    continue;
                }
                // A rejected whole-string alignment says little about the words in it: frizbee's
                // best alignment may borrow chars from later words (0 typos, too wide), and a word
                // that does not start the string lacks the start bonus (ratio below the cut).
                // Retry on one window per token start and keep the best window that passes.
                let Some((ratio, char_ranges)) = rules
                    .judge(s, r.score, &r.indices)
                    .or_else(|| rules.window_can_pass(r.score).then(|| rules.best_window(m, s)).flatten())
                else {
                    continue;
                };
                let quality = 0.9 * ratio;
                if best.get(doc).is_some_and(|h| h.quality >= quality) {
                    continue;
                }
                best.insert(
                    *doc,
                    TermHit {
                        doc: *doc,
                        field,
                        kind: MatchKind::Fuzzy,
                        quality,
                        positions: SmallVec::new(),
                        heading_idx: is_heading(field).then_some(*hidx),
                        char_ranges,
                    },
                );
            }
            out.extend(best.into_values());
        }
    }
    out.sort_by_key(|h| (h.doc, h.field as u8));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Field;
    use crate::index::Index;
    use crate::markdown::extract;
    use crate::query::Query;

    const NORMAL: FuzzyLevel = FuzzyLevel { enabled: true, loose: false };

    fn ix() -> Index {
        let mut ix = Index::new();
        for (path, text) in [
            ("AI in Data Engineering.md", "Notes on data pipelines.\n## AI workloads\nmore text\n"),
            ("Airflow Basics.md", ""),
            ("aim high.md", ""),
            ("Obsidian Troubleshooting.md", ""),
            ("Other.md", "how to fix troubleshooting issues"),
        ] {
            ix.upsert(extract(path, text, 0.0));
        }
        ix
    }

    fn term(s: &str) -> Term {
        Query::parse(s).terms.remove(0)
    }

    fn titles(ix: &Index, hits: &[TermHit]) -> Vec<String> {
        let mut t: Vec<String> = hits.iter().map(|h| ix.get(h.doc).unwrap().title.clone()).collect();
        t.sort();
        t.dedup();
        t
    }

    #[test]
    fn short_single_term_is_exact_only() {
        let ix = ix();
        let hits = match_term(&ix, &term("ai"), false, NORMAL);
        assert_eq!(titles(&ix, &hits), ["AI in Data Engineering"]);
        assert!(hits.iter().all(|h| h.kind == MatchKind::Exact));
    }

    #[test]
    fn short_multi_term_allows_prefix() {
        let ix = ix();
        let hits = match_term(&ix, &term("ai"), true, NORMAL);
        assert_eq!(titles(&ix, &hits), ["AI in Data Engineering", "Airflow Basics", "aim high"]);
        for h in hits.iter().filter(|h| ix.get(h.doc).unwrap().title != "AI in Data Engineering") {
            assert_eq!(h.kind, MatchKind::Prefix);
            assert!(h.quality < 1.0);
        }
    }

    #[test]
    fn prefix_quality_ratio() {
        let ix = ix();
        let hits = match_term(&ix, &term("engineer"), false, NORMAL);
        let h = hits.iter().find(|h| h.field == Field::Title).unwrap();
        assert_eq!(h.kind, MatchKind::Prefix);
        assert!((h.quality - 8.0 / 11.0).abs() < 1e-6, "{}", h.quality);
        // "ai in data engineering": the matched prefix "engineer" is bytes 11..19.
        assert_eq!(h.char_ranges.as_slice(), &[(11, 19)]);
    }

    #[test]
    fn fuzzy_on_title_only() {
        let ix = ix();
        let hits = match_term(&ix, &term("troubelshoot"), false, NORMAL);
        assert_eq!(titles(&ix, &hits), ["Obsidian Troubleshooting"]);
        let h = &hits[0];
        assert_eq!((h.field, h.kind), (Field::Title, MatchKind::Fuzzy));
        assert!(h.quality > 0.0 && h.quality < 0.9, "{}", h.quality);
        assert!(!h.char_ranges.is_empty());
        // Other.md's body contains "troubleshooting" (a fuzzy match), but Body is never fuzzy-matched.
        assert!(hits.iter().all(|h| h.field != Field::Body));
    }

    #[test]
    fn fuzzy_disabled_level() {
        let ix = ix();
        let off = FuzzyLevel { enabled: false, loose: false };
        let hits = match_term(&ix, &term("troubelshoot"), false, off);
        assert!(hits.iter().all(|h| h.kind != MatchKind::Fuzzy));
        assert!(hits.is_empty());
    }

    #[test]
    fn scoped_matching_splits_body_from_other_fields() {
        let ix = ix();
        let key = |h: &TermHit| (h.doc, h.field as u8, h.kind as u8, h.quality.to_bits(), h.positions.to_vec());
        for q in ["data", "troubleshooting", "ai", "data-engineering"] {
            let t = term(q);
            let all: Vec<_> = match_term(&ix, &t, true, NORMAL).iter().map(key).collect();
            let docs = body_docs(&ix, &t, true);
            let mut split = match_term_scoped(&ix, &t, true, NORMAL, Scope::NoBody);
            assert!(split.iter().all(|h| h.field != Field::Body));
            split.extend(match_term_scoped(&ix, &t, true, NORMAL, Scope::BodyOnly(&docs)));
            split.sort_by_key(|h| (h.doc, h.field as u8));
            assert_eq!(split.iter().map(key).collect::<Vec<_>>(), all, "{q}");
        }
        // Docs outside the set get no Body hit.
        let none = DocSet::default();
        assert!(match_term_scoped(&ix, &term("data"), true, NORMAL, Scope::BodyOnly(&none)).is_empty());
    }

    #[test]
    fn doc_set_ops() {
        let (mut a, mut b) = (DocSet::default(), DocSet::default());
        a.insert(3);
        a.insert(200);
        b.insert(3);
        b.insert(64);
        let mut i = a.clone();
        i.intersect_with(&b);
        assert!(i.contains(3) && !i.contains(200) && !i.contains(64));
        a.union_with(&b);
        assert!([3, 64, 200].iter().all(|&d| a.contains(d)) && !a.contains(4));
    }

    #[test]
    fn body_hits_carry_positions() {
        let ix = ix();
        let hits = match_term(&ix, &term("data"), false, NORMAL);
        let h = hits.iter().find(|h| h.field == Field::Body).unwrap();
        assert_eq!(ix.get(h.doc).unwrap().title, "AI in Data Engineering");
        assert_eq!(h.positions.as_slice(), &[2]);
        assert!(h.char_ranges.is_empty());
    }

    #[test]
    fn heading_hits_carry_heading_idx() {
        let ix = ix();
        let hits = match_term(&ix, &term("workloads"), false, NORMAL);
        let h = hits.iter().find(|h| h.field == Field::H2).unwrap();
        assert_eq!(h.heading_idx, Some(0));
        assert_eq!(h.positions.as_slice(), &[1]);
        assert_eq!(h.char_ranges.as_slice(), &[(3, 12)]);
    }

    #[test]
    fn compound_falls_back_to_all_parts() {
        let ix = ix();
        let hits = match_term(&ix, &term("data-engineering"), false, NORMAL);
        let h = hits.iter().find(|h| h.field == Field::Title).unwrap();
        assert_eq!(ix.get(h.doc).unwrap().title, "AI in Data Engineering");
        assert_eq!(h.kind, MatchKind::Exact);
        assert!((h.quality - 0.95).abs() < 1e-6);
        assert_eq!(h.positions.as_slice(), &[2, 3]);
        // No single field of one doc holds both "data" and "airflow".
        assert!(match_term(&ix, &term("data-airflow"), false, NORMAL).is_empty());
    }

    fn ix_of(docs: &[(&str, &str)]) -> Index {
        let mut ix = Index::new();
        for (path, text) in docs {
            ix.upsert(extract(path, text, 0.0));
        }
        ix
    }

    #[test]
    fn compound_short_part_is_exact_only() {
        let ix = ix_of(&[("General Airflow.md", ""), ("Airflow Basics.md", "")]);
        assert!(match_term(&ix, &term("gen-ai"), false, NORMAL).is_empty());
        let multi = Query::parse("gen-ai x").terms.remove(0);
        let hits = match_term(&ix, &multi, true, NORMAL);
        assert_eq!(titles(&ix, &hits), ["General Airflow"]);
    }

    #[test]
    fn fuzzy_rejects_scattered_subsequence() {
        let ix = ix_of(&[("AI rules for low budgets.md", ""), ("Gene editing in nature.md", "")]);
        assert!(match_term(&ix, &term("airflow"), false, NORMAL).is_empty());
        assert!(match_term(&ix, &term("engine"), false, NORMAL).is_empty());
    }

    #[test]
    fn fuzzy_rejects_substitution_heavy_match() {
        let ix = ix_of(&[("Airbyte Checkpointing.md", ""), ("Social Media Prescheduled Long Format.md", "")]);
        assert!(match_term(&ix, &term("writing"), false, NORMAL).is_empty());
        assert!(match_term(&ix, &term("should"), false, NORMAL).is_empty());
    }

    /// The matched-char rule on its own: frizbee scores `troubleshooting` against
    /// "utxx drbleshooting" at ratio ≥ 0.65 with a span inside the limit, but only 12 of the 15
    /// needle chars are matched (2 typos allowed → 13 needed): `r`, `o` are substituted, not typed.
    #[test]
    fn fuzzy_matched_char_rule_rejects_despite_ratio() {
        let (needle, hay) = ("troubleshooting", "utxx drbleshooting");
        let config = Config { max_typos: Some(2), sort: SortStrategy::Unsorted, ..Default::default() };
        let mut m = Matcher::new(needle, &config);
        let self_score = m.match_list(&[needle])[0].score as f32;
        let r = &m.match_list_indices(&[hay])[0];
        let (ranges, matched) = byte_ranges(hay, &r.indices);
        let span = hay[ranges[0].0 as usize..ranges.last().unwrap().1 as usize].chars().count();
        assert!(r.score as f32 / self_score >= MIN_FUZZY_RATIO, "{}", r.score as f32 / self_score);
        assert!(span <= 15 + 2 * 2 + 1, "{span}");
        assert_eq!(matched, 12);
        let ix = ix_of(&[("Utxx Drbleshooting.md", "")]);
        assert!(match_term(&ix, &term(needle), false, NORMAL).is_empty());
    }

    #[test]
    fn fuzzy_keeps_real_typo() {
        let ix = ix_of(&[("Obsidian Troubleshooting.md", "")]);
        let hits = match_term(&ix, &term("troubelshoot"), false, NORMAL);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].field, hits[0].kind), (Field::Title, MatchKind::Fuzzy));
    }

    /// frizbee's best alignment of `obsidain` in "obsidian troubleshooting" borrows the `i`, `n`
    /// of "troubleshooting" (0 typos, span too wide); the local 1-typo alignment on "obsidian"
    /// must still be found, and its ranges must point into the full title.
    #[test]
    fn fuzzy_retries_windows_when_best_alignment_is_too_wide() {
        let ix = ix_of(&[
            ("Obsidian Troubleshooting.md", ""),
            ("Obsidian Search (lots of files).md", ""),
            ("Obsidian Notes.md", ""),
        ]);
        let hits = match_term(&ix, &term("obsidain"), false, NORMAL);
        assert_eq!(titles(&ix, &hits), ["Obsidian Notes", "Obsidian Search (lots of files)", "Obsidian Troubleshooting"]);
        assert!(hits.iter().all(|h| (h.field, h.kind) == (Field::Title, MatchKind::Fuzzy)));
        let h = hits.iter().find(|h| ix.get(h.doc).unwrap().title == "Obsidian Troubleshooting").unwrap();
        let (s, e) = (h.char_ranges[0].0, h.char_ranges.last().unwrap().1);
        assert!(s == 0 && e <= 8, "{:?}", h.char_ranges);
        let hits = match_term(&ix, &term("obsidina"), false, NORMAL);
        assert!(titles(&ix, &hits).contains(&"Obsidian Search (lots of files)".to_string()), "{:?}", titles(&ix, &hits));
        // A hit found in a later window is offset back into the full string.
        let ix = ix_of(&[("Notes on Obsidian Troubleshooting.md", "")]);
        let hits = match_term(&ix, &term("obsidain"), false, NORMAL);
        assert_eq!(hits.len(), 1, "{hits:?}");
        let (s, e) = (hits[0].char_ranges[0].0, hits[0].char_ranges.last().unwrap().1);
        assert!(s >= 9 && e <= 17, "{:?}", hits[0].char_ranges);
    }

    #[test]
    fn hits_sorted_and_collapsed() {
        let ix = ix();
        let hits = match_term(&ix, &term("ai"), true, NORMAL);
        let keys: Vec<_> = hits.iter().map(|h| (h.doc, h.field as u8)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(keys, sorted);
    }
}
