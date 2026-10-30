//! Golden query runner (feature `golden`): loads a vault directory into an [`Engine`] and checks
//! the cases of a golden TOML file (spec §12). Used by `tests/golden.rs` and the CLI.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use walkdir::WalkDir;

use crate::engine::{Engine, Ghost, Hit, Mode, SearchOptions};
use crate::markdown::extract;
use crate::normalize::normalize;

/// Hits shown in a failure and checked by `expect_in_top`.
const TOP_N: usize = 5;

#[derive(Deserialize)]
struct GoldenFile {
    #[serde(default, rename = "case")]
    cases: Vec<Case>,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum CaseMode {
    #[default]
    All,
    Files,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    query: String,
    #[serde(default)]
    mode: CaseMode,
    expect_top: Option<String>,
    #[serde(default)]
    expect_in_top: Vec<String>,
    #[serde(default)]
    expect_in_top3: Vec<String>,
    /// Tier every hit named in `expect_top` / `expect_in_top` / `expect_in_top3` must have.
    expect_tier: Option<u8>,
    #[serde(default)]
    must_not_top3: Vec<String>,
    /// Names that must not be anywhere in the returned page.
    #[serde(default)]
    must_not_appear: Vec<String>,
    #[serde(default)]
    expect_similar_count: HashMap<String, u32>,
}

/// One failed expectation of one case, with the top hits the engine returned.
#[derive(Debug, Clone)]
pub struct Failure {
    pub query: String,
    pub expected: String,
    /// `(title, tier)` of the first 5 hits.
    pub got: Vec<(String, u8)>,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let got: Vec<String> = self.got.iter().map(|(t, tier)| format!("{t} (t{tier})")).collect();
        write!(f, "{} → expected {}, got [{}]", self.query, self.expected, got.join(", "))
    }
}

/// Indexes every `.md` file under `vault_dir` (dot-folders skipped; paths relative with `/`;
/// mtime in seconds) and sets ghosts for wikilink targets that resolve to no note.
pub fn load_vault(vault_dir: &Path) -> std::io::Result<Engine> {
    load_vault_with(vault_dir, &|_| false).map(|(engine, _)| engine)
}

/// What a vault load indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultStats {
    pub docs: usize,
    /// Total size of the indexed note texts in bytes.
    pub bytes: u64,
}

/// Like [`load_vault`], skipping every vault-relative path (`/`-separated) for which `exclude`
/// returns true. Also returns the indexed document and byte counts.
pub fn load_vault_with(vault_dir: &Path, exclude: &dyn Fn(&str) -> bool) -> std::io::Result<(Engine, VaultStats)> {
    let mut engine = Engine::new();
    let notes = read_vault_with(vault_dir, exclude)?;
    let mut bytes = 0u64;
    for n in &notes {
        bytes += n.text.len() as u64;
        engine.upsert(&n.path, n.mtime, &n.text);
    }
    engine.set_ghosts(ghosts(&notes));
    let stats = VaultStats { docs: notes.len(), bytes };
    Ok((engine, stats))
}

/// One markdown file read from a vault directory.
pub struct VaultNote {
    /// Vault-relative, `/`-separated.
    pub path: String,
    pub mtime: f64,
    pub text: String,
}

/// Reads every non-excluded `.md` file under `vault_dir` (dot-folders skipped), sorted by path.
pub fn read_vault_with(vault_dir: &Path, exclude: &dyn Fn(&str) -> bool) -> std::io::Result<Vec<VaultNote>> {
    let mut notes = Vec::new();
    let walker = WalkDir::new(vault_dir).sort_by_file_name().into_iter();
    for entry in walker.filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.')) {
        let entry = entry?;
        let is_note = entry.path().extension().is_some_and(|x| x == "md" || x == "base");
        if !entry.file_type().is_file() || !is_note {
            continue;
        }
        let rel = entry.path().strip_prefix(vault_dir).expect("walkdir yields paths under its root");
        let path = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
        if exclude(&path) {
            continue;
        }
        let mtime = entry
            .metadata()?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let text = std::fs::read_to_string(entry.path())?;
        notes.push(VaultNote { path, mtime, text });
    }
    Ok(notes)
}

