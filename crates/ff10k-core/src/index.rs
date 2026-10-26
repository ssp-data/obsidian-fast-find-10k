//! Index: doc table plus one inverted index (sorted term dictionary) per field.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;

use smallvec::SmallVec;

use crate::doc::{Doc, Field};
use crate::markdown::title_and_segments;
use crate::normalize::{normalize, strip_symbols};
use crate::tokenize::tokenize;

pub type DocId = u32;

/// Maximum number of body tokens that get positions; later tokens are indexed without them.
pub const BODY_POSITION_CAP: usize = 50_000;

/// One doc's occurrences of a term in a field.
///
/// `positions`: for `Body`, `Title`, `Alias`, `Tag`, `Link`, `Path` the token `pos` within its
/// string. For `H1/H2/H3` each entry is packed `(heading_idx << 16) | pos` (`pos` masked to 16
/// bits), where `heading_idx` indexes `Doc::headings`. Body tokens beyond `BODY_POSITION_CAP`
/// create the posting but add no position (so `positions` may be empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Posting {
    pub doc: DocId,
    pub positions: SmallVec<[u32; 4]>,
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

fn fidx(f: Field) -> usize {
    FIELDS.iter().position(|x| *x == f).unwrap()
}

struct Record {
    doc: Doc,
    text: String,
    keys: Keys,
}

/// Per-doc normalized strings, computed once per upsert/rename so queries never re-normalize.
struct Keys {
    /// `normalize(strip_symbols(path))`, for `path:` filters and downranked folders.
    path_norm: String,
    /// Link-count keys for backlinks: by title and by extension-less path.
    title_key: String,
    path_key: Option<String>,
    /// Alphanumeric runs in the title (for the short-title bonus).
    title_tokens: u16,
}

impl Keys {
    fn of(doc: &Doc) -> Keys {
        let title_key = normalize(&doc.title);
        let path_key = normalize(stem(&doc.path));
        Keys {
            path_norm: normalize(&strip_symbols(&doc.path)),
            path_key: (path_key != title_key).then_some(path_key),
            title_tokens: doc
                .title
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
                .count()
                .min(u16::MAX as usize) as u16,
            title_key,
        }
    }
}

#[derive(Default)]
pub struct Index {
    records: Vec<Option<Record>>,
    free: Vec<DocId>,
    path_to_id: HashMap<String, DocId>,
    dicts: [BTreeMap<String, Vec<Posting>>; 9],
    /// normalized link target -> number of links with that target.
    link_counts: HashMap<String, u32>,
    /// Lazily built `field_strings` caches (Title, Alias, H1, H2, H3); every mutator clears them.
    strings: [OnceCell<Vec<(DocId, u16, String)>>; 5],
    /// Lazily built backlink count per DocId (0 for free slots); cleared with `strings`.
    backlink_cache: OnceCell<Vec<u32>>,
    live: usize,
}

type TermMap = HashMap<String, SmallVec<[u32; 4]>>;

fn add_tokens(map: &mut TermMap, raw: &str, hidx: Option<usize>) {
    for t in tokenize(raw) {
        let p = match hidx {
            Some(h) => ((h as u32) << 16) | (t.pos & 0xFFFF),
            None => t.pos,
        };
        map.entry(t.text).or_default().push(p);
    }
}

