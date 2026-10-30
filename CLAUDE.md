# Fast Find 10K — working notes for Claude Code

Obsidian plugin: Telescope-style note finder for 10k+ vaults. Rust engine → WebAssembly in a Web
Worker; TypeScript plugin. Id `fast-find` (manifest ids allow only letters and hyphens), display
name "Fast Find 10K", repo `ssp-data/obsidian-fast-find-10k`, MIT.

The binding design and every ruling made since are in
`docs/superpowers/specs/2026-10-26-fast-find-10k-design.md` (§16 lists the rulings in order).
Read §6 (query, tiers, score) before touching ranking. This file is the short map; the spec is
the source of truth.

## Owner rules (do not drift)

- Accuracy over speed. Never trade a ranking rule for latency; measure first (`ff10k recall`,
  `docs/recall.md`), report, and let the owner decide tuning. Indexing may take seconds; a query
  must stay a few ms.
- Never read the owner's live vault (`~/Simon/SecondBrain`). Develop and test against
  `~/git/obsidian/trouble-tags-testvault` (a copy plus `Docs/` from `scripts/make-test-vault.sh`).
- The owner uses a US keyboard, Arch Linux, Obsidian desktop first, mobile second.
- Don't push; the owner releases (`make release VERSION=x.y.z`, CI publishes with attestations).

## Layout

- `crates/ff10k-core`: normalize, tokenize, markdown (extractor), canvas (→ markdown), doc,
  query, index, matcher (neo_frizbee fuzzy), rank, frecency, similar, vectors, engine, golden.
- `crates/ff10k-wasm`: JSON-string API over `Engine` (wasm-bindgen 0.2.128 pinned, simd128).
- `crates/ff10k-cli` (`ff10k`): index / query / similar / golden / bench / profile / recall.
- `plugin/src`: `main.ts` (lifecycle, settings, dictionary note, language toggle), `indexer.ts`
  (IndexedDB raw-text cache, 500 ms coalescing), `engine-client.ts` + `worker.ts` (wasm embedded
  in `main.js` via Blob URL), `ui/modal.ts` (Telescope modal, keymap scope), `ui/results.ts`,
  `ui/preview.ts`, `ui/similar.ts`, `ui/help.ts`, `settings.ts`, `settings-tab.ts`
  (declarative on 1.13+, imperative fallback), `smart-connections-bridge.ts`, `debug.ts`.
- `fixtures/vault` + `tests/golden/core.toml`: golden queries (`expect_top`, `expect_in_top`,
  `expect_tier`, `must_not_top3`, `must_not_appear`, `expect_similar_count`).
- `manifest.json`, `versions.json` live in the repo root (Obsidian review requirement).

## Engine behaviour that tests guard (see spec §6 for the rules themselves)

- Tiers: 1 title/alias, 2 headings, 3 content (body, tags, links, path, properties), 4 partial,
  5 semantic; a lower tier never ranks above a higher one. Within a tier exact/prefix hits sort
  before fuzzy-only hits, then score.
- Score parts: field weights × quality, unmatched-title-word factor (0.15/word, floor 0.5),
  whole-title bonus (+15), proximity, backlinks, frecency, combo; short-note and downrank
  multipliers. Ctrl+R shows them.
- Fuzzy (frizbee): 1 typo at 6–9 letters, 2 at 10+, 1 at 5 letters in Title/Alias only; span,
  ratio ≥ 0.65 and matched-char guards; a match never borrows a letter from a neighbouring word
  (`invoice` ≠ "Brand Voice"); window retry per word.
- Short-term rule: ≤ 3 letters exact-only in single-term queries, per compound part.
- Compounds join on `-`, `_` and `.` (`jasper.ai`); camelCase splits.
- Optional words (`a an the of to in on i`) never required. Abbreviation groups
  (`oss: open source, open-source software`) merge into the typed term's unit; hits through them
  are `expanded` (orange in the UI). Languages: built-in `dict/de.txt` + the dictionary note,
  only while the Ctrl+L session toggle is on; Ding/general dictionaries were rejected (noise).
