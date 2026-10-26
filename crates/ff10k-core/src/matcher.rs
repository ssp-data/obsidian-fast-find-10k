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
use crate::index::{DocId, Index, Posting};
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

    fn add(&mut self, field: Field, kind: MatchKind, quality: f32, p: &Posting, hl: Option<(&'a str, usize)>) {
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
            self.positions.extend_from_slice(&p.positions);
        }
        self.tokens.extend(hl);
    }
}

/// Exact (and, when allowed, prefix) matches of `needle` in `field`, merged into `out`.
fn match_token<'a>(
    index: &'a Index,
    field: Field,
    needle: &'a str,
    allow_prefix: bool,
    out: &mut HashMap<DocId, Acc<'a>>,
) {
    let track = has_strings(field);
    let needle_chars = needle.chars().count() as f32;
    let mut add = |tok: &'a str, postings: &[Posting]| {
        let (kind, quality) = if tok.len() == needle.len() {
            (MatchKind::Exact, 1.0)
        } else {
            (MatchKind::Prefix, needle_chars / tok.chars().count() as f32)
        };
        let hl = track.then_some((tok, needle.len()));
        for p in postings {
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

/// frizbee match indices are byte offsets of matched bytes; widen each to its whole char.
fn byte_ranges(s: &str, indices: &[u32]) -> SmallVec<[(u32, u32); 4]> {
    let mut r = SmallVec::new();
    for &i in indices {
        let mut start = (i as usize).min(s.len());
        while !s.is_char_boundary(start) {
            start -= 1;
        }
        let end = start + s[start..].chars().next().map_or(0, char::len_utf8);
        r.push((start as u32, end as u32));
    }
    merge(r)
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
/// `term_len / token_len`, Fuzzy `0.9 × score / self_score`.
pub fn match_term(index: &Index, term: &Term, multi_term: bool, fuzzy: FuzzyLevel) -> Vec<TermHit> {
    let Some(full) = term.parts.first() else { return Vec::new() };
    let len = full.chars().count();
    let allow_prefix = len >= 4 || multi_term;

    let mut matcher = typo_budget(len, fuzzy).map(|typos| {
        let config = Config { max_typos: Some(typos), sort: SortStrategy::Unsorted, ..Default::default() };
        let mut m = Matcher::new(term.norm.as_str(), &config);
        let self_score = m.match_list(&[term.norm.as_str()]).first().map_or(0, |x| x.score);
        (m, self_score as f32)
    });
    if matcher.as_ref().is_some_and(|m| m.1 <= 0.0) {
        matcher = None;
    }

    let mut out = Vec::new();
    for field in FIELDS {
        let mut hits: HashMap<DocId, Acc> = HashMap::new();
        match_token(index, field, full, allow_prefix, &mut hits);

        if term.parts.len() > 1 {
            let mut parts: Vec<HashMap<DocId, Acc>> = term.parts[1..]
                .iter()
                .map(|p| {
                    let mut m = HashMap::new();
                    match_token(index, field, p, allow_prefix, &mut m);
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

        if let Some((m, self_score)) = fuzzy_here {
            let strings = index.field_strings(field);
            let hay: Vec<&str> = strings.iter().map(|e| e.2.as_str()).collect();
            let mut best: HashMap<DocId, TermHit> = HashMap::new();
            for r in m.match_list_indices(&hay) {
                let (doc, hidx, s) = &strings[r.index as usize];
                if r.score == 0 || r.exact || matched.contains(doc) {
                    continue;
                }
                let quality = 0.9 * r.score as f32 / *self_score;
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
                        char_ranges: byte_ranges(s, &r.indices),
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

