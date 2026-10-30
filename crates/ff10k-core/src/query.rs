use serde::{Deserialize, Serialize};

use crate::normalize::{is_whitespace_like, normalize};
use crate::tokenize::tokenize_query_term;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Term {
    pub raw: String,
    pub norm: String,
    pub parts: Vec<String>,
    pub has_upper: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    pub terms: Vec<Term>,
    pub phrases: Vec<Vec<String>>,
    pub excludes: Vec<String>,
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    /// `[key]` / `[key:value]` property filters, normalized (Obsidian's search syntax).
    pub props: Vec<(String, Option<String>)>,
    /// `type:note|base|canvas|drawing` file-kind filters (any listed kind passes).
    pub types: Vec<FileKind>,
    pub normalized: String,
}

/// What kind of file a result is, by extension: the `type:` filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Note,
    Base,
    Canvas,
    Drawing,
}

impl FileKind {
    /// `type:` values: `note`/`notes`/`md`, `base`/`bases`, `canvas`/`canvases`,
    /// `drawing`/`drawings`/`excalidraw`; case-insensitive. None for anything else.
    pub fn parse(s: &str) -> Option<FileKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "note" | "notes" | "md" | "markdown" => Some(FileKind::Note),
            "base" | "bases" => Some(FileKind::Base),
            "canvas" | "canvases" => Some(FileKind::Canvas),
            "drawing" | "drawings" | "excalidraw" => Some(FileKind::Drawing),
            _ => None,
        }
    }

    /// The kind of a vault path (`.excalidraw.md` counts as a drawing, not a note).
    pub fn of_path(path: &str) -> FileKind {
        let p = path.to_ascii_lowercase();
        if p.ends_with(".base") {
            FileKind::Base
        } else if p.ends_with(".canvas") {
            FileKind::Canvas
        } else if p.ends_with(".excalidraw") || p.ends_with(".excalidraw.md") {
            FileKind::Drawing
        } else {
            FileKind::Note
        }
    }
}

/// Full normalized compound of a word, or None when it has no letter/digit.
fn compound(word: &str) -> Option<String> {
    tokenize_query_term(word).into_iter().next()
}

impl Query {
    pub fn parse(input: &str) -> Query {
        let mut q = Query {
            terms: Vec::new(),
            phrases: Vec::new(),
            excludes: Vec::new(),
            tags: Vec::new(),
            paths: Vec::new(),
            props: Vec::new(),
            types: Vec::new(),
            normalized: String::new(),
        };
        let mut rest = input;
        loop {
            rest = rest.trim_start_matches(is_whitespace_like);
            if rest.is_empty() {
                break;
            }
            if let Some(after) = rest.strip_prefix('"') {
                if let Some(end) = after.find('"') {
                    let phrase: Vec<String> =
                        after[..end].split(is_whitespace_like).filter_map(compound).collect();
                    if !phrase.is_empty() {
                        q.phrases.push(phrase);
                    }
                    rest = &after[end + 1..];
                } else {
                    // Unterminated quote: ignore the quote character.
                    rest = after;
                }
                continue;
            }
            let end = rest.find(is_whitespace_like).unwrap_or(rest.len());
            let (word, tail) = rest.split_at(end);
            rest = tail;
            q.push_word(word);
        }
        q.normalized = q.terms.iter().map(|t| t.norm.as_str()).collect::<Vec<_>>().join(" ");
        q
    }

    fn push_word(&mut self, word: &str) {
        if let Some(r) = word.strip_prefix('-') {
            let n = normalize(r);
            if !n.is_empty() {
                self.excludes.push(n);
            }
        } else if let Some(r) = word.strip_prefix('#') {
            let n = normalize(r);
            if !n.is_empty() {
                self.tags.push(n);
            }
        } else if word.len() > 5 && word.is_char_boundary(5) && word[..5].eq_ignore_ascii_case("type:") && FileKind::parse(&word[5..]).is_some() {
            self.types.push(FileKind::parse(&word[5..]).unwrap());
        } else if word.len() > 2 && word.starts_with('[') && word.ends_with(']') {
            let inner = &word[1..word.len() - 1];
            let (k, v) = match inner.split_once(':') {
                Some((k, v)) => (normalize(k), Some(normalize(v))),
                None => (normalize(inner), None),
            };
            if !k.is_empty() {
                self.props.push((k, v.filter(|v| !v.is_empty())));
            }
        } else if word.len() >= 5 && word.is_char_boundary(5) && word[..5].eq_ignore_ascii_case("path:") {
            let n = normalize(&word[5..]);
            if !n.is_empty() {
                self.paths.push(n);
            }
        } else {
            let parts = tokenize_query_term(word);
            if !parts.is_empty() {
                self.terms.push(Term {
                    raw: word.to_string(),
                    norm: normalize(word),
                    parts,
                    has_upper: word.chars().any(char::is_uppercase),
                });
            }
        }
    }

    /// Appends an already-normalized word as a plain term.
    fn push_norm_word(&mut self, word: &str) {
        let parts = tokenize_query_term(word);
        if !parts.is_empty() {
            self.terms.push(Term { raw: word.to_string(), norm: normalize(word), parts, has_upper: false });
        }
        self.normalized = self.terms.iter().map(|t| t.norm.as_str()).collect::<Vec<_>>().join(" ");
    }

    /// A copy with every tag for which `is_tag` is false searched as a word instead (`#115` in
    /// a title is not a tag filter when no note carries that tag). None when nothing changes.
    pub fn with_tags_as_words(&self, is_tag: impl Fn(&str) -> bool) -> Option<Query> {
        if self.tags.iter().all(|t| is_tag(t)) {
            return None;
        }
        let mut q = self.clone();
        q.tags.clear();
        for t in &self.tags {
            if is_tag(t) {
                q.tags.push(t.clone());
            } else {
                q.push_norm_word(t);
            }
        }
        Some(q)
    }

