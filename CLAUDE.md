# Fast Find 10K — working notes for Claude Code

Obsidian plugin: Telescope-style note finder for 10k+ vaults. Rust engine → WebAssembly in a Web
Worker; TypeScript plugin. Id `fast-find` (manifest ids allow only letters and hyphens), display
name "Fast Find 10K", repo `ssp-data/obsidian-fast-find-10k`, MIT.

The binding design and every ruling made since are in
`docs/superpowers/specs/2026-10-26-fast-find-10k-design.md` (§16 lists the rulings in order).
Read §6 (query, tiers, score) before touching ranking. This file is the short map; the spec is
the source of truth. `docs/architecture.md` is the reader-facing overview (keep it in step when
the indexer, worker, cache or bridge change).

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
- `plugin/src`: `main.ts` (lifecycle, settings, ribbon, dictionary note, language toggle),
  `indexer.ts` (vault diff + events, 500 ms coalescing) over `cache.ts` (IndexedDB raw text,
  frecency, combos), `engine-client.ts` + `worker.ts` + `protocol.ts` (wasm embedded in `main.js`
  via Blob URL), `unresolved.ts` (ghost docs), `actions.ts` (open/create/copy), `keys.ts`
  (binding parse, alternatives), `settings.ts`, `settings-tab.ts` (declarative on 1.13+,
  imperative fallback), `smart-connections-bridge.ts`, `debug.ts`; `ui/`: `modal.ts` (Telescope
  modal, keymap scope), `results.ts`, `preview.ts`, `similar.ts`, `help.ts`, `contrast.ts`.
- `fixtures/vault` + `tests/golden/core.toml`: golden queries (`expect_top`, `expect_in_top`,
  `expect_tier`, `must_not_top3`, `must_not_appear`, `expect_similar_count`).
- `manifest.json`, `versions.json` live in the repo root (Obsidian review requirement).
- `docs/showcase.md`: one screenshot per feature, hot-linked from the GitHub issue that asked
  for it (`scripts/release-images.sh PREV` lists those URLs); the README keeps only three images.
- `docs/architecture.md`: reader-facing overview (worker, cache, updates, Smart Connections).
- `docs/comparison.md`: native search vs Omnisearch vs Fast Find vs QMD on the test vault,
  screenshots in `docs/images/compare/*.webp` (1600 px, ~100 KB). Method: Obsidian CLI evals
  drive each tool (`openGlobalSearch(q)` + `view.resultCountEl`, `omnisearch.api.search`,
  `plugins['fast-find'].engine.search(q,'all',30,false)`), `dev:screenshot`, `magick -resize
  1600x -quality 80`. QMD is a CLI with its own index in `~/.cache/qmd`.
- `docs/dev-checklist.md` (build/install/reload/eval recipes per task), `docs/perf.md`
  (timings, memory), `docs/recall.md` (recall vs Omnisearch, how to re-run).

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
- Mobile (`Platform.isMobile` → `.ff10k-mobile`): one column, input line + pill row (preview and
  matches pills, ✕ close, swipe-left closes), preview on and key hints hidden by default, two-tap
  open when the preview is on, safe-area insets. Footer elements are null there: never touch one
  without `?.` (a throw in the results path silently left the preview empty on mobile).
- Preview is plain text (speed): image embeds as `<img>`, mermaid fences rendered 250 ms after the
  selection settles, Excalidraw PNG/SVG export shown if present. Nothing else is rendered.
- Colours: `styles.css` uses only `--ff10k-*` palette variables (accent, h1–h3, faint, muted,
  orange, selection, on-accent), never a raw theme colour. `ui/contrast.ts` sets them inline at
  modal open (and on `css-change`): each theme colour is blended toward `--text-normal` until it
  reads 4.5:1 on the primary and secondary backgrounds (3:1 for the selection bar/tint). The
  stylesheet's `color-mix` values are only the fallback. Measured across Kanagawa, Asia Neon,
  Fireplace, Osaka Jade, Omarchy and the default theme in both modes (2026-10-30).
