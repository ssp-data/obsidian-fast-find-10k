//! Index: doc table plus one inverted index (sorted term dictionary) per field.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Bound;
use std::rc::Rc;

use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::doc::{Doc, Field};
use crate::markdown::{body_line, extract_note, title_and_segments};
use crate::normalize::{normalize, strip_symbols};
use crate::similar::{word_spans, TitleKeys};
use crate::tokenize::{for_each_token_at, for_each_token_with, Scratch, Token};

pub type DocId = u32;

/// Time spent in each phase of `Index::upsert_profiled`, accumulated across calls (native only;
/// the plain `upsert*` paths never read the clock).
#[derive(Clone, Copy, Debug, Default)]
pub struct UpsertProfile {
    /// `markdown::extract` (filled by `Engine::upsert_profiled`).
    pub extract: std::time::Duration,
    /// Re-deriving and dropping the old postings of a replaced doc.
    pub unindex: std::time::Duration,
    /// Tokenizing every field into per-field term maps.
    pub derive: std::time::Duration,
    /// Merging the term maps into the posting lists, plus link counts.
    pub insert: std::time::Duration,
    /// Cached per-doc keys (`Keys::of`: path/title/alias normalization, `TitleKeys`).
    pub keys: std::time::Duration,
}

/// Runs `f`, adding its wall time to `slot` when profiling (never reads the clock otherwise).
pub(crate) fn timed<T>(slot: Option<&mut std::time::Duration>, f: impl FnOnce() -> T) -> T {
    match slot {
        #[cfg(not(target_arch = "wasm32"))]
        Some(d) => {
            let t = std::time::Instant::now();
            let out = f();
            *d += t.elapsed();
            out
        }
        _ => f(),
    }
}

/// Maximum number of body tokens that get positions; later tokens are indexed without them.
pub const BODY_POSITION_CAP: usize = 50_000;

/// One doc's occurrences of a term in a field (a view into a `PostingList`).
///
/// `positions`: for `Body`, `Title`, `Alias`, `Tag`, `Link`, `Path` the token `pos` within its
/// string. For `H1/H2/H3` each entry is packed `(heading_idx << 16) | pos` (`pos` masked to 16
/// bits), where `heading_idx` indexes `Doc::headings`. Body tokens beyond `BODY_POSITION_CAP`
/// create the posting but add no position (so `positions` may be empty).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Posting<'a> {
    pub doc: DocId,
    pub positions: &'a [u32],
}

/// A term's postings in one field, sorted by doc id, stored column-wise: 8 bytes per posting
/// plus 4 per position (a `Vec` of structs with inline positions cost ~40 bytes per posting).
#[derive(Default, Debug)]
pub struct PostingList {
    docs: Vec<DocId>,
    /// `positions[ends[i - 1]..ends[i]]` belong to `docs[i]` (`0..ends[0]` for the first).
    ends: Vec<u32>,
    positions: Vec<u32>,
}

static EMPTY_LIST: PostingList = PostingList { docs: Vec::new(), ends: Vec::new(), positions: Vec::new() };

impl PostingList {
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    fn start(&self, i: usize) -> usize {
        if i == 0 { 0 } else { self.ends[i - 1] as usize }
    }

    pub fn get(&self, i: usize) -> Option<Posting<'_>> {
        let doc = *self.docs.get(i)?;
        Some(Posting { doc, positions: &self.positions[self.start(i)..self.ends[i] as usize] })
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = Posting<'_>> + '_ {
        (0..self.docs.len()).map(|i| self.get(i).expect("index in range"))
    }

    /// Adds `doc`'s posting, keeping doc order (ids can be reused out of order). `doc` must
    /// not be in the list yet.
    fn insert(&mut self, doc: DocId, positions: &[u32]) {
        let n = positions.len() as u32;
        if self.docs.last().is_none_or(|&last| last < doc) {
            self.docs.push(doc);
            self.positions.extend_from_slice(positions);
            self.ends.push(self.positions.len() as u32);
            return;
        }
        let at = self.docs.partition_point(|&d| d < doc);
        let start = self.start(at);
        self.docs.insert(at, doc);
        self.positions.splice(start..start, positions.iter().copied());
        self.ends.insert(at, start as u32 + n);
        for e in &mut self.ends[at + 1..] {
            *e += n;
        }
    }