/// Derive every (field, term -> positions) of a doc. Deterministic, so `remove` can re-derive it.
fn derive(doc: &Doc) -> Vec<TermMap> {
    let mut maps: Vec<TermMap> = (0..FIELDS.len()).map(|_| TermMap::new()).collect();
    add_tokens(&mut maps[fidx(Field::Title)], &strip_symbols(&doc.title), None);
    for a in &doc.aliases {
        add_tokens(&mut maps[fidx(Field::Alias)], a, None);
    }
    for (i, h) in doc.headings.iter().enumerate() {
        let f = match h.level {
            1 => Field::H1,
            2 => Field::H2,
            3 => Field::H3,
            _ => continue,
        };
        add_tokens(&mut maps[fidx(f)], &h.text, Some(i));
    }
    for (i, t) in doc.tags.iter().enumerate() {
        let n = normalize(t);
        if !n.is_empty() {
            maps[fidx(Field::Tag)].entry(n).or_default().push(i as u32);
        }
    }
    for l in &doc.links {
        add_tokens(&mut maps[fidx(Field::Link)], &l.target, None);
        if let Some(a) = &l.alias {
            add_tokens(&mut maps[fidx(Field::Link)], a, None);
        }
    }
    for s in &doc.path_segments {
        add_tokens(&mut maps[fidx(Field::Path)], &strip_symbols(s), None);
    }
    let body = &mut maps[fidx(Field::Body)];
    for (i, t) in tokenize(&doc.body).into_iter().enumerate() {
        let e = body.entry(t.text).or_default();
        if i < BODY_POSITION_CAP {
            e.push(t.pos);
        }
    }
    maps
}