- Obsidian's release review lints the source: set styles with classes, `setCssStyles` or
  `setCssProps`, never `el.style.x = …`; no `:has()` in `styles.css` (toggle a class from TS
  instead, e.g. `.ff10k-hit` on preview lines with a match); no scrollbar properties.
- Obsidian review lint: `createFragment()` not `document.*`, `el.setCssStyles()` /
  `el.setCssProps()` not `el.style.*`, no `:has()` in `styles.css` (class the element instead),
  no `innerHTML`. Diagnostics only through `debug.ts` (`console.debug`, "Log engine timings");
  timers via `window.*` (`self.*` in the worker).
- Settings: new fields need a default in `DEFAULT_SETTINGS`, clamping in `mergeSettings`, a row in
  both `getSettingDefinitions()` and the legacy tab, and a serde default on the Rust `Config` so
  old configs still parse. New `Field`s need `FIELDS` in both index.rs and matcher.rs, the
  `dicts` array size, `Weights`, plugin `Weights`/`Field` types and `WEIGHT_NAMES`.

## Settings and keys added since 0.2 (defaults)

- `ribbonIcon` on; `previewOpenMobile` on; `similarPanel` always | desktop | never (title
  similarity needs no Smart Connections, so the panel is not empty with that plugin off; the
  create guard shows it regardless); `keyHints` desktop; `fontScale` 100 (80–130 %, phones cap
  at 120): `--ff10k-scale` on the modal scales results, preview and similar panel through
  `--ff10k-fs-small/smaller` (bar, results, preview, similar); footer and help keep the theme
  size. The settings tab shows a live sample (`.ff10k-sample`, built from the modal's own
  classes) and pushes the slider to an open search via `plugin.applyFontScale()`.
- Keys: Alt+J/K preview scroll, Alt+H/L panels, Alt+N next match, `Alt+Shift+N | Alt+P`
  previous, Ctrl+M matches-only (line numbers, `matches` pill on mobile). `startState` takes
  the preview from the platform setting for resume too.

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
The owner often uses the test vault while a session runs: an eval can come back empty or a
search box can be found empty or with extra letters. Verify the state you need (input value,
result count) right before a screenshot and again after, and retry rather than trust one run.
`dev:mobile on` when already on does not reload, so a new build needs `plugin:reload`. Promises
returned from `eval` are not awaited: write to `window.__x` and read it in a second eval.
Modal internals are reachable as `plugins['fast-find'].modal` (private fields are plain JS).

Shell: `pkill -f <pattern>` matches the calling shell's own command line and kills it; bracket a
letter (`pgrep -f "bin/qmd [e]mbed"`). Parallel Bash calls share cwd; use absolute paths.

## Releasing

`make release-notes VERSION=x.y.z` drafts `RELEASE_NOTES.md` from commits since the last tag
(Claude CLI prose, images from closed issues via `scripts/release-images.sh`); review, commit,
then `make release VERSION=x.y.z` (refuses while the notes are unchanged since the last tag).
The committed `RELEASE_NOTES.md` always describes the *last* release; do not edit it for
unreleased work, the owner drafts at release time. History: spec and plan 2026-10-26, 0.1.0
2026-10-27, 0.2.0 and 0.2.4 2026-10-30 (0.2.3 docs: comparison, architecture, similar-panel
setting; 0.2.4 theme-colour contrast pass). Last released: 0.2.4.

## Open items

- Fuzzy window hits are broad (`pandas` ~ "Andreas"); tightening proposed, not applied.
- `first two words` recall trails Omnisearch at @1 (tie-break among equals); accepted, see
  `docs/recall.md`.
- Canvas/Excalidraw are previewed as text, not rendered.
- Worker memory ~164 MB on the 9k-note vault (budget 150), `docs/perf.md`.
- Reddit/GitHub asks still open: none known. Unreleased after 0.2.4: the review-lint fix
  (`9b7f659`). Several sessions may run at once: check `git status` and `git log` before editing.
