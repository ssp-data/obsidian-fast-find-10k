use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_normalization::UnicodeNormalization;

/// ASCII whitespace, NBSP, U+2000-U+200B, narrow NBSP, medium math space,
/// ideographic space and BOM.
pub fn is_whitespace_like(c: char) -> bool {
    c.is_ascii_whitespace()
        || matches!(
            c,
            '\u{00A0}' | '\u{2000}'..='\u{200B}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
        )
}

/// Lowercase letters with no Unicode decomposition that other keyboards type as ASCII:
/// `ß`→`ss`, `ø`→`o`, `æ`→`ae`, `œ`→`oe`, `ł`→`l`, `đ`→`d`, `ı`→`i`, `ð`→`d`, `þ`→`th`
/// (capitals are lowercased first: `ẞ`, `Ø`, `Æ`, `Œ`, `Ł`, `Đ`, `Ð`, `Þ`). Applied to documents
/// and queries alike, so `straße` and `strasse` meet at `strasse`.
fn fold_letter(c: char) -> Option<&'static str> {
    Some(match c {
        'ß' => "ss",
        'ø' => "o",
        'æ' => "ae",
        'œ' => "oe",
        'ł' => "l",
        'đ' => "d",
        'ı' => "i",
        'ð' => "d",
        'þ' => "th",
        _ => return None,
    })
}

/// Chars of `inner` with `fold_letter` applied (expansions are spliced in).
struct FoldLetters<I: Iterator<Item = char>> {
    inner: I,
    pending: std::str::Chars<'static>,
}

impl<I: Iterator<Item = char>> Iterator for FoldLetters<I> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        if let Some(c) = self.pending.next() {
            return Some(c);
        }
        let c = self.inner.next()?;
        match fold_letter(c) {
            Some(s) => {
                self.pending = s.chars();
                self.pending.next()
            }
            None => Some(c),
        }
    }
}

/// NFKC, lowercase, diacritic folding, letter table (`fold_letter`), whitespace collapse, trim.
pub fn normalize(s: &str) -> String {
    let mut out = String::new();
    normalize_into(s, &mut out);
    out
}

/// `normalize` writing into a reused buffer (cleared first).
pub fn normalize_into(s: &str, out: &mut String) {
    out.clear();
    if s.is_ascii() {
        // NFKC, NFD and NFC are the identity on ASCII and it has no combining marks: only
        // lowercasing and the whitespace collapse remain.
        out.reserve(s.len());
        push_collapsed(out, s.bytes().map(|b| char::from(b.to_ascii_lowercase())));
        return;
    }
    let lowered: String = s.nfkc().flat_map(char::to_lowercase).collect();
    let folded = lowered.nfd().filter(|&c| {
        c == '\u{3099}' || c == '\u{309A}' || get_general_category(c) != GeneralCategory::NonspacingMark
    });
    out.reserve(lowered.len());
    push_collapsed(out, FoldLetters { inner: folded.nfc(), pending: "".chars() });
}

/// Appends `chars` with whitespace runs collapsed to one space and leading/trailing ones dropped.
fn push_collapsed(out: &mut String, chars: impl Iterator<Item = char>) {
    let start = out.len();
    let mut pending_space = false;
    for c in chars {
        if is_whitespace_like(c) || c.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && out.len() > start {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        }
    }
}

/// Removes emoji, pictographs, dingbats, variation selector-16, ZWJ and skin tones.
pub fn strip_symbols(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            !matches!(c, '\u{FE0F}' | '\u{200D}' | '\u{1F3FB}'..='\u{1F3FF}')
                && !matches!(
                    get_general_category(c),
                    GeneralCategory::OtherSymbol | GeneralCategory::ModifierSymbol
                )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfkc_and_lowercase() {
        assert_eq!(normalize("Ｄata　Engineering"), "data engineering");
    }

    #[test]
    fn folds_diacritics() {
        assert_eq!(normalize("Späti café"), "spati cafe");
    }

    #[test]
    fn folds_letters_without_a_decomposition() {
        // No NFD mapping exists for these, so they are folded by the explicit table.
        assert_eq!(
            normalize("Straße ẞ Søren Æbleskiver Œuvre Łódź Đình ıstanbul Þór ð"),
            "strasse ss soren aebleskiver oeuvre lodz dinh istanbul thor d"
        );
        assert_eq!(normalize("STRASSE"), "strasse");
    }

    #[test]
    fn collapses_hidden_whitespace() {
        assert_eq!(normalize("AI\u{00A0}\u{200B}Notes "), "ai notes");
    }

    #[test]
    fn strips_emoji_from_titles() {
        let s = strip_symbols("💡 Resources/👉🏻 Data Engineering");
        assert_eq!(s, " Resources/ Data Engineering");
        assert_eq!(normalize(&s), "resources/ data engineering");
    }

    #[test]
    fn ascii_fast_path_matches_unicode_path() {
        // Same input with a trailing non-ASCII space forces the general path.
        for s in ["  Data\tEngineering\x0b\x0cAI  ", "MiXeD-Case_42", "", " \n "] {
            let general = normalize(&format!("{s}\u{3000}"));
            assert_eq!(normalize(s), general, "{s:?}");
        }
    }

    #[test]
    fn keeps_japanese_marks() {
        assert_eq!(normalize("パーティー"), "パーティー");
    }
}