fn stem(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

fn link_key(target: &str) -> String {
    normalize(stem(target.trim()))
}

impl Index {
    pub fn new() -> Self {
        Index::default()
    }

    pub fn upsert(&mut self, doc: Doc) -> DocId {
        self.upsert_with_text(doc, String::new())
    }

    /// Insert or replace by path, keeping the raw note text for previews.
    pub fn upsert_with_text(&mut self, doc: Doc, text: String) -> DocId {
        let id = match self.path_to_id.get(&doc.path).copied() {
            Some(id) => {
                self.unindex(id);
                id
            }
            None => match self.free.pop() {
                Some(id) => id,
                None => {
                    self.records.push(None);
                    (self.records.len() - 1) as DocId
                }
            },
        };
        self.path_to_id.insert(doc.path.clone(), id);
        self.index(id, &doc);
        if self.records[id as usize].is_none() {
            self.live += 1;
        }
        self.records[id as usize] = Some(Record { keys: Keys::of(&doc), doc, text });
        self.clear_strings();
        id
    }

    fn index(&mut self, id: DocId, doc: &Doc) {
        for (fi, map) in derive(doc).into_iter().enumerate() {
            for (term, positions) in map {
                let list = self.dicts[fi].entry(term).or_default();
                let p = Posting { doc: id, positions };
                // Keep lists sorted by doc id (ids can be reused out of order).
                match list.last() {
                    Some(last) if last.doc > id => {
                        let at = list.partition_point(|x| x.doc < id);
                        list.insert(at, p);
                    }
                    _ => list.push(p),
                }
            }
        }
        for l in &doc.links {
            *self.link_counts.entry(link_key(&l.target)).or_insert(0) += 1;
        }
    }

    /// Drop the doc's postings. Re-derives its terms and filters only those posting lists
    /// (cost bounded by the doc's distinct terms times their list lengths).
    fn unindex(&mut self, id: DocId) {
        let Some(rec) = self.records[id as usize].as_ref() else { return };
        let maps = derive(&rec.doc);
        for (fi, map) in maps.into_iter().enumerate() {
            for term in map.keys() {
                if let Some(list) = self.dicts[fi].get_mut(term) {
                    if let Ok(at) = list.binary_search_by_key(&id, |p| p.doc) {
                        list.remove(at);
                    }
                    if list.is_empty() {
                        self.dicts[fi].remove(term);
                    }
                }
            }
        }
        for l in &rec.doc.links {
            let k = link_key(&l.target);
            if let Some(c) = self.link_counts.get_mut(&k) {
                *c -= 1;
                if *c == 0 {
                    self.link_counts.remove(&k);
                }
            }
        }
    }

    pub fn remove(&mut self, path: &str) -> Option<DocId> {
        let id = self.path_to_id.remove(path)?;
        self.unindex(id);
        self.records[id as usize] = None;
        self.free.push(id);
        self.live -= 1;
        self.clear_strings();
        Some(id)
    }

    /// Move a doc to a new path, keeping its id; Title and Path postings are rebuilt.
    /// Returns false if `old` is unknown or `new` is already taken.
    pub fn rename(&mut self, old: &str, new: &str) -> bool {
        if old == new || self.path_to_id.contains_key(new) {
            return false;
        }
        let Some(&id) = self.path_to_id.get(old) else { return false };
        self.unindex(id);
        let rec = self.records[id as usize].as_mut().unwrap();
        let (title, segs) = title_and_segments(new);
        rec.doc.path = new.to_string();
        rec.doc.title = title;
        rec.doc.path_segments = segs;
        rec.keys = Keys::of(&rec.doc);
        let doc = rec.doc.clone();
        self.index(id, &doc);
        self.path_to_id.remove(old);
        self.path_to_id.insert(new.to_string(), id);
        self.clear_strings();
        true
    }

    pub fn get(&self, id: DocId) -> Option<&Doc> {
        self.records.get(id as usize)?.as_ref().map(|r| &r.doc)
    }

    /// `normalize(strip_symbols(path))` of a doc, cached at upsert/rename.
    pub fn path_norm(&self, id: DocId) -> Option<&str> {
        self.records.get(id as usize)?.as_ref().map(|r| r.keys.path_norm.as_str())
    }

    /// Number of alphanumeric runs in the doc's title, cached at upsert/rename.
    pub fn title_tokens(&self, id: DocId) -> u16 {
        self.records.get(id as usize).and_then(Option::as_ref).map_or(0, |r| r.keys.title_tokens)
    }

    pub fn text(&self, id: DocId) -> Option<&str> {
        self.records.get(id as usize)?.as_ref().map(|r| r.text.as_str())
    }

    /// Ids of all live docs, ascending.
    pub fn ids(&self) -> impl Iterator<Item = DocId> + '_ {
        self.records.iter().enumerate().filter(|(_, r)| r.is_some()).map(|(i, _)| i as DocId)
    }

    pub fn id_of(&self, path: &str) -> Option<DocId> {
        self.path_to_id.get(path).copied()
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    pub fn exact(&self, field: Field, token: &str) -> &[Posting] {
        self.dicts[fidx(field)].get(token).map_or(&[], |v| v.as_slice())
    }

    /// All terms starting with `prefix`, in dictionary order. Empty prefix yields nothing.
    pub fn prefix(&self, field: Field, prefix: &str) -> Vec<(&str, &[Posting])> {
        if prefix.is_empty() {
            return Vec::new();
        }
        self.dicts[fidx(field)]
            .range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect()
    }

    fn clear_strings(&mut self) {
        for c in &mut self.strings {
            c.take();
        }
        self.backlink_cache.take();
    }

    /// Normalized strings for fuzzy matching, as `(doc, heading_idx, string)`; `heading_idx` is 0
    /// for Title/Alias. Other fields yield an empty slice. Built lazily and cached until the next
    /// mutation; each rebuild is O(N docs) for the requested field.
    pub fn field_strings(&self, field: Field) -> &[(DocId, u16, String)] {
        let (slot, level) = match field {
            Field::Title => (0, 0),
            Field::Alias => (1, 0),
            Field::H1 => (2, 1),
            Field::H2 => (3, 2),
            Field::H3 => (4, 3),
            _ => return &[],
        };
        self.strings[slot].get_or_init(|| {
            let mut out = Vec::new();
            for (id, rec) in self.records.iter().enumerate() {
                let Some(rec) = rec else { continue };
                let id = id as DocId;
                match field {
                    Field::Title => out.push((id, 0, normalize(&strip_symbols(&rec.doc.title)))),
                    Field::Alias => {
                        out.extend(rec.doc.aliases.iter().map(|a| (id, 0, normalize(a))))
                    }
                    _ => {
                        for (i, h) in rec.doc.headings.iter().enumerate() {
                            if h.level == level {
                                out.push((id, i as u16, normalize(&h.text)));
                            }
                        }
                    }
                }
            }
            out
        })
    }

    /// Links pointing at the doc (by title or extension-less path). O(1) after a lazy O(N)
    /// rebuild following each mutation.
    pub fn backlinks(&self, id: DocId) -> u32 {
        let counts = self.backlink_cache.get_or_init(|| {
            let count = |k: &str| self.link_counts.get(k).copied().unwrap_or(0);
            self.records
                .iter()
                .map(|r| {
                    r.as_ref().map_or(0, |r| {
                        count(&r.keys.title_key) + r.keys.path_key.as_deref().map_or(0, count)
                    })
                })
                .collect()
        });
        counts.get(id as usize).copied().unwrap_or(0)
    }

    pub fn docs_with_tag(&self, tag: &str) -> Vec<DocId> {
        self.exact(Field::Tag, &normalize(tag)).iter().map(|p| p.doc).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::extract;
    use std::time::Instant;

    fn mk(path: &str, text: &str) -> Doc {
        extract(path, text, 0.0)
    }

    #[test]
    fn upsert_and_exact_lookup() {
        let mut ix = Index::new();
        let a = ix.upsert(mk("AI notes.md", "x"));
        let _b = ix.upsert(mk("Data Engineering.md", "x"));
        let hits = ix.exact(Field::Title, "ai");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc, a);
    }

    #[test]
    fn prefix_range() {
        let mut ix = Index::new();
        for t in ["engineering", "engineer", "english", "zebra"] {
            ix.upsert(mk(&format!("{t}.md"), ""));
        }
        let terms: Vec<_> = ix.prefix(Field::Title, "eng").iter().map(|x| x.0).collect();
        assert_eq!(terms, vec!["engineer", "engineering", "english"]);
        let terms: Vec<_> = ix.prefix(Field::Title, "engin").iter().map(|x| x.0).collect();
        assert_eq!(terms, vec!["engineer", "engineering"]);
        assert!(ix.prefix(Field::Title, "").is_empty());
    }

    #[test]
    fn upsert_replaces() {
        let mut ix = Index::new();
        let a = ix.upsert(mk("f/old.md", "alpha"));
        let mut d = mk("f/old.md", "beta");
        d.title = "fresh".into();
        assert_eq!(ix.upsert(d), a);
        assert!(ix.exact(Field::Title, "old").is_empty());
        assert_eq!(ix.exact(Field::Title, "fresh").len(), 1);
        assert!(ix.exact(Field::Body, "alpha").is_empty());
        assert_eq!(ix.exact(Field::Body, "beta").len(), 1);
        assert_eq!(ix.len(), 1);
    }

    #[test]
    fn remove_frees_postings() {
        let mut ix = Index::new();
        ix.upsert(mk("a.md", "hello"));
        ix.upsert(mk("b.md", "hello"));
        assert_eq!(ix.remove("a.md"), Some(0));
        assert_eq!(ix.len(), 1);
        assert!(ix.exact(Field::Title, "a").is_empty());
        assert_eq!(ix.exact(Field::Body, "hello").len(), 1);
        assert_eq!(ix.remove("a.md"), None);
        // id reuse keeps lists sorted
        let c = ix.upsert(mk("c.md", "hello"));
        assert_eq!(c, 0);
        let docs: Vec<_> = ix.exact(Field::Body, "hello").iter().map(|p| p.doc).collect();
        assert_eq!(docs, vec![0, 1]);
    }

    #[test]
    fn rename_keeps_id() {
        let mut ix = Index::new();
        let id = ix.upsert(mk("a/x.md", "body"));
        assert!(ix.rename("a/x.md", "b/y.md"));
        assert_eq!(ix.id_of("b/y.md"), Some(id));
        assert_eq!(ix.id_of("a/x.md"), None);
        let d = ix.get(id).unwrap();
        assert_eq!(d.title, "y");
        assert_eq!(d.path_segments, vec!["b"]);
        assert!(ix.exact(Field::Title, "x").is_empty());
        assert_eq!(ix.exact(Field::Title, "y").len(), 1);
        assert!(ix.exact(Field::Path, "a").is_empty());
        assert_eq!(ix.exact(Field::Path, "b").len(), 1);
        assert!(!ix.rename("nope.md", "z.md"));
    }

    #[test]
    fn backlinks_counted_by_target_basename() {
        let mut ix = Index::new();
        let b = ix.upsert(mk("B.md", ""));
        ix.upsert(mk("A.md", "see [[B]]"));
        ix.upsert(mk("C.md", "see [[B|bee]]"));
        assert_eq!(ix.backlinks(b), 2);
        ix.remove("C.md");
        assert_eq!(ix.backlinks(b), 1);
    }

    #[test]
    fn cached_keys_follow_rename_and_link_changes() {
        let mut ix = Index::new();
        let id = ix.upsert(mk("💡 Area/Old Name.md", ""));
        assert_eq!(ix.path_norm(id), Some("area/old name.md"));
        assert_eq!(ix.title_tokens(id), 2);
        assert_eq!(ix.backlinks(id), 0);
        assert!(ix.rename("💡 Area/Old Name.md", "Archive/New-Long Name.md"));
        assert_eq!(ix.path_norm(id), Some("archive/new-long name.md"));
        assert_eq!(ix.title_tokens(id), 3);
        // The backlink cache is rebuilt after a mutation adds a link to the renamed doc.
        assert_eq!(ix.backlinks(id), 0);
        ix.upsert(mk("A.md", "see [[New-Long Name]]"));
        assert_eq!(ix.backlinks(id), 1);
    }

    #[test]
    fn huge_body_caps_positions() {
        let body = "word ".repeat(200_000);
        let doc = mk("big.md", &body);
        let mut ix = Index::new();
        let t = Instant::now();
        ix.upsert(doc);
        assert!(t.elapsed().as_secs_f32() < 2.0, "{:?}", t.elapsed());
        let p = &ix.exact(Field::Body, "word")[0];
        assert!(p.positions.len() <= BODY_POSITION_CAP);
        assert_eq!(p.positions.len(), BODY_POSITION_CAP);
    }

    #[test]
    fn field_strings_and_headings() {
        let mut ix = Index::new();
        let id = ix.upsert(mk("n.md", "---\naliases: [Foo Bar]\n---\n# Top\n## Sub Heading\n"));
        assert_eq!(ix.field_strings(Field::Title), &[(id, 0, "n".to_string())]);
        assert_eq!(ix.field_strings(Field::Alias), &[(id, 0, "foo bar".to_string())]);
        assert_eq!(ix.field_strings(Field::H2), &[(id, 1, "sub heading".to_string())]);
        let p = &ix.exact(Field::H2, "sub")[0];
        assert_eq!(p.positions.as_slice(), &[1u32 << 16]);
        ix.remove("n.md");
        assert!(ix.field_strings(Field::Title).is_empty());
    }

    #[test]
    fn field_strings_not_stale_after_id_reuse() {
        let mut ix = Index::new();
        ix.upsert(mk("A.md", ""));
        assert_eq!(ix.field_strings(Field::Title)[0].2, "a");
        ix.remove("A.md");
        let id = ix.upsert(mk("B.md", ""));
        assert_eq!(ix.field_strings(Field::Title), &[(id, 0, "b".to_string())]);
    }

    #[test]
    fn rename_matches_extract() {
        let mut ix = Index::new();
        let id = ix.upsert(mk("old.md", ""));
        let new = "💡 Resources/🧮 Zettelkasten/New Name.md";
        assert!(ix.rename("old.md", new));
        let e = mk(new, "");
        let d = ix.get(id).unwrap();
        assert_eq!(d.title, e.title);
        assert_eq!(d.path_segments, e.path_segments);
    }

    #[test]
    fn tags_and_text() {
        let mut ix = Index::new();
        let id = ix.upsert_with_text(mk("t.md", "---\ntags: [a/b]\n---\nx"), "RAW".into());
        assert_eq!(ix.docs_with_tag("a"), vec![id]);
        assert_eq!(ix.docs_with_tag("a/b"), vec![id]);
        assert!(ix.docs_with_tag("b2").is_empty());
        assert_eq!(ix.text(id), Some("RAW"));
    }
}
