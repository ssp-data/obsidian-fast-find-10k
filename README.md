# Fast Find 10K

A fast note finder for large vaults (10k+ notes). A Rust search engine compiled to WebAssembly runs in a worker and powers instant fuzzy, field-weighted, frecency-ranked search with optional semantic "similar notes".

Spec: [docs/superpowers/specs/2026-10-26-fast-find-10k-design.md](docs/superpowers/specs/2026-10-26-fast-find-10k-design.md)

## Development

- Build: `cargo build --release` (CLI binary: `target/release/ff10k`)
- Test: `cargo test --workspace`
- Golden: `cargo test -p ff10k-core --features golden --test golden`, or `ff10k golden <vault> <golden.toml>` (exit 1 on failures)
- Bench: `ff10k bench <vault>` (p50/p95 in microseconds, 200 runs per query)

### CLI (`ff10k`)

```sh
ff10k index   <vault>                                   # docs, bytes, index time, RSS
ff10k query   <vault> "<q>" [--files] [--debug] [--limit N] [--smart-env]
ff10k similar <vault> <vault-relative-path> [--smart-env]
ff10k golden  <vault> <golden.toml>
ff10k bench   <vault>
```

The vault walk skips dot-folders and non-`.md` files and honours Obsidian "Excluded files"
(`userIgnoreFilters` in `.obsidian/app.json`: `/regex/` entries or path prefixes).
`--smart-env` loads Smart Connections embeddings from `<vault>/.smart-env`.

## Credits

fff, fzf, Telescope, Omnisearch, Smart Connections.