/// Unresolved wikilink targets, with the number of distinct notes linking to each.
pub fn ghosts(notes: &[VaultNote]) -> Vec<Ghost> {
    let mut known: HashSet<String> = HashSet::new();
    for VaultNote { path, .. } in notes {
        let stem = path.strip_suffix(".md").unwrap_or(path);
        known.insert(normalize(stem));
        known.insert(normalize(stem.rsplit('/').next().unwrap_or(stem)));
    }
    let resolves = |target: &str| {
        let t = target.strip_suffix(".md").unwrap_or(target);
        known.contains(&normalize(t)) || known.contains(&normalize(t.rsplit('/').next().unwrap_or(t)))
    };
    // target -> linking note paths, in first-seen order.
    let mut order: Vec<String> = Vec::new();
    let mut refs: HashMap<String, HashSet<&str>> = HashMap::new();
    for VaultNote { path, text, .. } in notes {
        for link in extract(path, text, 0.0).links {
            let target = link.target.trim();
            if target.is_empty() || resolves(target) {
                continue;
            }
            if !refs.contains_key(target) {
                order.push(target.to_string());
            }
            refs.entry(target.to_string()).or_default().insert(path);
        }
    }
    order.into_iter().map(|t| Ghost { refs: refs[&t].len() as u32, target: t }).collect()
}

fn matches(h: &Hit, name: &str) -> bool {
    h.title == name || h.path.ends_with(name)
}

/// Whether some indexed note answers to `name` (title or path suffix), so a negative check
/// cannot pass because of a misspelled name.
fn indexed(engine: &Engine, name: &str) -> bool {
    engine.docs().any(|d| d.title == name || d.path.ends_with(name))
}

/// Runs every case of `golden` against the vault at `vault_dir`.
///
/// Panics when the vault or the golden file cannot be read or parsed (it is a test harness).
pub fn run_golden(vault_dir: &Path, golden: &Path) -> Vec<Failure> {
    run_golden_with(vault_dir, golden, &|_| false)
}

/// Like [`run_golden`], loading the vault through [`load_vault_with`] with `exclude`.
pub fn run_golden_with(vault_dir: &Path, golden: &Path, exclude: &dyn Fn(&str) -> bool) -> Vec<Failure> {
    let engine = load_vault_with(vault_dir, exclude).map(|(e, _)| e).unwrap_or_else(|e| panic!("cannot load vault {}: {e}", vault_dir.display()));
    let text = std::fs::read_to_string(golden).unwrap_or_else(|e| panic!("cannot read {}: {e}", golden.display()));
    let file: GoldenFile = toml::from_str(&text).unwrap_or_else(|e| panic!("cannot parse {}: {e}", golden.display()));
    file.cases.iter().flat_map(|c| run_case(&engine, c)).collect()
}