- Filters: `"phrase"`, `#tag`, `path:`, `-word`, `[key]`, `[key:value]`, `type:note|base|canvas|
  drawing`. A tag/key no note has, or an exclusion that leaves nothing, is retried as words.
- Frontmatter: `aliases`, `tags`, `title` (→ alias when it differs from the file name) are
  special; `cssclasses`, `created` are ignored; every other key is a `Property`.
- Files: `.md`, `.base` (YAML as body), `.canvas` (converted to markdown at `Engine::upsert`),
  `.excalidraw` / `.excalidraw.md` (markdown; `compressed-json` fence dropped).
- Similar notes: title overlap + Smart Connections vectors; the `N similar` pill counts exactly
  what the panel lists (same floor, top-5 semantic cap). Query-to-note scores are lower than
  note-to-note: two thresholds (`minSimilarity` 0.70 panel, `semanticMinSimilarity` 0.60 tier).
- Semantic tier default `always`; the bridge re-reads Smart Connections vectors on modal open
  (fingerprint, ≤ 1/30 s); the plugin never computes embeddings.

## Plugin behaviour worth knowing

- Keys are a keymap (`settings.ts` `DEFAULT_KEYMAP`); every action appears once in `ui/help.ts`
  groups (test enforces it), in the footer hints and in the Keys settings. Navigation defaults use
  Alt (J/K preview, H/L panels, N/P matches), never Shift+letter: capitals must stay typable. A
  binding may list alternatives (`Alt+Shift+N | Alt+P`); changed defaults go into
  `KEYMAP_MIGRATIONS` so saved configs follow. The modal swallows
  Ctrl+R / Ctrl+L / Ctrl+C while open; Ctrl+C copies when text is selected, else clears.
- Mobile (`Platform.isMobile` → `.ff10k-mobile`): one column, input line + pill row (+preview
  pill, ✕ close, swipe-left closes), preview off and key hints hidden by default, two-tap open
  when the preview is on, safe-area insets.
- Preview is plain text (speed): image embeds as `<img>`, mermaid fences rendered 250 ms after the
  selection settles, Excalidraw PNG/SVG export shown if present. Nothing else is rendered.
- Diagnostics only through `debug.ts` (`console.debug`, "Log engine timings"); timers via
  `window.*` (`self.*` in the worker); `createFragment()` not `document.*` (Obsidian review).
- Settings: new fields need a default in `DEFAULT_SETTINGS`, clamping in `mergeSettings`, a row in
  both `getSettingDefinitions()` and the legacy tab, and a serde default on the Rust `Config` so
  old configs still parse. New `Field`s need `FIELDS` in both index.rs and matcher.rs, the
  `dicts` array size, `Weights`, plugin `Weights`/`Field` types and `WEIGHT_NAMES`.

## Verify before claiming done

```sh
make test            # cargo test --workspace, goldens, typo recall, plugin tests
make build           # wasm + plugin → plugin/dist
make install VAULT=~/git/obsidian/trouble-tags-testvault   # symlinks dist into the vault
obsidian vault=trouble-tags-testvault plugin:reload id=fast-find   # Obsidian CLI (desktop)
obsidian vault=trouble-tags-testvault dev:mobile on|off            # mobile layout emulation
ff10k recall <vault> [--every 10]                                   # recall numbers, docs/recall.md
```

Timing tests widen by `FF10K_PERF_SLACK` (5 on CI). Keep every golden green; add a fixture +
golden for each ranking change, and re-run `ff10k recall` for anything touching matching.
Live checks against Obsidian go through `obsidian eval` on the plugin's engine API
(`app.plugins.plugins['fast-find'].engine.search(...)`), never by driving the owner's UI while
they work. Mobile emulation and `plugin:reload` reload Obsidian; wait for the plugin to come up.

## Open items

- Fuzzy window hits are broad (`pandas` ~ "Andreas"); tightening proposed, not applied.
- `first two words` recall trails Omnisearch at @1 (tie-break among equals); accepted, see
  `docs/recall.md`.
- Canvas/Excalidraw are previewed as text, not rendered.
- Worker memory ~164 MB on the 9k-note vault (budget 150), `docs/perf.md`.
