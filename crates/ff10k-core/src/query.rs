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
    pub normalized: String,
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
        !(self.excludes.is_empty() && self.tags.is_empty() && self.paths.is_empty())
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