fn run_case(engine: &Engine, case: &Case) -> Vec<Failure> {
    let mode = match case.mode {
        CaseMode::All => Mode::All,
        CaseMode::Files => Mode::FilesOnly,
    };
    let opts = SearchOptions { mode, ..SearchOptions::default() };
    let hits = engine.search(&case.query, &opts).hits;
    let got: Vec<(String, u8)> = hits.iter().take(TOP_N).map(|h| (h.title.clone(), h.tier)).collect();
    let fail = |expected: String| Failure { query: case.query.clone(), expected, got: got.clone() };
    let mut out = Vec::new();
    if let Some(top) = &case.expect_top {
        if !hits.first().is_some_and(|h| matches(h, top)) {
            out.push(fail(format!("top = {top:?}")));
        }
    }
    for name in &case.expect_in_top {
        if !hits.iter().take(TOP_N).any(|h| matches(h, name)) {
            out.push(fail(format!("{name:?} in top {TOP_N}")));
        }
    }
    for name in &case.expect_in_top3 {
        if !hits.iter().take(3).any(|h| matches(h, name)) {
            out.push(fail(format!("{name:?} in top 3")));
        }
    }
    if let Some(tier) = case.expect_tier {
        for name in case.expect_top.iter().chain(&case.expect_in_top).chain(&case.expect_in_top3) {
            match hits.iter().find(|h| matches(h, name)) {
                Some(h) if h.tier == tier => {}
                Some(h) => out.push(fail(format!("tier({name:?}) = {tier}, was {}", h.tier))),
                None => out.push(fail(format!("tier({name:?}) = {tier}, but no such hit"))),
            }
        }
    }
    for name in case.must_not_top3.iter().chain(&case.must_not_appear) {
        if !indexed(engine, name) {
            out.push(fail(format!("{name:?} to exist, but no indexed note has that title or path")));
        }
    }
    for name in &case.must_not_top3 {
        if hits.iter().take(3).any(|h| matches(h, name)) {
            out.push(fail(format!("{name:?} not in top 3")));
        }
    }
    for name in &case.must_not_appear {
        if hits.iter().any(|h| matches(h, name)) {
            out.push(fail(format!("{name:?} not in the page")));
        }
    }
    let mut similar: Vec<(&String, &u32)> = case.expect_similar_count.iter().collect();
    similar.sort();
    for (name, &want) in similar {
        match hits.iter().find(|h| matches(h, name)) {
            Some(h) if h.similar_count == want => {}
            Some(h) => out.push(fail(format!("similar_count({name:?}) = {want}, was {}", h.similar_count))),
            None => out.push(fail(format!("similar_count({name:?}) = {want}, but no such hit"))),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_vault_with_reports_docs_and_bytes() {
        let dir = std::env::temp_dir().join(format!("ff10k-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("skip")).unwrap();
        std::fs::write(dir.join("a.md"), "12345").unwrap();
        std::fs::write(dir.join("skip/b.md"), "xx").unwrap();
        std::fs::write(dir.join("c.txt"), "zzz").unwrap();
        let (engine, stats) = load_vault_with(&dir, &|p| p.starts_with("skip/")).unwrap();
        assert_eq!(engine.doc_count(), 1);
        assert_eq!(stats, VaultStats { docs: 1, bytes: 5 });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tier_and_top3_keys() {
        let dir = std::env::temp_dir().join(format!("ff10k-tier-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Airbyte State.md"), "x").unwrap();
        std::fs::write(dir.join("Other.md"), "airbyte state").unwrap();
        let golden = dir.join("g.toml");
        std::fs::write(
            &golden,
            r#"
[[case]]
query = "airbyte state"
expect_top = "Airbyte State"
expect_in_top3 = ["Other"]
expect_tier = 1

[[case]]
query = "airbyte state"
expect_in_top3 = ["Missing"]
"#,
        )
        .unwrap();
        let failures = run_golden(&dir, &golden);
        let expected: Vec<&str> = failures.iter().map(|f| f.expected.as_str()).collect();
        assert_eq!(expected, ["tier(\"Other\") = 1, was 3", "\"Missing\" in top 3"], "{expected:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn negative_keys_fail_on_a_name_no_note_has() {
        let dir = std::env::temp_dir().join(format!("ff10k-neg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Airbyte Checkpointing.md"), "state").unwrap();
        std::fs::write(dir.join("Other.md"), "airbyte").unwrap();
        let golden = dir.join("g.toml");
        std::fs::write(
            &golden,
            r#"
[[case]]
query = "airbyte"
must_not_appear = ["Airbyte Checkpointng"]
must_not_top3 = ["Airbyte Checkpointng"]

[[case]]
query = "state"
must_not_appear = ["Other"]
must_not_top3 = ["Other.md"]
"#,
        )
        .unwrap();
        let failures = run_golden(&dir, &golden);
        let expected: Vec<&str> = failures.iter().map(|f| f.expected.as_str()).collect();
        assert_eq!(expected.len(), 2, "{expected:?}");
        assert!(expected.iter().all(|e| e.contains("no indexed note")), "{expected:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
