//! Golden queries (tests/golden/*.toml) against the synthetic fixture vault.
//! Run: `cargo test -p ff10k-core --features golden --test golden`.

use std::path::PathBuf;

use ff10k_core::golden::run_golden;

#[test]
fn golden_core() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let failures = run_golden(&repo.join("fixtures/vault"), &repo.join("tests/golden/core.toml"));
    let report: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert!(failures.is_empty(), "{} golden failure(s):\n{}", failures.len(), report.join("\n"));
}
