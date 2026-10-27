//! Typo recall over the fixture vault: one typo in one title word must not cost the note its
//! tier-1 place. For every title with ≥ 2 words and every word of ≥ 6 chars, the query
//! `<misspelled word> <other title word>` must return the note in tier 1, for one
//! transposition, one deletion and one substitution (each in the middle of the word).
//! Deletions only apply to words of ≥ 7 chars: a shorter result is below the 6-char fuzzy
//! minimum (spec §16) and is not a fuzzy query at all.
//! Run: `cargo test -p ff10k-core --features golden --test typo_recall -- --nocapture`.

use std::path::PathBuf;

use ff10k_core::golden::{load_vault, read_vault_with};
use ff10k_core::markdown::extract;
use ff10k_core::SearchOptions;

/// Allowed misses (`query → title`), with the reason. Keep empty unless a miss is legitimate.
const ALLOWED_MISSES: &[(&str, &str)] = &[];

/// The engine's default optional terms: never a useful "other" word.
const OPTIONAL: [&str; 8] = ["a", "an", "the", "of", "to", "in", "on", "i"];

fn words(title: &str) -> Vec<String> {
    title.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Transposition, deletion and substitution of `w` around its middle char.
fn misspellings(w: &str) -> Vec<(&'static str, String)> {
    let c: Vec<char> = w.chars().collect();
    let mid = c.len() / 2;
    let mut out = Vec::new();
    // Transposition: the first differing neighbour pair from the middle on.
    if let Some(i) = (mid..c.len() - 1).chain(0..mid).find(|&i| c[i] != c[i + 1]) {
        let mut t = c.clone();
        t.swap(i, i + 1);
        out.push(("transposition", t.into_iter().collect()));
    }
    if c.len() >= 7 {
        let mut d = c.clone();
        d.remove(mid);
        out.push(("deletion", d.into_iter().collect()));
    }
    let mut s = c.clone();
    s[mid] = if s[mid] == 'z' { 'q' } else { 'z' };
    out.push(("substitution", s.into_iter().collect()));
    out
}

#[test]
fn one_typo_keeps_tier_one() {
    let vault = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vault");
    let engine = load_vault(&vault).unwrap();
    let notes = read_vault_with(&vault, &|_| false).unwrap();
    let opts = SearchOptions { limit: 1000, ..SearchOptions::default() };
    let (mut checked, mut misses) = (0, Vec::new());
    for n in &notes {
        let title = extract(&n.path, &n.text, 0.0).title;
        let ws = words(&title);
        if ws.len() < 2 {
            continue;
        }
        for (i, w) in ws.iter().enumerate() {
            if w.chars().count() < 6 {
                continue;
            }
            let Some(other) = ws.iter().enumerate().find(|&(j, o)| j != i && o != w && !OPTIONAL.contains(&o.as_str())) else {
                continue;
            };
            for (kind, typo) in misspellings(w) {
                let query = format!("{typo} {}", other.1);
                checked += 1;
                let hits = engine.search(&query, &opts).hits;
                let hit = hits.iter().find(|h| h.path == n.path);
                if hit.is_some_and(|h| h.tier == 1) || ALLOWED_MISSES.contains(&(query.as_str(), title.as_str())) {
                    continue;
                }
                let got = hit.map_or("absent".to_string(), |h| format!("tier {}", h.tier));
                misses.push(format!("{query:?} ({kind} of {w:?}) → {title:?}: {got}"));
            }
        }
    }
    println!("typo recall: {checked} queries, {} misses", misses.len());
    for m in &misses {
        println!("  {m}");
    }
    assert!(checked > 50, "only {checked} queries checked");
    assert!(misses.is_empty(), "{} miss(es):\n{}", misses.len(), misses.join("\n"));
}
