# Fast Find 10K — Design

Date: 2026-10-26
Status: draft, awaiting owner approval
Owner: Simon Späti (simu@sspaeti.com)

## 1. Intent

An Obsidian plugin for **known-item search in large vaults**: the user knows a note exists and wants it in one try. Today the user runs Omnisearch on a ~10k-note, 28 MB markdown vault (plus ~8k images) and increasingly has to search two or three times, retyping almost the exact title, before the note surfaces. Obsidian's own Quick Switcher and `[[` autocomplete also degrade above 10,000 items (hence the name). Not finding a note leads to accidental near-duplicate notes ("AI" / "AI and Data Engineering" / "Data Engineering with AI").

Fast Find 10K replaces Omnisearch for this user. It is a Rust search engine compiled to WebAssembly, running in a Web Worker inside an Obsidian plugin, with a Telescope/fzf-style floating modal. The ranking is structurally title-first so body text can never drown a title hit. It also detects near-duplicates, creates missing notes, and uses Smart Connections embeddings when available.

Inspirations: [fff](https://github.com/dmtrKovalenko/fff) (Rust engine, frecency, SIMD fuzzy, "searchable while scanning"), [fzf](https://github.com/junegunn/fzf) and [telescope.nvim](https://github.com/nvim-telescope/telescope.nvim) (UI, keys, preview), [Omnisearch](https://github.com/scambier/obsidian-omnisearch) (feature baseline and what to avoid).

## 2. Goals, non-goals, anti-goals

**Goals**

1. The known note is the top result on the first query, in typical cases where some of its title words (possibly misspelled, prefixed, reordered) are typed.
2. Query results in a few milliseconds for 10k notes / 30 MB text. UI never freezes, not during indexing, not during typing.
3. Startup to searchable in under one second with a warm cache; cold indexing runs in the background with progressive results.
4. Surface near-duplicate notes next to results and before creating a new note.
5. Honor Obsidian's "Excluded files" setting fully (excluded = invisible, not downranked).
6. Testable ranking: a golden query set that must keep passing as weights are tuned.

**Non-goals (v1)**

- Indexing images, PDFs, canvas, or any non-markdown file. Markdown only.
- In-file search (Omnisearch's second modal).
- Replacing Obsidian's `[[` autocomplete or Quick Switcher. (The user will rebind Quick Switcher themselves later; `[[` interception is a v2 toggle, see §13.)
- Mobile optimization. It should work on mobile because WASM runs there, but desktop is the target and nothing is sacrificed for mobile.
- Block-level (per heading section) semantic search. Note-level only.
- A native sidecar binary. The core is a plain Rust crate so one could be added later; not now.

**Anti-goals (would make it a failure even if it "works")**

- A note whose title contains all query words ranked below a note that only mentions them in the body.
- A 300 ms input debounce or any visible lag per keystroke.
- Indexing on the UI thread, or one giant serialized index blob rewritten on every change (Omnisearch's design).
- Short tokens like `AI` matching `aim`, `air`, `Airflow`.
- Errors or degraded search when Smart Connections is absent, disabled, or half-indexed.

## 3. Constraints

- **Obsidian plugin packaging.** A community plugin release ships exactly `main.js`, `manifest.json`, `styles.css`. The WASM binary and the worker script must be embedded in `main.js` (base64 / Blob URL) and instantiated at load. Keep the WASM under ~2 MB.
- **Plugin id/name.** Id `fast-find-10k`, name "Fast Find 10K". Obsidian guidelines forbid "Obsidian" in the name. Repo: `obsidian-fast-find-10k` (current folder `obsidian-fzf` is to be renamed by the owner; builder works in whatever the folder is called).
- **Toolchain on the dev machine.** Arch Linux, `cargo 1.99` from pacman (not rustup), Node 26. The `wasm32-unknown-unknown` target and `wasm-bindgen-cli` must be installed (Arch packages `rust-wasm` and `wasm-bindgen`, or rustup). Builder verifies.
- **Test vault.** `/home/sspaeti/git/obsidian/trouble-tags-testvault` (~9.4k notes, Smart Connections installed and indexing). The user's real vault must never be read by tooling or tests. Synthetic fixtures go under `generated/` in the test vault; fixtures committed to the repo must be synthetic, never copies of the user's notes.
- **Real vault sizing** (for budgets): 9,885 notes, 28.5 MB markdown, 8,013 images, 17,966 files; Smart Connections has 12,441 note vectors (19 MB) and 70,495 block vectors.

## 4. Architecture

```
┌──────────────────────── Obsidian (renderer) ────────────────────────┐
│  main.ts (plugin shell, TypeScript)                                  │
│   • vault events, cachedRead, metadataCache.isUserIgnored,           │
│     unresolvedLinks, Smart Connections bridge, settings              │
│   • Modal UI (Telescope layout), preview, similar panel, keys        │
│   • IndexedDB cache (per-note rows) + frecency store                 │
│            │  postMessage (structured clone / transferables)         │
│  ┌─────────▼──────────── Web Worker ──────────────────────────────┐  │
│  │  worker.ts  → loads WASM, owns the engine, serializes queries   │  │
│  │  ┌───────────────── Rust core (wasm32) ─────────────────────┐   │  │
│  │  │ markdown field extraction · normalization · inverted     │   │  │
│  │  │ index · frizbee fuzzy · tiered ranking · frecency math · │   │  │
│  │  │ title-overlap dedup · cosine over vector matrix          │   │  │
│  │  └───────────────────────────────────────────────────────────┘   │  │
│  └────────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────┘
```

**Rust workspace layout** (builder may rename):

- `crates/ff10k-core` — pure Rust, no wasm or Obsidian dependency. Everything about indexing and ranking. Unit tests, golden tests, criterion benches live here.
- `crates/ff10k-wasm` — thin `wasm-bindgen` wrapper exposing the worker API.
- `crates/ff10k-cli` — dev tool: `ff10k index <dir>`, `ff10k query <dir> "<q>"`, `ff10k golden <dir> <golden.toml>`, `ff10k bench <dir>`. Reads a vault folder directly, optionally reads `.smart-env` vectors. Used for tuning against the full test vault without opening Obsidian.
- `plugin/` — TypeScript plugin, esbuild, embeds the wasm + worker.

**Why these choices**

- Rust/WASM instead of native: the corpus is small (30 MB); WASM with SIMD answers queries in low single-digit ms. Native would add per-OS binaries and downloads for no perceptible gain. Obsidian's vault events replace a filesystem watcher.
- Web Worker: Omnisearch's main weakness is main-thread indexing and freezes. All engine work runs off-thread; the UI thread only renders.
- Rust parses the markdown itself (frontmatter, headings, tags, links) from raw text instead of relying on Obsidian's `metadataCache`. One deterministic extraction path that the CLI and tests share. `metadataCache` is only used for `isUserIgnored` and `unresolvedLinks`.

## 5. Indexing

### 5.1 Sources and exclusions

- Index every `.md` file in the vault except:
  - files for which `app.metadataCache.isUserIgnored(path)` is true (Obsidian "Excluded files"). These are never indexed, never shown.
  - paths matching the plugin's own "Ignored folders/globs" setting.
  - `.obsidian/`, `.trash/`, `.smart-env/`, and any dot-folder.
- Nothing else is indexed in v1. Images and attachments are not even enumerated.
- A separate "Downranked folders" setting (e.g. `Archive/`, `templates/`) keeps notes searchable but applies a score multiplier (default 0.3) within their tier.

### 5.2 Per-note document model (extracted in Rust)

| Field | Source | Notes |
|---|---|---|
| `path` | vault path | id |
| `title` | basename without `.md` | emoji stripped for matching, kept for display |
| `aliases[]` | frontmatter `aliases` (string or list) | each alias separately (Omnisearch bug to avoid) |
| `h1[]`, `h2[]`, `h3[]` | ATX headings `#`, `##`, `###` | deeper headings fold into body; heading line numbers kept for jump-to |
| `tags[]` | frontmatter `tags` + inline `#tag` | nested `#a/b` yields `a/b`, `a`, `b` |
| `links[]` | `[[Target]]`, `[[Target|alias]]`, `[[Target#Heading]]` | target and alias text indexed as link text; targets also used for backlink counts |
| `path_segments[]` | folder names | emoji stripped |
| `body` | everything else | frontmatter block removed, wikilink/markdown syntax stripped to its text, code fences kept as text, base64 data URIs dropped |
| `mtime`, `size` | fs stat | cache key |
| `word_count` | derived | shorter-title / tiny-note tie-breaks |

Headings are stored with their byte offset and line number so a heading hit can open the note at that heading.

### 5.3 Normalization and tokenization

Applied identically to documents and queries:

- Unicode NFKC, then lowercase.
- Fold diacritics (`é`→`e`), except where it would merge Japanese marks (follow Omnisearch's exception list).
- Collapse all whitespace variants (NBSP U+00A0, zero-width U+200B/U+FEFF, non-breaking hyphen) to a plain space or remove; trim. This directly fixes the user's "hidden characters in filename" failure class.
- Strip emoji and symbol pictographs from title and path segments for matching only.
- Tokens: split on any non-letter/digit. Additionally split camelCase (`DataEngineering` → `data`, `engineering`) and keep hyphen/underscore compounds both whole and split (`data-engineering` → `data-engineering`, `data`, `engineering`).
- No stemming. Prefix matching covers `engineer`/`engineering`.
- Keep positions for body tokens (needed for proximity and phrase matching).

### 5.4 Index structures (in the worker, in memory)

- String arena for titles, aliases, headings (for frizbee fuzzy matching on the raw strings).
- Per-field inverted index: token → posting list of (doc id, positions). Term dictionary sorted (or an FST) so prefix queries are a range scan.
- Doc table with field metadata, mtime, folder id, frecency score, backlink count.
- Optional vector matrix: `n_docs × 384` f32, row per doc id, plus a "has vector" bitmap (see §9).

Memory budget for 10k notes / 30 MB: under 150 MB in the worker.

### 5.5 Cache and startup

Persist per-note rows in IndexedDB (database `ff10k/<appId>`, store `notes`, key `path`): `{path, mtime, size, text}` where `text` is the raw file contents. Rationale: reading 10k files through Obsidian's vault API is the slow part (seconds); re-extracting fields and rebuilding the index from cached text in Rust takes well under a second for 30 MB. Persisting the built index is deliberately avoided.

Startup sequence:

1. Plugin load → spawn worker → instantiate WASM.
2. Stream all cached rows to the worker in batches (e.g. 500 notes per message, transferable buffers). The engine becomes searchable as soon as the first batch is in; the modal shows "indexing 3,120 / 9,383" in its status area while incomplete (fff's scan-while-searching).
3. Diff vault file list (path, mtime, size) against cache: read changed/new files with `cachedRead`, send to worker, upsert rows in IndexedDB; delete rows for missing files.
4. Done. Status shows total and time.

Incremental updates during the session: `create`, `modify`, `delete`, `rename` vault events → coalesce per path for ~500 ms → read → upsert worker and cache. No "wait for window blur" (Omnisearch). Renames update the path key and keep frecency.

Cache invalidation: a schema version number; on mismatch, drop and rebuild. A "Rebuild index" command and settings button do the same.

Unresolved links are not cached; they come from `app.metadataCache.unresolvedLinks` on load and on `resolved` events, aggregated into (target → referencing note count), and sent to the worker as ghost docs with `title = target`, `is_ghost = true`.

## 6. Query and ranking

### 6.1 Query syntax

- Whitespace-separated terms, all must match (AND), order-free.
- `"exact phrase"` → terms must be adjacent in one field.
- `-term` → exclude notes where term matches anywhere.
- `#tag` → filter to notes with that tag (nested tags match parents).
- `path:foo` → filter to paths containing `foo` (case-insensitive substring over normalized path). Applied **before** scoring, not after a top-N cut.
- Everything else is literal. No regex in v1.

Filters narrow the candidate set first; scoring runs only on candidates.

### 6.2 Per-term matching

For each query term, against each field:

| Term length | Allowed matches |
|---|---|
| 1–3 chars, single-term query | whole token only (exact). `ai` matches token `ai`, never `aim`, `Airflow`. |
| 1–3 chars, multi-term query | exact or prefix. The other terms already narrow the candidates, so `data eng` may reach `engineering`. Exact token matches score 1.0 and prefix matches score `term_len / token_len`, so "AI in Data Engineering" still outranks "Airflow for Data Engineering" on `ai data engineering`. (Design call, see §16.) |
| 4–5 chars | exact or prefix |
| 6+ chars | exact, prefix, or fuzzy on title/alias/heading strings |

Fuzzy uses frizbee (`neo_frizbee`, SIMD Smith-Waterman, same matcher as fff and blink.cmp). Typo budget: 1 typo for 6–9 chars, 2 for 10+. Fuzzy is applied only to title, alias and heading strings; body matching is exact/prefix via the inverted index (fuzzy body matching is noise at this scale and is explicitly not wanted).

Smart case: if the query contains an uppercase letter, case-sensitive bonus like fff; otherwise case-insensitive.

### 6.3 Tiers (hard ordering)

Each result gets a tier. Results are sorted by tier first; nothing in a lower tier is ever shown above a higher tier.

| Tier | Condition | Label in UI |
|---|---|---|
| 1 | every term matches in `title` or `aliases` | title / alias |
| 2 | every term matches in title, aliases, or `h1`–`h3` (at least one term only in a heading) | h1 / h2 / h3 |
| 3 | every term matches somewhere (at least one term only in `tags`, `links`, `path_segments`, or `body`) | body (or tag / link / path) |
| 4 | not every term matches, but at least one term matches in title/alias | partial (greyed) |
| 5 | semantic neighbours of the query (only with Smart Connections, see §9) | semantic |

Ghost docs (unresolved links) participate in tier 1 with their target text as title and render distinctly.

Tier 4 is shown only when tiers 1–3 together hold fewer than 5 results, or always via a setting. Tier 5 likewise (fewer than 5 lexical hits, or Ctrl+S).

Worked examples (also golden tests):

- `obsidian trouble` → "Obsidian Troubleshooting": both terms in title → tier 1, top.
- `data engineer ai` → "AI in Data Engineering" tier 1; "👉🏻 Data Engineering" with an `## AI …` heading tier 2; "RW Pandas AI" with "data engineer" in body tier 3.
- `AI` → tier 1 only titles containing the token `ai`; "Airflow" never appears.

### 6.4 Score within a tier

`score = Σ_terms (field_weight × match_quality) + proximity + frecency + combo + structural − penalties`

- `field_weight` defaults: title 100, alias 90, h1 60, h2 50, h3 40, tag 30, link text 20, path segment 15, body 10. Each term counts its best field only.
- `match_quality` ∈ (0, 1]: exact token 1.0, prefix scaled by `term_len / token_len`, fuzzy = normalized frizbee score.
- `proximity`: bonus when all terms fall within one heading section, and a larger bonus when within 8 body tokens of each other. Phrase queries require adjacency instead.
- `frecency`: fff's formula: `Σ exp(−0.0693 × days_since_access)` over the last 30 days (10-day half-life), capped (`10 + sqrt(x − 10)` above 10), scaled into the score. An "access" is **any** note open in Obsidian (`workspace.on("file-open")`), not only opens through the modal, so the signal is useful from day one and the empty-query list behaves like a recent-files list. Opens triggered by the modal itself are recorded once (not double-counted via `file-open`). Stored in IndexedDB store `access` (`path → [timestamps]`, max 128 per path). "Clear frecency" wipes this store. (Design call, see §16.)
- `combo`: fff's query tracker. If the same normalized query led to opening the same note ≥ 3 times, that note gets a large boost for that query (effectively sticky top). Store `queries` (`query → {path, count}`).
- `structural`: small bonus for shorter titles (exact-title hits beat long titles containing the words), and `log(1 + backlinks)` as a hub bonus (tiny).
- `penalties`: downranked folders multiply by 0.3; notes under 20 words get a small penalty unless title matched exactly.

Ties: more recent `mtime` first, then path.

Empty query: tier 0 "recent" = top 20 by frecency, then by mtime. Ghost docs excluded.

Result limit: 50 scored results returned to the UI per query; the UI renders 30 and loads the rest on scroll. `select_nth_unstable` style partial sort in the engine.

### 6.5 "Why ranked here" debug view

Keyboard toggle (default Ctrl+/ in the modal; configurable) shows under each row: tier, per-term field hit and match quality, and the score components. Also the engine returns these in the result payload only when debug is on. This is a tuning tool and should be cheap to keep.

### 6.6 Latency

No debounce. Each keystroke posts a query with an incrementing id; the worker drops any queued query older than the latest before running; the UI ignores responses whose id is not the latest. Budget: p95 < 10 ms engine time for 10k notes, UI render < 16 ms.

## 7. Near-duplicate detection ("similar notes")

Two independent signals, merged in the similar panel for the selected result:

1. **Title-token overlap** (always available). Jaccard over normalized title + alias token sets. ≥ 0.8 → "same words, different order"; 0.6–0.8 → "similar title". Also flags titles equal after normalization except trailing `-2`, `(1)`, `copy`, extra punctuation, double spaces (the "RW MLOps is mostly data engineering" vs "…Engineering." case). Computed in the engine over tier 1–3 results plus the selected note's nearest title neighbours (cheap: only compare the selected note against the result list and against notes sharing ≥ 2 rare title tokens via the title inverted index).
2. **Embedding cosine** (only with Smart Connections, §9). Top 5 by cosine against the selected note's vector, shown with the same 0–1 score the Smart Connections pane shows, so numbers match what the user already knows.

Rows in the result list that have ≥ 1 similar note (either signal above threshold) get an "N similar" badge. The panel lists them with the reason ("same words, different order", "0.83").

## 8. Create note and unresolved links

- Shift+Enter creates a note from the query text (Mod+Shift+Enter: in a new tab). Filename = query with `\ / : * ? " < > |` removed and whitespace trimmed; if the selected row is a ghost (unresolved link), the exact link target is used instead. Location follows Obsidian's "Default location for new notes" (same folder as current, configured folder, or vault root), via `fileManager` / `vault.create`, then open.
- **Create guard**: before creating, the modal swaps the similar panel to "Already have?" showing up to 3 nearest existing notes (title-overlap and, if available, semantic neighbours of the query embedding). Enter on one of them opens it instead; Shift+Enter again confirms creation; Esc cancels. Setting to disable the guard.
- Ghost rows: unresolved link targets appear as tier-1 candidates, rendered italic with a dashed icon and "not created · linked from N notes". Enter on a ghost = create it (with the guard).
- Alt+Enter inserts a link to the selected result at the cursor of the active editor (`fileManager.generateMarkdownLink`); for a heading hit, `[[Note#Heading]]`. The modal closes.

## 9. Smart Connections integration

Setting "Use Smart Connections embeddings" (default: on when the plugin is installed and enabled). Design target is **Smart Connections 4.7.2** (installed in both the test vault and the owner's vault). The property paths below were verified against the 4.7.2 bundle (`embed_batch` taking `{embed_input, purpose}`, items exposing `.vec`, `mf_*` raw float32 stores with `file_i` in the `.ajson`). All Smart Connections access lives in one bridge module (`plugin/src/smart-connections-bridge.ts`) so a future major version only touches that file; the bridge checks `manifest.version` and disables itself with a console warning on a major version it does not know. When on:

- Bridge waits for `window.smart_env` and `smart_env.smart_sources` to be loaded (poll with backoff, up to ~60 s, then give up silently and retry on the next modal open).
- Vectors: iterate `smart_env.smart_sources.items`; for each with a `vec` (Float32Array, 384 dims) take `path → vec`. Copy into one contiguous `Float32Array` and transfer it to the worker with a `path[]` list. Rebuild the matrix when the item count or any `last_read.hash` changes (check on modal open, at most once per 30 s). Notes without a vector simply have none (bitmap).
- Query embedding: `smart_env.smart_sources.embed_model.embed_batch([{embed_input: query, purpose: "query"}])` → `vec`. Only called for tier 5 (semantic) and the create guard, never per keystroke for lexical search. Result cached per normalized query for the modal session.
- Cosine similarity is computed in Rust over the matrix (vectors are already normalized; dot product suffices). Scores are displayed as Smart Connections displays them (cosine, 2 decimals).
- The engine must work identically with the vector matrix absent. No code path may throw when `window.smart_env` is missing, partially loaded, or a different model/dimension (if `dims ≠ 384`, read dims from `embedding_models` and adapt; if inconsistent, disable with a one-line console warning).
- The CLI can load vectors from `.smart-env/smart_sources/mf_*` (raw little-endian f32, `dims × 4` bytes per row) using `file_i` from `smart_sources*.ajson` for offline tests of the similar panel. The plugin does not read these files.

## 10. UI

Telescope layout (approved mockup "A"): a floating modal taking roughly 90% of the window width and height.

- **Top bar**: search input; mode pills `all` / `files only`; status `21 / 9,383 · 3 ms`, or `indexing 3,120 / 9,383` while cold.
- **Left column (≈55%)**: results grouped by tier with small uppercase group labels. Each row: match badge (`title`, `alias`, `h2`, `h3`, `body`, `tag`, `link`), title with matched characters highlighted, folder path in muted text (full path on hover), for heading hits the heading text in accent colour, for body hits a one-line excerpt around the first match, `N similar` badge where applicable. Ghost rows italic with dashed icon.
- **Right column, top**: preview of the selected note: lightweight text rendering (headings coloured, `[[links]]` and tags tinted, matches highlighted, scrolled so the first match is visible). Not full Obsidian markdown rendering in v1 (keeps it instant on 50-page notes). Preview toggle Ctrl+P hides it and gives the full width to results.
- **Right column, bottom**: similar notes panel (§7). Shows "Smart Connections off" hint text when only title overlap is available.
- **Footer**: key hints.
- Styling uses Obsidian CSS variables so themes apply. Monospace is not forced.

Keys (modal; all configurable in settings):

| Key | Action |
|---|---|
| Enter | open (jump to matched heading/body line) |
| Mod+Enter | open in new tab |
| Mod+Alt+Enter | open in split |
| Shift+Enter | create note from query / ghost (with guard) |
| Alt+Enter | insert `[[link]]` at cursor |
| Ctrl+F (in modal) | toggle files-only mode (tier 1 only, no body index consulted) |
| Ctrl+S | add semantic tier for this query |
| Ctrl+J / Ctrl+K, ↑ / ↓ | move selection |
| Ctrl+P | toggle preview |
| Ctrl+/ | toggle "why ranked" |
| Tab | focus similar panel (then Enter opens the similar note) |
| Esc | close |

Commands registered: "Fast Find 10K: Search", "Fast Find 10K: Search file names", "Fast Find 10K: Rebuild index". No default hotkeys (Obsidian guideline); README suggests binding the first to the user's Omnisearch key.

Opening a note from a heading/body hit places the cursor at the match line and scrolls it into view (Omnisearch does this; keep it).

Modal prefill: current editor selection if any, else empty (not the last query; the empty query shows the frecency list which is more useful).

## 11. Settings

- Ignored folders/globs (in addition to Obsidian's Excluded files).
- Downranked folders (multiplier 0.3).
- Field weights (title, alias, h1, h2, h3, tag, link, path, body) as sliders with "reset to defaults".
- Fuzzy typo tolerance: off / normal (default) / loose.
- Show partial-match tier: only when few results (default) / always / never.
- Semantic tier: auto when few results (default) / on demand only / never.
- Use Smart Connections embeddings (on/off), minimum similarity shown (default 0.70).
- Create guard on/off.
- Preview on/off by default.
- Keymap.
- Debug: show "why ranked" by default; log engine timings to console.
- Buttons: Rebuild index, Clear frecency.

## 12. Testing and tuning

- **Rust unit tests** in `ff10k-core`: normalization, tokenizer, markdown field extraction (frontmatter variants, nested tags, aliases list vs string, headings with trailing `#`, wikilinks with aliases/headings/blocks), tier assignment, score monotonicity, dedup rules.
- **Golden query tests**: `tests/golden/*.toml`, each entry `{query, mode?, expect_top = "<title or path>", expect_in_top = ["…"], must_not_top3 = ["…"]}`. Run against a synthetic fixture vault committed under `fixtures/vault/` (generated by a script; includes the DE/AI family, near-duplicate pairs, decoys like "Airflow"/"aim", emoji folders, NBSP titles, aliases, nested tags, unresolved links). The same runner, via the CLI, runs against the full test vault locally (not in CI). Initial golden set from the conversation:

  | Query | Expect top | Must not top 3 |
  |---|---|---|
  | `obsidian trouble` | Obsidian Troubleshooting | |
  | `data engineering ai` | AI in Data Engineering (tier 1 family) | body-only hits |
  | `data engineer ai` | same | |
  | `ai data engineering` | same | |
  | `AI` | notes with token `ai` in title | Airflow, aim decoys |
  | `mlops data engineering` | both MLOps notes, flagged similar | |
  | `rust data eng` | both Rust DE notes, flagged similar | |
  | `obsidian troubleshoot` (typo variants `troubelshoot`) | Obsidian Troubleshooting | |

- **Benches** (criterion): index build for 10k synthetic notes, query latency for 1-, 2-, 3-term queries, fuzzy query, vector scan. Budgets in §14.
- **Plugin-side tests**: minimal; the TypeScript shell is thin. A manual checklist in `docs/` for modal behaviour on the test vault.

## 13. Phasing

**v1 (this design)**: everything above except items listed under v2.

**v2 candidates** (not designed here): `[[` autocomplete via `EditorSuggest` with a toggle (plugin suggesters take priority over Obsidian's built-in, so it is feasible); block-level semantic neighbours; other file extensions (canvas, txt); full Obsidian markdown rendering in the preview; in-file search; mobile tuning; a native sidecar.

## 14. Performance budgets (10k notes, 30 MB, desktop)

| Metric | Budget |
|---|---|
| Warm start → searchable | < 1 s |
| Cold full index (reading via vault API) | < 30 s, in background, progressive |
| Query engine time, p95 | < 10 ms |
| Keystroke to painted results | < 30 ms |
| Semantic query (embed + scan) | < 100 ms |
| Worker memory | < 150 MB |
| `main.js` size (with embedded wasm) | < 3 MB |

## 15. Left to the builder

- Exact crate names, module layout, serialization format for worker messages (JSON vs binary), choice of FST vs sorted vector for the term dictionary.
- Exact proximity window sizes and the numeric constants in §6.4, as long as the golden tests pass and the constants are settings-visible where §11 says so.
- How the wasm and worker are embedded in `main.js` (base64 inline + Blob URL is the known approach).
- esbuild config, Rust build scripts, CI (GitHub Actions building wasm + plugin release assets).
- Whether the preview reads the note text from the worker (cached) or via `cachedRead` (either is fine; worker avoids disk).
- Fixture generator script design.

## 16. Decisions made during brainstorming (for the record)

- WASM-only, desktop-optimized, mobile incidental. No native binary.
- Telescope layout (A) over stacked layout (B).
- Semantic features all in v1: similar panel, create guard, semantic tier (gated by thresholds and a toggle).
- Rust extracts fields from raw markdown; Obsidian metadata only for exclusions and unresolved links.
- Name "Fast Find 10K", id `fast-find-10k`.
- The owner will rebind Quick Switcher themselves; `[[` autocomplete is a v2 toggle.

Calls made by the designer after the builder check (owner may override):

- Short terms (≤3 chars) are exact-only in single-term queries but may prefix-match in multi-term queries. Keeps `AI` alone strict while letting `rust data eng` work.
- Frecency counts every note open in Obsidian, not only opens via the modal.
- Smart Connections bridge targets 4.7.2, isolated in one module, self-disables on an unknown major version.

Rulings made during implementation (controller):

- Smart case (§6.2) is deferred: fuzzy matching runs the normalized query against normalized field strings, so case bonuses are inert in v1.
- Fuzzy hits are rejected when the matched span exceeds `needle_chars + 2×max_typos + 1` characters or the score ratio is below 0.5, so scattered subsequences (`airflow` → "AI rules for low budgets") never enter tier 1.
- The short-term rule applies per compound part (`gen-ai` keeps `ai` exact-only in single-term queries).