    /// Drops `doc`'s posting, if present.
    fn remove(&mut self, doc: DocId) {
        let Ok(at) = self.docs.binary_search(&doc) else { return };
        let (start, end) = (self.start(at), self.ends[at] as usize);
        self.docs.remove(at);
        self.ends.remove(at);
        self.positions.drain(start..end);
        let n = (end - start) as u32;
        for e in &mut self.ends[at..] {
            *e -= n;
        }
    }
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
    /// Raw note text (`upsert_note`); empty for `upsert`.
    text: String,
    keys: Keys,
    body: BodySrc,
    /// Token position at the start of each body line (`\n`-separated), for excerpt windows.
    line_pos: Vec<u32>,
}

/// Where a record's body lines come from.
enum BodySrc {
    /// `doc.body` as given (`upsert`).
    Kept,
    /// `doc.body` and its line starts were dropped (`upsert_note`): body line `i` is
    /// `markdown::body_line(text, src[i])`, so the body is not stored next to the raw text.
    Text(Vec<u32>),
}

impl Record {
    fn body_lines(&self) -> usize {
        match &self.body {
            BodySrc::Kept => self.line_pos.len(),
            BodySrc::Text(src) => src.len().min(self.line_pos.len()),
        }
    }

    fn body_line(&self, i: usize) -> Cow<'_, str> {
        match &self.body {
            BodySrc::Kept => Cow::Borrowed(self.doc.body.split('\n').nth(i).unwrap_or("")),
            BodySrc::Text(src) => Cow::Owned(body_line(&self.text, src[i])),
        }
    }

    /// The doc with its body (re-extracted from the raw text when it was dropped).
    fn full_doc(&self) -> Cow<'_, Doc> {
        match &self.body {
            BodySrc::Kept => Cow::Borrowed(&self.doc),
            BodySrc::Text(_) => Cow::Owned(extract_note(&self.doc.path, &self.text, self.doc.mtime).0),
        }
    }
}

/// Body text around one token position: whole body lines `first_line..` joined by `\n`, with
/// every token in it (`byte_start`/`byte_end` relative to `text`, `pos` as in the postings).
pub struct BodyWindow {
    pub text: String,
    pub first_line: usize,
    pub tokens: Vec<Token>,
}

/// Per-doc normalized strings, computed once per upsert/rename so queries never re-normalize.
struct Keys {
    /// `normalize(strip_symbols(path))`, for `path:` filters and downranked folders.
    path_norm: String,
    /// Link-count keys for backlinks: by title and by extension-less path.
    title_key: String,
    path_key: Option<String>,
    /// Normalized/canonical title and sorted non-stopword token set, for near-duplicate lookups.
    sim: TitleKeys,
    /// Per alias: `normalize(alias)` and its `word_spans`, in `doc.aliases` order.
    aliases: Vec<(String, Vec<(u32, u32)>)>,
}

impl Keys {
    fn of(doc: &Doc) -> Keys {
        let title_key = normalize(&doc.title);
        let path_key = normalize(stem(&doc.path));
        Keys {
            path_norm: normalize(&strip_symbols(&doc.path)),
            path_key: (path_key != title_key).then_some(path_key),
            sim: TitleKeys::of(&doc.title),
            aliases: doc
                .aliases
                .iter()
                .map(|a| {
                    let norm = normalize(a);
                    let spans = word_spans(&norm);
                    (norm, spans)
                })
                .collect(),
            title_key,
        }
    }
}

#[derive(Default)]
pub struct Index {
    records: Vec<Option<Record>>,
    free: Vec<DocId>,
    path_to_id: HashMap<String, DocId>,
    dicts: [Dict; 9],
    /// normalized link target -> number of links with that target.
    link_counts: HashMap<String, u32>,
    /// Lazily built `field_strings` caches (Title, Alias, H1, H2, H3); every mutator clears them.
    strings: [OnceCell<Vec<(DocId, u16, String)>>; 5],
    /// Lazily built backlink count per DocId (0 for free slots); cleared with `strings`.
    backlink_cache: OnceCell<Vec<u32>>,
    live: usize,
}