    /// A copy with the exclusions searched as words (`KW13 -Marktleistung` when excluding
    /// leaves nothing). None when there are no exclusions or nothing else to search (`-note`
    /// alone means "everything without note", never "note").
    pub fn with_excludes_as_words(&self) -> Option<Query> {
        if self.excludes.is_empty() || (self.terms.is_empty() && self.phrases.is_empty()) {
            return None;
        }
        let mut q = self.clone();
        q.excludes.clear();
        for e in &self.excludes {
            q.push_norm_word(e);
        }
        Some(q)
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.phrases.is_empty() && !self.has_filters()
    }

    pub fn has_filters(&self) -> bool {
        !(self.excludes.is_empty() && self.tags.is_empty() && self.paths.is_empty() && self.props.is_empty() && self.types.is_empty())
    }

    /// A copy with every property filter whose key `is_key` rejects searched as words instead
    /// (`[foo:bar]` with no note carrying `foo`). None when nothing changes.
    pub fn with_props_as_words(&self, is_key: impl Fn(&str) -> bool) -> Option<Query> {
        if self.props.iter().all(|(k, _)| is_key(k)) {
            return None;
        }
        let mut q = self.clone();
        q.props.clear();
        for (k, v) in &self.props {
            if is_key(k) {
                q.props.push((k.clone(), v.clone()));
            } else {
                q.push_norm_word(k);
                if let Some(v) = v {
                    q.push_norm_word(v);
                }
            }
        }
        Some(q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terms() {
        let q = Query::parse("Obsidian trouble");
        let norms: Vec<_> = q.terms.iter().map(|t| t.norm.as_str()).collect();
        assert_eq!(norms, ["obsidian", "trouble"]);
        assert!(q.terms[0].has_upper);
        assert!(!q.terms[1].has_upper);
        assert_eq!(q.terms[0].raw, "Obsidian");
        assert_eq!(q.normalized, "obsidian trouble");
    }

    #[test]
    fn phrase_exclude_tag_path() {
        let q = Query::parse("\"data mesh\" -draft #ai path:Resources");
        assert_eq!(q.phrases, vec![vec!["data".to_string(), "mesh".to_string()]]);
        assert_eq!(q.excludes, ["draft"]);
        assert_eq!(q.tags, ["ai"]);
        assert_eq!(q.paths, ["resources"]);
        assert!(q.terms.is_empty());
        assert_eq!(q.normalized, "");
        assert!(q.has_filters());
        assert!(!q.is_empty());
    }

    #[test]
    fn compound_term_parts() {
        let q = Query::parse("data-engineering");
        assert_eq!(q.terms.len(), 1);
        assert_eq!(q.terms[0].parts, ["data-engineering", "data", "engineering"]);
    }

    #[test]
    fn type_filters() {
        let q = Query::parse("type:base coffee Type:Drawings type:unknown");
        assert_eq!(q.types, vec![FileKind::Base, FileKind::Drawing]);
        let norms: Vec<_> = q.terms.iter().map(|t| t.norm.as_str()).collect();
        assert_eq!(norms, ["coffee", "type:unknown"]);
        assert!(q.has_filters());
        assert_eq!(FileKind::of_path("a/B.Canvas"), FileKind::Canvas);
        assert_eq!(FileKind::of_path("a/venn.excalidraw.md"), FileKind::Drawing);
        assert_eq!(FileKind::of_path("a/note.md"), FileKind::Note);
        assert!(Query::parse("type:").terms.len() == 1);
    }

    #[test]
    fn property_filters() {
        let q = Query::parse("[Herkunftsland:India] [price] espresso []");
        assert_eq!(q.props, vec![("herkunftsland".to_string(), Some("india".to_string())), ("price".to_string(), None)]);
        let norms: Vec<_> = q.terms.iter().map(|t| t.norm.as_str()).collect();
        assert_eq!(norms, ["espresso"]);
        assert!(q.has_filters());
        let w = q.with_props_as_words(|k| k == "price").unwrap();
        assert_eq!(w.props, vec![("price".to_string(), None)]);
        let norms: Vec<_> = w.terms.iter().map(|t| t.norm.as_str()).collect();
        assert_eq!(norms, ["espresso", "herkunftsland", "india"]);
        assert!(q.with_props_as_words(|_| true).is_none());
    }

    #[test]
    fn syntax_only_is_empty() {
        for s in ["#", "path:", "\"\"", "-", "   "] {
            assert!(Query::parse(s).is_empty(), "{s:?}");
        }
    }

    #[test]
    fn unterminated_quote_is_terms() {
        let q = Query::parse("\"data mesh");
        let norms: Vec<_> = q.terms.iter().map(|t| t.norm.as_str()).collect();
        assert_eq!(norms, ["data", "mesh"]);
        assert!(q.phrases.is_empty());
    }

    #[test]
    fn nested_tag_and_case_insensitive_path() {
        let q = Query::parse("#Area/Work PATH:Notes/Ünï");
        assert_eq!(q.tags, ["area/work"]);
        assert_eq!(q.paths, ["notes/uni"]);
    }

    #[test]
    fn odd_inputs_do_not_panic() {
        for s in ["\"\"\"\"", "path:", "#", "--x", "-#tag", "\"", "pa", "päth:x", "\u{3000}-\u{3000}"] {
            let _ = Query::parse(s);
        }
        assert_eq!(Query::parse("--x").excludes, ["-x"]);
    }
}
