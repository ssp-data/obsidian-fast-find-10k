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

/// NFKC, lowercase, diacritic folding, whitespace collapse, trim.
pub fn normalize(s: &str) -> String {
    let lowered: String = s.nfkc().flat_map(char::to_lowercase).collect();
    let folded: String = lowered
        .nfd()
        .filter(|&c| {
            c == '\u{3099}'
                || c == '\u{309A}'
                || get_general_category(c) != GeneralCategory::NonspacingMark
        })
        .nfc()
        .collect();
    let mut out = String::with_capacity(folded.len());
    let mut pending_space = false;
    for c in folded.chars() {
        if is_whitespace_like(c) || c.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        }
    }
    out
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
    fn keeps_japanese_marks() {
        assert_eq!(normalize("パーティー"), "パーティー");
    }
}