/// One field's term dictionary: a hash map for exact lookups and posting-list updates, and a
/// sorted map (sharing the key strings) for prefix range scans. Only a term's first and last
/// posting touch the sorted map.
#[derive(Default)]
struct Dict {
    ids: FxHashMap<Rc<str>, u32>,
    sorted: BTreeMap<Rc<str>, u32>,
    lists: Vec<PostingList>,
    free: Vec<u32>,
}

impl Dict {
    fn get(&self, term: &str) -> &PostingList {
        self.ids.get(term).map_or(&EMPTY_LIST, |&i| &self.lists[i as usize])
    }

    fn list_mut(&mut self, term: String) -> &mut PostingList {
        let slot = match self.ids.get(term.as_str()) {
            Some(&i) => i,
            None => {
                let i = self.free.pop().unwrap_or_else(|| {
                    self.lists.push(PostingList::default());
                    (self.lists.len() - 1) as u32
                });
                let key: Rc<str> = term.into();
                self.ids.insert(key.clone(), i);
                self.sorted.insert(key, i);
                i
            }
        };
        &mut self.lists[slot as usize]
    }

    /// Drops `doc`'s posting from the term's list, and the term once its list is empty.
    fn remove_doc(&mut self, term: &str, doc: DocId) {
        let Some(&i) = self.ids.get(term) else { return };
        let list = &mut self.lists[i as usize];
        list.remove(doc);
        if list.is_empty() {
            *list = PostingList::default();
            self.ids.remove(term);
            self.sorted.remove(term);
            self.free.push(i);
        }
    }

    fn prefix(&self, prefix: &str) -> Vec<(&str, &PostingList)> {
        self.sorted
            .range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, &i)| (&**k, &self.lists[i as usize]))
            .collect()
    }
}

type TermMap = FxHashMap<String, SmallVec<[u32; 4]>>;

/// Appends `p` to the term's positions, allocating the key only for a term new to this map.
fn push_pos(map: &mut TermMap, term: &str, p: Option<u32>) {
    let list = match map.get_mut(term) {
        Some(list) => list,
        None => map.entry(term.to_owned()).or_default(),
    };
    list.extend(p);
}

fn add_tokens(map: &mut TermMap, raw: &str, hidx: Option<usize>, scratch: &mut Scratch) {
    for_each_token_with(scratch, raw, 0, |text, pos, _, _| {
        let p = match hidx {
            Some(h) => ((h as u32) << 16) | (pos & 0xFFFF),
            None => pos,
        };
        push_pos(map, text, Some(p));
    });
}

/// A doc's postings per field (term -> positions), its body token count (its `word_count`) and
/// the token position at the start of each body line. Deterministic, so `unindex` can
/// re-derive it.
struct Derived {
    maps: Vec<TermMap>,
    words: u32,
    line_pos: Vec<u32>,
}

