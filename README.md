# Fast Find 10K

A fast note finder for large vaults (10k+ notes). A Rust search engine compiled to WebAssembly runs in a worker and powers instant fuzzy, field-weighted, frecency-ranked search with optional semantic "similar notes".

Spec: [docs/superpowers/specs/2026-10-26-fast-find-10k-design.md](docs/superpowers/specs/2026-10-26-fast-find-10k-design.md)
This is how it looks:
![image](docs/images/find-fast-preview.webp)

## Install

There is no release yet (a GitHub release with `main.js`, `manifest.json` and `styles.css` comes later). For development, build and symlink the plugin into a vault:

```sh
bash scripts/build-wasm.sh                  # needs the wasm32-unknown-unknown target and wasm-bindgen-cli 0.2.128
cd plugin && npm ci && npm run build        # dist/main.js (wasm and worker embedded), manifest.json, styles.css
V=<vault>/.obsidian/plugins/fast-find-10k
mkdir -p $V && ln -sf $PWD/dist/main.js $PWD/dist/manifest.json $PWD/dist/styles.css $V/
```

Then enable "Fast Find 10K" under Settings → Community plugins. Requires Obsidian 1.5.7 or newer.
[docs/dev-checklist.md](docs/dev-checklist.md) has the reload and verification steps.

## Usage

Commands (bind them to hotkeys in Settings → Hotkeys):

| Command | What it does |
|---|---|
| Search | opens the finder over titles, aliases, headings, tags, links, paths and content |
| Search file names | opens it in files-only mode (titles, aliases, paths) |
| Search (resume last) | reopens it with the last query |
| Rebuild index | drops the note cache and reads every file again |

Query syntax: plain words are AND-ed and match by prefix and with typo tolerance; `"exact phrase"`, `#tag`, `path:folder`, `-exclude`. No input debounce: every keystroke is a query.

An empty query lists recently opened notes. Rows show which field matched (title, alias, heading, content, link) with highlights, a preview on the right, and an `N similar` pill when near-duplicates exist.

## Keys

Defaults (`Mod` is Cmd on macOS, Ctrl elsewhere); every binding can be changed in the settings, and Ctrl+? shows the current ones in the modal.

| Key | Action |
|---|---|
| ↓ / Ctrl+J | next result |
| ↑ / Ctrl+K | previous result |
| Shift+J / Shift+K | scroll the preview down / up |
| Tab | focus the similar-notes panel |
| Enter | open |
| Mod+Enter | open in a new tab |
| Mod+Alt+Enter | open in a split |
| Alt+Shift+Enter | open all shown results in tabs (10 or fewer) |
| Shift+Enter | create a note named after the query (with the "Already have?" guard) |
| Mod+Shift+Enter | create it in a new tab |
| Ctrl+F | toggle files only |
| Ctrl+S | add semantic results (Smart Connections) |
| Ctrl+P | toggle the preview |
| Ctrl+R | toggle "why ranked here" (tier, per-term field and match, score parts); while the search is open Ctrl+R does not reload Obsidian |
| Ctrl+? (Ctrl+Shift+/) | toggle the key help |
| Esc | close (or leave the similar panel / help) |

## Settings

- **Indexing**: ignored folders/globs (never indexed; Obsidian "Excluded files" are always honoured), downranked folders (still searchable, score × 0.3), rebuild index.
- **Ranking**: field weights (title 100, alias 90, h1 60, h2 50, h3 40, tag 30, link 20, path 15, body 10; a weight of 0 turns the field off), fuzzy typo tolerance (off / normal / loose), partial-match tier (when few / always / never).
- **Semantic**: use Smart Connections embeddings, minimum similarity shown, semantic tier (auto / on demand / never).
- **Behaviour**: create guard, preview open by default.
- **Keys**: every modal binding.
- **Debug**: "why ranked" by default, log engine timings to the console, clear frecency.

## How ranking works

Each query word (and each quoted phrase) must match. A note's **tier** comes from the weakest field its words matched, and a lower tier never ranks above a higher one:

1. **title · alias**: every word in the title or an alias (unresolved link targets, "ghosts", also appear here and can be created with Shift+Enter)
2. **headings**: every word in a title, alias or H1–H3
3. **content**: every word somewhere, including body, tags, links and path
4. **partial**: some words missing but one in the title or an alias (shown when the other tiers have few hits)
5. **semantic**: Smart Connections neighbours of the query, when enabled

Within a tier the score adds the field weights × match quality (exact 1.0, prefix by length ratio, fuzzy below that), shrinks it for title words the query left unmatched, and adds proximity (words close together or in one section), backlinks, frecency (opens over the last 30 days, decaying) and a sticky bonus for a note opened three times from the same query. Downranked folders and very short notes are multiplied down. Ctrl+R shows every part for each row.

## Smart Connections

When [Smart Connections](https://github.com/brianpetro/obsidian-smart-connections) (4.7.2) is installed and has embedded the vault, Fast Find 10K reads its note vectors (read-only, through one bridge module) and uses them for the semantic tier, the similar-notes panel and the create guard. Without it everything else works and the similar panel uses title similarity only.

## Performance

On a 9,385-note / 27 MB vault: warm start about 1.2–1.4 s to fully indexed (searchable after the first 500 notes), engine time per query 4–18 ms in Obsidian, 1–8 ms native. Numbers and method in [docs/perf.md](docs/perf.md).

## Development

- Build: `cargo build --release` (CLI binary: `target/release/ff10k`)
- Test: `cargo test --workspace`; plugin: `cd plugin && npm test`
- Golden: `cargo test -p ff10k-core --features golden --test golden`, or `ff10k golden <vault> <golden.toml>` (exit 1 on failures)
- Benches: `cargo bench -p ff10k-core` (criterion), `ff10k bench <vault>` (p50/p95 in microseconds, 200 runs per query), `ff10k profile <vault>` (ingest time per phase, engine heap)

### CLI (`ff10k`)

```sh
ff10k index   <vault>                                   # docs, bytes, index time, RSS
ff10k query   <vault> "<q>" [--files] [--debug] [--limit N] [--smart-env]
ff10k similar <vault> <vault-relative-path> [--smart-env]
ff10k golden  <vault> <golden.toml>
ff10k bench   <vault>
ff10k profile <vault>                                   # read / extract / tokenize / insert / keys split, heap
```

The vault walk skips dot-folders and non-`.md` files and honours Obsidian "Excluded files"
(`userIgnoreFilters` in `.obsidian/app.json`: `/regex/` entries or path prefixes).
`--smart-env` loads Smart Connections embeddings from `<vault>/.smart-env`.

## Credits

Ideas and behaviour borrowed from [fff](https://github.com/dmtrKovalenko/fff.nvim), [fzf](https://github.com/junegunn/fzf), [Telescope](https://github.com/nvim-telescope/telescope.nvim), [Omnisearch](https://github.com/scambier/obsidian-omnisearch) and [Smart Connections](https://github.com/brianpetro/obsidian-smart-connections). Fuzzy matching by [neo_frizbee](https://crates.io/crates/neo_frizbee).
