//! Tokenizer: splits raw text into normalized tokens with word positions and raw byte offsets.

use unicode_general_category::{get_general_category, GeneralCategory};

use crate::normalize::normalize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub pos: u32,
    pub byte_start: u32,
    pub byte_end: u32,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
        || c == '-'
        || c == '_'
        || matches!(
            get_general_category(c),
            GeneralCategory::NonspacingMark | GeneralCategory::SpacingMark
        )
}

/// Byte ranges of maximal runs of word characters, with leading/trailing joiners trimmed.
fn words(raw: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let push = |s: usize, e: usize, out: &mut Vec<(usize, usize)>| {
        let w = &raw[s..e];
        let t = w.trim_start_matches(['-', '_']);
        let s2 = s + (w.len() - t.len());
        let t = t.trim_end_matches(['-', '_']);
        if !t.is_empty() {
            out.push((s2, s2 + t.len()));
        }
    };
    for (i, c) in raw.char_indices() {
        if is_word_char(c) {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            push(s, i, &mut out);
        }
    }
    if let Some(s) = start {
        push(s, raw.len(), &mut out);
    }
    out
}

/// Split a separator-free piece before an uppercase letter following a lowercase letter (digits do not split: `10K`).
fn camel_pieces(raw: &str, base: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<char> = None;
    for (i, c) in raw.char_indices() {
        if let Some(p) = prev {
            if c.is_uppercase() && p.is_lowercase() {
                out.push((base + start, base + i));
                start = i;
            }
        }
        prev = Some(c);
    }
    out.push((base + start, base + raw.len()));
    out
}

fn has_alnum(s: &str) -> bool {
    s.chars().any(char::is_alphanumeric)
}

pub fn tokenize(raw: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut pos: u32 = 0;
    for (ws, we) in words(raw) {
        let word = &raw[ws..we];
        // Leaves: hyphen/underscore parts, each further split at camelCase boundaries.
        let mut leaves: Vec<(usize, usize)> = Vec::new();
        let mut off = 0;
        for part in word.split(['-', '_']) {
            if !part.is_empty() {
                leaves.extend(camel_pieces(part, ws + off));
            }
            off += part.len() + 1;
        }
        let make = |s: usize, e: usize, pos: u32| {
            let text = normalize(&raw[s..e]);
            has_alnum(&text).then(|| Token {
                text,
                pos,
                byte_start: s as u32,
                byte_end: e as u32,
            })
        };
        if leaves.len() > 1 {
            tokens.extend(make(ws, we, pos));
        }
        for (k, &(s, e)) in leaves.iter().enumerate() {
            tokens.extend(make(s, e, pos + k as u32));
        }
        pos += leaves.len().max(1) as u32;
    }
    tokens
}

pub fn tokenize_query_term(term: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tokenize(term) {
        if !out.contains(&t.text) {
            out.push(t.text);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(raw: &str) -> Vec<String> {
        tokenize(raw).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn splits_on_non_alnum() {
        let t = tokenize("Obsidian: Trouble-shoot!");
        let texts: Vec<_> = t.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["obsidian", "trouble-shoot", "trouble", "shoot"]);
        let pos: Vec<_> = t[1..].iter().map(|t| t.pos).collect();
        assert_eq!(pos, [1, 1, 2]);
    }

    #[test]
    fn splits_camel_case() {
        assert_eq!(texts("DataEngineering"), ["dataengineering", "data", "engineering"]);
        let t = tokenize("DataEngineering");
        assert_eq!(t.iter().map(|t| t.pos).collect::<Vec<_>>(), [0, 0, 1]);
    }

    #[test]
    fn keeps_digits() {
        assert_eq!(texts("Fast Find 10K"), ["fast", "find", "10k"]);
    }

    #[test]
    fn byte_offsets_point_into_raw() {
        let raw = "Ölig  Ai";
        let last = tokenize(raw).pop().unwrap();
        assert_eq!(&raw[last.byte_start as usize..last.byte_end as usize], "Ai");
    }

    #[test]
    fn wikilink_text_is_tokenized_without_brackets() {
        assert_eq!(texts("[[Data Mesh|mesh]]"), ["data", "mesh", "mesh"]);
    }

    #[test]
    fn query_term_dedups() {
        assert_eq!(
            tokenize_query_term("Data-Engineering"),
            ["data-engineering", "data", "engineering"]
        );
    }

    #[test]
    fn decomposed_accent_matches_precomposed() {
        assert_eq!(texts("Spa\u{308}ti"), ["spati"]);
        assert_eq!(texts("Spa\u{308}ti"), texts("Sp\u{e4}ti"));
    }

    #[test]
    fn decomposed_kana_stays_one_token_with_marks_in_range() {
        let raw = "\u{30CF}\u{3099}";
        let t = tokenize(raw);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].text, normalize(raw));
        assert_eq!((t[0].byte_start as usize, t[0].byte_end as usize), (0, raw.len()));
    }
}