fn derive(doc: &Doc) -> Derived {
    let mut scratch = Scratch::default();
    let mut maps: Vec<TermMap> = (0..FIELDS.len()).map(|_| TermMap::default()).collect();
    add_tokens(&mut maps[fidx(Field::Title)], &strip_symbols(&doc.title), None, &mut scratch);
    for a in &doc.aliases {
        add_tokens(&mut maps[fidx(Field::Alias)], a, None, &mut scratch);
    }
    for (i, h) in doc.headings.iter().enumerate() {
        let f = match h.level {
            1 => Field::H1,
            2 => Field::H2,
            3 => Field::H3,
            _ => continue,
        };
        add_tokens(&mut maps[fidx(f)], &h.text, Some(i), &mut scratch);
    }
    for (i, t) in doc.tags.iter().enumerate() {
        let n = normalize(t);
        if !n.is_empty() {
            maps[fidx(Field::Tag)].entry(n).or_default().push(i as u32);
        }
    }
    for l in &doc.links {
        add_tokens(&mut maps[fidx(Field::Link)], &l.target, None, &mut scratch);
        if let Some(a) = &l.alias {
            add_tokens(&mut maps[fidx(Field::Link)], a, None, &mut scratch);
        }
    }
    for s in &doc.path_segments {
        add_tokens(&mut maps[fidx(Field::Path)], &strip_symbols(s), None, &mut scratch);
    }
    let body = &mut maps[fidx(Field::Body)];
    let (mut i, mut pos) = (0, 0);
    let mut line_pos = Vec::with_capacity(doc.body_line_src.len());
    // Line by line (same tokens and positions as the whole body), recording each line's start.
    for line in doc.body.split('\n') {
        line_pos.push(pos);
        pos = for_each_token_with(&mut scratch, line, pos, |text, p, _, _| {
            push_pos(body, text, (i < BODY_POSITION_CAP).then_some(p));
            i += 1;
        });
    }
    Derived { maps, words: i as u32, line_pos }
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

    /// Insert or replace a doc by path, as given (its body is kept; no raw text).
    pub fn upsert(&mut self, doc: Doc) -> DocId {
        self.upsert_inner(doc, String::new(), None, None)
    }

    /// Insert or replace a note by path from its raw text: extracts the doc, keeps the raw text
    /// for previews, and rebuilds body lines from it on demand instead of storing the body.
    pub fn upsert_note(&mut self, path: &str, mtime: f64, text: &str) -> DocId {
        let (doc, src) = extract_note(path, text, mtime);
        self.upsert_inner(doc, text.to_owned(), Some(src), None)
    }

    /// `upsert_note`, adding each phase's wall time to `prof` (dev profiling only).
    pub fn upsert_note_profiled(&mut self, path: &str, mtime: f64, text: &str, prof: &mut UpsertProfile) -> DocId {
        let (doc, src) = timed(Some(&mut prof.extract), || extract_note(path, text, mtime));
        self.upsert_inner(doc, text.to_owned(), Some(src), Some(prof))
    }

    /// `doc.word_count` is (re)set from the body tokens counted while indexing. With `src`
    /// (body-line sources into `text`), the body is dropped after indexing.
    fn upsert_inner(
        &mut self,
        mut doc: Doc,
        text: String,
        src: Option<Vec<u32>>,
        mut prof: Option<&mut UpsertProfile>,
    ) -> DocId {
        let id = match self.path_to_id.get(&doc.path).copied() {
            Some(id) => {
                timed(prof.as_deref_mut().map(|p| &mut p.unindex), || self.unindex(id));
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
        let d = timed(prof.as_deref_mut().map(|p| &mut p.derive), || derive(&doc));
        doc.word_count = d.words;
        timed(prof.as_deref_mut().map(|p| &mut p.insert), || self.insert(id, &doc, d.maps));
        if self.records[id as usize].is_none() {
            self.live += 1;
        }
        let keys = timed(prof.map(|p| &mut p.keys), || Keys::of(&doc));
        let body = match src {
            Some(src) => {
                doc.body = String::new();
                doc.body_line_starts = Vec::new();
                BodySrc::Text(src)
            }
            None => BodySrc::Kept,
        };
        self.records[id as usize] = Some(Record { keys, doc, text, body, line_pos: d.line_pos });
        self.clear_strings();
        id
    }

    fn index(&mut self, id: DocId, doc: &Doc) {
        let maps = derive(doc).maps;
        self.insert(id, doc, maps);
    }

    fn insert(&mut self, id: DocId, doc: &Doc, maps: Vec<TermMap>) {
        for (fi, map) in maps.into_iter().enumerate() {
            for (term, positions) in map {
                self.dicts[fi].list_mut(term).insert(id, &positions);
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
        let maps = derive(&rec.full_doc()).maps;
        for (fi, map) in maps.into_iter().enumerate() {
            for term in map.keys() {
                self.dicts[fi].remove_doc(term, id);
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
        let doc = rec.full_doc().into_owned();
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

    /// Cached near-duplicate keys of the doc's title.
    pub fn title_keys(&self, id: DocId) -> Option<&TitleKeys> {
        self.records.get(id as usize)?.as_ref().map(|r| &r.keys.sim)
    }

    /// Each alias normalized, with its non-stopword word spans; cached at upsert/rename.
    pub fn alias_words(&self, id: DocId) -> &[(String, Vec<(u32, u32)>)] {
        self.records.get(id as usize).and_then(Option::as_ref).map_or(&[], |r| &r.keys.aliases)
    }

    /// Body lines around the token at `pos` (a Body posting position of the doc), reaching at
    /// least `margin` bytes before the first and after the last token at `pos`, or the body's
    /// start/end. Costs one line rebuild per line in the window.
    pub fn body_window(&self, id: DocId, pos: u32, margin: usize) -> Option<BodyWindow> {
        let rec = self.records.get(id as usize)?.as_ref()?;
        let n = rec.body_lines();
        let l = rec.line_pos[..n].partition_point(|&p| p <= pos).checked_sub(1)?;
        let line = rec.body_line(l);
        let (mut lo, mut hi) = (usize::MAX, 0);
        for_each_token_at(&line, rec.line_pos[l], |_, p, s, e| {
            if p == pos {
                lo = lo.min(s as usize);
                hi = hi.max(e as usize);
            }
        });
        if lo == usize::MAX {
            return None;
        }
        let (mut before, mut after) = (lo, line.len() - hi);
        let (mut first, mut last) = (l, l);
        let mut lines: VecDeque<Cow<str>> = VecDeque::from([line]);
        while before < margin && first > 0 {
            first -= 1;
            let t = rec.body_line(first);
            before += t.len() + 1;
            lines.push_front(t);
        }
        while after < margin && last + 1 < n {
            last += 1;
            let t = rec.body_line(last);
            after += t.len() + 1;
            lines.push_back(t);
        }
        let text = lines.into_iter().collect::<Vec<_>>().join("\n");
        let mut tokens = Vec::new();
        for_each_token_at(&text, rec.line_pos[first], |t, pos, byte_start, byte_end| {
            tokens.push(Token { text: t.to_owned(), pos, byte_start, byte_end })
        });
        Some(BodyWindow { text, first_line: first, tokens })
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

    pub fn exact(&self, field: Field, token: &str) -> &PostingList {
        self.dicts[fidx(field)].get(token)
    }

    /// All terms starting with `prefix`, in dictionary order. Empty prefix yields nothing.
    pub fn prefix(&self, field: Field, prefix: &str) -> Vec<(&str, &PostingList)> {
        if prefix.is_empty() {
            return Vec::new();
        }
        self.dicts[fidx(field)].prefix(prefix)
    }

    /// Builds every lazy cache now (fuzzy/highlight strings of Title, Alias, H1–H3 and the
    /// backlink counts), so the next query does not pay for them.
    pub fn warm_up(&self) {
        for f in [Field::Title, Field::Alias, Field::H1, Field::H2, Field::H3] {
            self.field_strings(f);
        }
        self.backlinks(0);
    }

    /// True when every lazy cache is built (none cleared since the last `warm_up`).
    pub fn caches_built(&self) -> bool {
        self.strings.iter().all(|c| c.get().is_some()) && self.backlink_cache.get().is_some()
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
        assert_eq!(hits.get(0).unwrap().doc, a);
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
        assert_eq!(ix.title_keys(id).unwrap().spans.len(), 2);
        assert_eq!(ix.backlinks(id), 0);
        assert!(ix.rename("💡 Area/Old Name.md", "Archive/New-Long Name.md"));
        assert_eq!(ix.path_norm(id), Some("archive/new-long name.md"));
        assert_eq!(ix.title_keys(id).unwrap().spans.len(), 3);
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
        let p = ix.exact(Field::Body, "word").get(0).unwrap();
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
        let p = ix.exact(Field::H2, "sub").get(0).unwrap();
        assert_eq!(p.positions, &[1u32 << 16]);
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
    fn posting_list_keeps_positions_with_their_doc() {
        let mut l = PostingList::default();
        l.insert(5, &[1, 2]);
        l.insert(9, &[]);
        l.insert(2, &[7]);
        l.insert(7, &[3, 4, 5]);
        let all: Vec<_> = l.iter().map(|p| (p.doc, p.positions.to_vec())).collect();
        assert_eq!(all, vec![(2, vec![7]), (5, vec![1, 2]), (7, vec![3, 4, 5]), (9, vec![])]);
        l.remove(5);
        l.remove(4);
        let all: Vec<_> = l.iter().map(|p| (p.doc, p.positions.to_vec())).collect();
        assert_eq!(all, vec![(2, vec![7]), (7, vec![3, 4, 5]), (9, vec![])]);
        l.remove(2);
        l.remove(9);
        l.remove(7);
        assert!(l.is_empty() && l.positions.is_empty() && l.ends.is_empty());
    }

    #[test]
    fn word_count_filled_while_indexing() {
        let text = "# H\nOne two-three [[Link|alias]]\n```\ncode here\n```";
        let mut ix = Index::new();
        let id = ix.upsert_note("w.md", 0.0, text);
        assert_eq!(ix.get(id).unwrap().word_count, mk("w.md", text).word_count);
        assert!(ix.get(id).unwrap().word_count > 0);
    }

    #[test]
    fn warm_up_builds_every_lazy_cache() {
        let mut ix = Index::new();
        ix.upsert(mk("n.md", "# Top\n## Sub\n### Deep\nsee [[n]]"));
        assert!(!ix.caches_built());
        ix.warm_up();
        assert!(ix.caches_built());
        ix.upsert(mk("m.md", ""));
        assert!(!ix.caches_built(), "a mutation clears them again");
    }

    #[test]
    fn note_body_survives_rename_and_replace() {
        let mut ix = Index::new();
        let id = ix.upsert_note("a/x.md", 0.0, "---\ntags: [t]\n---\nhello [[World|planet]]\n```\ncode line\n```");
        assert!(ix.rename("a/x.md", "b/y.md"));
        for w in ["hello", "world", "planet", "code"] {
            assert_eq!(ix.exact(Field::Body, w).len(), 1, "{w}");
        }
        ix.upsert_note("b/y.md", 0.0, "fresh");
        assert!(ix.exact(Field::Body, "hello").is_empty());
        assert_eq!(ix.exact(Field::Body, "fresh").get(0).unwrap().doc, id);
        assert!(ix.remove("b/y.md").is_some());
        assert!(ix.exact(Field::Body, "fresh").is_empty());
    }

    #[test]
    fn body_window_matches_the_whole_body() {
        let filler: String = (0..40).map(|i| format!("line {i} with [[Link {i}|alias]] words\n")).collect();
        let text = format!("# Head\n{filler}needle here\n```\nfenced needle2\n```\n{filler}");
        let doc = mk("w.md", &text);
        let mut ix = Index::new();
        let id = ix.upsert_note("w.md", 0.0, &text);
        let kept = ix.upsert(mk("k.md", &text));
        for t in crate::tokenize::tokenize(&doc.body) {
            for (i, margin) in [(id, 64), (kept, 64), (id, 0)] {
                let w = ix.body_window(i, t.pos, margin).unwrap();
                let off = doc.body_line_starts[w.first_line] as usize;
                assert_eq!(&doc.body[off..off + w.text.len()], w.text);
                let at: Vec<_> = w.tokens.iter().filter(|x| x.pos == t.pos).collect();
                assert!(at.iter().any(|x| x.text == t.text && (x.byte_start as usize + off) == t.byte_start as usize));
                let first = at.iter().map(|x| x.byte_start as usize).min().unwrap();
                assert!(first >= margin || w.first_line == 0);
            }
        }
    }

    #[test]
    fn tags_and_text() {
        let mut ix = Index::new();
        let raw = "---\ntags: [a/b]\n---\nx";
        let id = ix.upsert_note("t.md", 0.0, raw);
        assert_eq!(ix.docs_with_tag("a"), vec![id]);
        assert_eq!(ix.docs_with_tag("a/b"), vec![id]);
        assert!(ix.docs_with_tag("b2").is_empty());
        assert_eq!(ix.text(id), Some(raw));
        assert!(ix.get(id).unwrap().body.is_empty(), "body not stored next to the raw text");
    }
}
