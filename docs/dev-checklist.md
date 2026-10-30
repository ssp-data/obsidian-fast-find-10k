# Dev checklist: build, install, reload, verify

Test vault: `<path>/trouble-tags-testvault` on the owner's machine, never the live vault. About half of it is real documentation markdown pulled in with `bash scripts/make-test-vault.sh <vault>` (DuckDB, dbt, Kubernetes, VS Code docs, the Rust book; permissive licences, about 5,000 notes under `Docs/`), the rest the owner's own test notes.
Every Obsidian CLI call below targets it explicitly with `vault=trouble-tags-testvault`; Obsidian must be running with that vault open.

## Build

```bash
bash scripts/build-wasm.sh            # crates/ff10k-wasm/pkg/ (ff10k_wasm.js + ff10k_wasm_bg.wasm)
cd plugin
npm install
npm run build                         # tsc --noEmit, then esbuild → dist/main.js, dist/manifest.json, dist/styles.css
npm run dev                           # same, watching (inline sourcemaps)
npm test                              # node --test: worker bundle in a Node worker thread
stat -c %s dist/main.js               # must stay < 3 MB (0.1.0: 1,268,992 bytes)
```

The worker (`src/worker.ts` + wasm as bytes) is bundled first as an IIFE string and embedded in
`main.js` through the virtual module `virtual:worker-source`; `main.js` is the only code artifact.

## Install (once)

```bash
V=<vault>/.obsidian/plugins/fast-find
mkdir -p $V && ln -sf $PWD/dist/main.js $PWD/dist/manifest.json $PWD/dist/styles.css $V/
```

## Enable and reload

```bash
o() { obsidian vault=trouble-tags-testvault "$@"; }
o eval code="app.plugins.loadManifests().then(()=>0)"   # pick up a newly symlinked plugin
o plugin:enable id=fast-find filter=community        # once
o plugin:reload id=fast-find                         # after every build
o plugin id=fast-find                                # shows "enabled true"
```

## Verify

```bash
o dev:debug on                    # start capturing console messages
o dev:console clear
o plugin:reload id=fast-find
o command id=fast-find:search
o eval code="[...document.querySelectorAll('.notice')].map(n=>n.textContent).join(' | ')"
                                  # → "Fast Find 10K: 9382/9382 indexed, top "Obsidian Troubleshoot""
o dev:console level=error         # nothing from fast-find
o dev:errors                      # nothing from fast-find
o dev:debug off
```

`eval` does not await promises; to check an async result, stash it on `window` and read it in a
second call:

```bash
o eval code="const e=app.plugins.plugins['fast-find'].engine; e.upsert([{path:'x/Obsidian Troubleshooting.md',mtime:1,text:'# x'}]); window.__a=e.search('obs','all',5,false).then(()=>'resolved',x=>x); window.__b=e.search('obsidian trouble','all',5,false).then(r=>r.hits[0].title+' '+r.elapsedUs); Promise.all([window.__a,window.__b]).then(v=>window.__ff=v); 'sent'"
o eval code="JSON.stringify(window.__ff)"   # → ["stale","Obsidian Troubleshooting <elapsedUs>"]
o eval code="app.plugins.plugins['fast-find'].engine.remove(['x/Obsidian Troubleshooting.md']); 'ok'"
```

## Indexing (cache + indexer)

The console always logs one line per full run:
`[ff10k] indexed N docs in X ms (warm: Y from cache, Z re-read)`. Live status:
`app.plugins.plugins['fast-find'].indexer.status` → `{indexed, total, done, elapsedMs}`.
Cache: IndexedDB `ff10k/<appId>` (stores `notes`, `access`, `queries`, `meta`).

```bash
o command id=fast-find:rebuild-index      # clear note cache, re-read every file (cold)
o dev:console | grep ff10k                    # → indexed 9382 docs ... (warm: 0 from cache, 9382 re-read)
o plugin:reload id=fast-find              # warm start
o dev:console | grep ff10k                    # → ... (warm: 9382 from cache, 0 re-read)
# onload → done, measured around disable/enable:
o eval code="app.plugins.disablePlugin('fast-find').then(()=>{window.__t1=performance.now(); return app.plugins.enablePlugin('fast-find')}).then(()=>{const p=app.plugins.plugins['fast-find']; const iv=setInterval(()=>{ if(p.indexer?.status.done){clearInterval(iv); window.__r=Math.round(performance.now()-window.__t1)} },5)}); 'sent'"
o eval code="window.__r"
```

Test vault (9,384 `.md`; 1 in a dot-folder and 1 in `templates/` (Obsidian "Excluded files") → 9,382
indexed; ~7,000 images never enumerated), 2026-10-26, task 16:

| Measure | Result |
|---|---|
| Cold (empty cache), from `start()` | 9,382 docs in 3,667 ms |
| Warm, onload → done (3 runs) | 3,567 / 3,593 / 3,587 ms, 0 re-read |
| Warm, onload → first batch searchable | 309 ms |
| Cache cursor, all 9,382 rows (no engine) | 182 ms (first 500 at 16 ms) |
| Worker `upsert`, 9,382 docs | ~3,556 ms (dominates warm start) |
| Edit a note → new word searchable | 567 ms (500 ms coalesce + read), doc count unchanged |
| Add 120 images to a folder | 0 `cachedRead` calls, doc count unchanged |
| Rename a note | new title searchable at once, doc count unchanged |
| Rebuild command | 7,619 ms (bulk `remove` of 9,382 docs ≈ 3.3 s + re-read/upsert) |

Warm start misses the < 1 s budget: the time is in the worker (wasm extraction + indexing), not
in IndexedDB. Tuning belongs to Task 22.

Pre-existing errors from other plugins in the test vault (smart-connections "Ribbon actions require
converged env.config.actions", cursor-smith watchdog) are unrelated.

## Search modal (task 17)

The CLI cannot type into Obsidian, but the modal reacts to synthetic DOM events, so it can be
driven through `eval`: set the input value and fire `input`; fire `keydown` on `document`
(Obsidian's keymap routes it to the modal's `Scope`).

```bash
o eval code="app.plugins.plugins['fast-find'].settings.logTimings=true; 'ok'"   # session only
o command id=fast-find:search                 # or search-files (opens in files-only mode)
# type a query
o eval code="const i=document.querySelector('.ff10k-input'); i.value='obsidian trouble'; i.dispatchEvent(new Event('input')); 'ok'"
# read status, tier labels and the top rows
o eval code="const m=document.querySelector('.ff10k-modal'); [m.querySelector('.ff10k-status').textContent, [...m.querySelectorAll('.ff10k-tier')].map(e=>e.textContent).join(' | '), ...[...m.querySelectorAll('.ff10k-row')].slice(0,5).map(r=>r.textContent)].join('\n')"
# press a key (Ctrl+F; use key:'Enter', key:'j', key:'/', key:'Escape', ctrlKey/altKey as needed)
o eval code="document.dispatchEvent(new KeyboardEvent('keydown',{key:'f',ctrlKey:true,bubbles:true,cancelable:true})); 'ok'"
# after an open: leaf count, active file, cursor
o eval code="app.workspace.getLeavesOfType('markdown').length+' '+app.workspace.getActiveFile()?.path+' '+JSON.stringify(app.workspace.activeEditor?.editor?.getCursor())"
# simulated typing, 15 chars, one per 60 ms; then read the timing lines
o eval code="const q='data engineerin'; const i=document.querySelector('.ff10k-input'); let n=0; const step=()=>{ n++; i.value=q.slice(0,n); i.dispatchEvent(new Event('input')); if(n<q.length) setTimeout(step,60); }; step(); 'typing'"
o dev:console | grep '\[ff10k\] search'           # query, engine µs, render ms
o dev:screenshot path=/tmp/ff10k-modal.png        # may show the previous frame; re-run if stale
```

Observed on the test vault, 2026-10-26:

| Check | Result |
|---|---|
| empty query | `recent` group, 20 rows, status `20 / 9,382 · 3 ms` |
| `obsidian trouble` | first row `title` "Obsidian Troubleshoot" under `title · alias`; labels `title · alias \| headings \| content`; title highlights `<Obsidian> <Trouble>shoot` |
| `data engineer ai` | 50 rows, labels `title · alias \| headings \| content` in that order; `N similar` pills on heading rows |
| `AI` | 50 title rows (Artificial intelligence (AI), OpenAI, …), 0 rows containing "Airflow" |
| Ctrl+F | pill `files only` active, only `title` badges (labels `title · alias \| partial`); Ctrl+F again → `all` |
| Ctrl+J ×2, ↓ / Ctrl+K, ↑ | selection 0 → 3 → 1 |
| Ctrl+P | `ff10k-preview-hidden` toggles on the modal |
| Ctrl+/ | per-term debug line under every row, e.g. `obsidian: title exact 1.0 · trouble: title prefix 0.6 \| fields 158.33 · … \| score 186.92` |
| Tab / Shift+Enter / Ctrl+S | Notice "coming in a later task" |
| Enter | same leaf (3 → 3 leaves), opens Obsidian Troubleshoot, modal closed |
| Mod+Enter | new tab (3 → 4 leaves) |
| Mod+Alt+Enter | split (root split children 1 → 2) |
| heading hit (`h3 ### Troubleshooting` in Obsidian.md) | cursor `{line:223, ch:0}`; line 223 is `### Troubleshooting` |
| 30 rows rendered, scroll to bottom | 50 rows |
| editor selection `# Obsidi` | prefilled as the query |
| Esc | modal closed |
| typing `data engineerin` at 60 ms/char | 15/15 responses painted; engine 6.1–15.3 ms (median ≈ 7.8), render 2.4–5.2 ms |
| typing `obsidian troubl` at 15 ms/char | 12/15 painted (3 superseded as stale); engine 2.2–14.1 ms, render 2.5–3.9 ms; final top row correct |

First query after a plugin reload takes 59–68 ms in the engine (cold); later queries 2–18 ms.

## Preview pane and similar panel (task 18)

Preview and similar panel paint in one `requestAnimationFrame` per selection change. A window
that is not composited (Obsidian on another Hyprland workspace) produces no frames, so after a
synthetic key press force one with `o dev:screenshot` before reading the DOM.

```bash
S=/tmp/ff10k; mkdir -p $S
K() { o eval code="document.dispatchEvent(new KeyboardEvent('keydown',{key:'$1',bubbles:true,cancelable:true})); 'ok'"; }
o command id=fast-find:search
o eval code="const i=document.querySelector('.ff10k-input'); i.value='RW MLOps is mostly data engineering'; i.dispatchEvent(new Event('input')); 'ok'"
o dev:screenshot path=$S/f.png; K ArrowDown; o dev:screenshot path=$S/f.png
# similar rows: title ~ path ~ reason
o eval code="[...document.querySelectorAll('.ff10k-similar-row')].map(r=>[...r.children].map(c=>c.textContent).join(' ~ ')).join('\n')"
# preview: first visible line (heading hits scroll their heading to the top)
o eval code="const p=document.querySelector('.ff10k-preview'); const l=[...p.querySelectorAll('.ff10k-line')]; const t=l.find(x=>x.offsetTop-p.offsetTop>=p.scrollTop-1); l.indexOf(t)+' '+t.className+' '+t.textContent"
# coalescing: wrap docText/similar with counters, then 20 moves in one burst
o eval code="const e=app.plugins.plugins['fast-find'].engine; e.__o=e.docText; window.__c={doc:0}; e.docText=function(p){window.__c.doc++; return e.__o.call(this,p)}; for(let n=0;n<20;n++) document.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true,cancelable:true})); 'ok'"
o dev:screenshot path=$S/f.png; o eval code="JSON.stringify(window.__c)"
K Tab; K Enter                   # similar panel: focus first row, open it
```

Observed on the test vault, 2026-10-26:

| Check | Result |
|---|---|
| select `RW MLOps is mostly data engineering` | panel: `RW MLOps Is Mostly Data Engineering.` · `copy / punctuation variant`; footer `Smart Connections off: title similarity only` |
| select `rust data engineering` top hit | 4 rows: 3 × `same words, different order`, 1 × `similar title` |
| `troubleshooting`, h3 hit `### Troubleshooting` in Obsidian.md | preview top line = line 223, class `ff10k-h3`, text `Troubleshooting`; modal ancestors not scrolled |
| Ctrl+P | `ff10k-preview-hidden`, right column `display:none`, results 443 → 805 px (full body width); Ctrl+P again restores |
| 20 × ↓ in one burst | `docText` 1, `similar` 1 for 20 selection moves |
| key handler cost | 0.28 ms per move; preview paint incl. worker round trip 0.2–9.5 ms over 20 notes |
| Tab | first similar row `is-selected`, focus on `.ff10k-similar-list`; ↓ stays on the only row |
| Esc / Tab in panel | selection cleared, focus back on `.ff10k-input`, modal stays open |
| Tab, Enter | modal closed, active file `…/RW MLOps Is Mostly Data Engineering..md` |
| empty query (tier 0 `recent`) | panel `—`, 0 `similar` calls; preview renders (76 lines) |

Preview paint on the largest notes (`Preview.show` with the text cached, including the layout it forces,
3 runs each; after the review fix: per-line caps of 2,000 chars and 50 styled ranges, a linear
segmenter, and `content-visibility: auto` on lines from 200 below the scroll anchor):

| Note | Size | Before fix | After fix |
|---|---|---|---|
| `book proposal mindmap.excalidraw.md` (52,483 lines, 4,000 rendered) | 897 KB | 77–96 ms | 36–60 ms |
| `RW Tools of Titans.md` (660 lines, longest 3,093 chars) | 239 KB | 24–66 ms | 10–13 ms |
| `Productivity Expert … (Transcript).md` (5,000 lines, 4,000 rendered) | 149 KB | 40–42 ms | 29–31 ms |

What is left is DOM creation for 4,000 line divs, plus real layout of every line above the first
match when that match sits deep in the note. With a target at line 300 or 2,500 the line lands
0.1–0.2 px from the top. The lazy lines are all below the anchor, so the scroll stays exact.

Ghost rows take the same `—` path but cannot be checked in the app yet: ghosts are not fed to
the engine before the create-note task.

## Frecency, ghosts, create note (task 19)

Insert link (Alt+Enter) was removed in task 23 on owner request; its checks are gone from this section.

Frecency: every `file-open` of an indexed note appends a timestamp (seconds, newest 128 kept) to
the `access` store and the engine; an open through the modal is recorded by the modal (plus one
combo count in `queries` under the normalized query) and the `file-open` that follows within
500 ms is skipped. Ghosts: `metadataCache.unresolvedLinks` (targets without `#…`/`|…`, missing
attachments dropped, sources the indexer excludes skipped), refreshed after each indexer run and
on `metadataCache 'resolved'`, at most every 2 s, capped at 5,000 (one console line when capped).

```bash
# open notes outside the modal (fires file-open), then read the recent list
o eval code="const fs=app.vault.getMarkdownFiles().slice(0,3); (async()=>{for(const f of fs){await app.workspace.getLeaf(false).openFile(f); await new Promise(r=>setTimeout(r,300));}})(); 'ok'"
o command id=fast-find:search; o dev:screenshot path=$S/f.png
o eval code="[...document.querySelectorAll('.ff10k-row')].slice(0,4).map(r=>r.textContent).join('\n')"
# access store / combo store
o eval code="app.plugins.plugins['fast-find'].cache.getAccess().then(a=>window.__acc=a.map(x=>x.path+':'+x.ts.length)); 'ok'"
o eval code="JSON.stringify(window.__acc)"
o eval code="const r=indexedDB.open('ff10k/'+app.appId); r.onsuccess=()=>{const q=r.result.transaction('queries').objectStore('queries').getAll(); q.onsuccess=()=>window.__q=JSON.stringify(q.result)}; 'ok'"
o eval code="window.__q"
# create / guard: Shift+Enter, Ctrl+Shift+Enter
o eval code="document.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',shiftKey:true,bubbles:true,cancelable:true})); 'ok'"
o eval code="const s=document.querySelector('.ff10k-similar'); s.querySelector('.ff10k-similar-label').textContent+': '+[...s.querySelectorAll('.ff10k-similar-title')].map(e=>e.textContent).join(', ')"
```

Observed on the test vault, 2026-10-26 (6,097 ghost targets → capped to 5,000; top by refs:
`_The Bible (NKJV)` 71, `Writing` 50, `Why are we here on Earth` 32,
`🌵 My Values, Principles and Purpose` 31, `2025-01-14` 31):

| Check | Result |
|---|---|
| open 3 notes through the workspace, empty query | `recent`: the 3 notes, newest first, then the older recent ones |
| plugin reload, empty query | same 3 on top (access store → `setAccess` on start) |
| open a note through the modal (Enter) | access count 1 → 2 (the `file-open` is not counted again) |
| same query (`Why you  need an orchestrator`, mixed case/spaces) → same note 3× | `queries` count 3; debug `combo 1000.00` on the top row |
| `smart note taking` | first row ghost, italic, `link` badge, `Smart Note Taking · not created · linked from 27 notes` |
| Shift+Enter on it | panel `Already have?`: Process of Smart Note-Taking, RW How to Take Smart Notes…, How To Take Smart Notes-…; first row selected, focus in the panel |
| Esc | panel back to `similar notes · possible duplicates`, focus on the input, modal open |
| Shift+Enter, Shift+Enter (new-note location = folder `ff10k-new`) | `ff10k-new/Smart Note Taking.md` created and active, modal closed |
| 2 s later, `smart note taking` | 0 ghost rows; the new note is the first title row |
| `Index`, Shift+Enter (no note named Index yet) | guard `_index, LlamaIndex, CocoIndex`; Shift+Enter → `ff10k-new/Index.md` |
| `Index` again, folder now `ff10k-idx`, Shift+Enter then Ctrl+Shift+Enter | guard lists `ff10k-new/Index.md` first; `ff10k-idx/Index.md` created in a new tab (4 → 5 leaves) |
| `index`: both rows | `ff10k-idx/Index.md`, `ff10k-new/Index.md` (distinct paths) |
| guard on, Enter on a guard row | opens that existing note, nothing created |
| `createGuard` false, Shift+Enter | created at once |

All notes and folders created for these checks were deleted afterwards and the new-note location
restored (`root`).

Review fixes (task 19), 2026-10-26:

| Check | Result |
|---|---|
| plugin reload, `why you need an orchestrator` | `combo 1000.00` (the `queries` store is loaded into the engine on start) |
| ghost `ff10k-g/sub/Ghost Folder Note`, Shift+Enter | `ff10k-g/sub/Ghost Folder Note.md` created (folders made), the link resolves |
| ghost `ff10kdotnote2.md` | `ff10kdotnote2.md` (no `.md.md`) |
| ghost `../evil` | Notice `cannot create a note for the link "../evil"`, nothing created |
| `frecency.clear()` | 0 access, 0 queries rows; debug `frecency 0.00 · combo 0.00` |

Ghosts set by hand for a check (`engine.setGhosts([...])`) last until the next metadata
`resolved` refresh. A test ghost with 1 ref can fall outside the 5,000 cap on this vault.

## Smart Connections bridge, semantic tier (task 20)

`plugin/src/smart-connections-bridge.ts` is the only code that reads `window.smart_env` or the
Smart Connections plugin object. Property paths read live on Smart Connections 4.7.2:

| Path | Found |
|---|---|
| `app.plugins.plugins['smart-connections'].manifest.version` | `4.7.2` |
| `app.plugins.enabledPlugins.has('smart-connections')` | not reliable: the Lazy Plugin Loader starts it with `enablePlugin` and after a reload it is loaded (`_loaded true`) but missing from `enabledPlugins`, so the bridge accepts either |
| `smart_env.smart_sources.items` | plain object keyed by vault path (9,368 items) |
| `item.path` / `item.key` | vault path (`fixtures/all-forms.md`) |
| `item.vec` | getter (`collection.embeddings.get_item_vector`), `Float32Array(384)`, norm 1.0; 8,383 of 9,368 items have one; ~20 µs per read (~250 ms for all), so the bridge reads it in slices of 500 |
| `item.data.last_read.hash` | content hash (`1yxrmmy`) |
| `item.data.embedding.default` | `{ mf_ccarbz: { file, file_i, read_hash, at } }`: used with `last_read.hash` for the fingerprint (no getter, ~4 µs per item) |
| `smart_sources.embed_model.dims` / `.model_key` | `384` / `TaylorAI/bge-micro-v2` |
| `embed_model.embed_batch([{ embed_input, purpose: 'query' }])` | `[{ embed_input, purpose, vec: Float32Array(384), tokens }]`, ~7-10 ms |

Smart Connections loads after its items: `items` exist before `vec` is readable. The bridge polls
(1, 2, 4, 8, 16, 29 s) until the plugin is loaded and one of the first items with an embedding
reference has a `vec`, then syncs.

```bash
o plugin:reload id=fast-find
o dev:console | grep ff10k                       # → [ff10k] vectors: 8383 notes (dims 384, skipped 0)
o eval code="const b=app.plugins.plugins['fast-find'].bridge; b.available()+' '+b.hasVectors()"   # → true true
```

The CLI's window does not render frames here (`requestAnimationFrame` never fires, cursor-smith
reports "no frame"), so the preview/similar paint is triggered by hand for checks:
`app.plugins.plugins['fast-find'].modal.paint()`.

Test vault, 2026-10-26:

| Check | Result |
|---|---|
| plugin reload | `[ff10k] vectors: 8383 notes (dims 384, skipped 0)` |
| full vault reload (SC lazy-loaded "long") | vectors in the engine 34.8 s after reload, by the poll |
| `generated/note-201.md`: engine `similar` vs SC `find_connections({exclude_blocks_from_source_connections: true})` | note-060 0.9865 / 0.9865, note-002 0.9859 / 0.9859, note-028 0.9847 / 0.9847 (and a hand dot product gives the same) |
| SC pane default (blocks included) | lists block keys (`note-076.md#` 0.9843): different items, not different scores |
| select `idempotency-retry-pattern` | panel `idempotency 0.81, data-lineage 0.80, observability-monitoring-pattern 0.80, …`, hint hidden |
| `notes about making pipelines resilient` (3 content rows, then partial) | groups `content \| partial \| semantic`; `0.77 RW How to Design Resilient and Large Scale Data Systems`, `0.73 Restate`, `0.73 RW How to Build a Resilient DAG`, … |
| `duckdb` + Ctrl+S | `title · alias \| semantic`; `0.75 RW AI That Quacks …`, … |
| `obsidian trouble` + Ctrl+S | no group: the only neighbour ≥ 0.70 is already a lexical hit |
| embed + `semantic` for a new query | 7 ms + 1 ms |
| vector sync, unchanged | 54 ms wall, longest UI block 6 ms |
| vector sync, changed (full rebuild) | 408 ms wall, longest UI block 65 ms |
| guard `keeping pipelines resilient` (no title match) | `Already have?` `RW How to Design Resilient … [0.71]`, `Restate [0.71]` |
| guard `resilient data systems` | `RW How to Design Resilient … [title]` (the semantic hit is the same note: deduped) |
| SC disabled (`app.plugins.disablePlugin`), ff10k reloaded | no ff10k warnings or errors; no semantic group; Ctrl+S → Notice `Smart Connections embeddings not available`; hint `Smart Connections off: title similarity only` shown |
| SC re-enabled at runtime (`plugin:enable`) | SC itself is broken until an app reload (every `vec` getter throws `reading 'embedding_models'`); the bridge logs one `[ff10k] smart connections: reading vectors failed (…)` and stays off |
| `o reload` | SC back (lazy loader), vectors back, `available true`, `hasVectors true` |

Review fixes (task 20), 2026-10-26. The model was made to hang with
`smart_env.smart_sources.embed_model.embed_batch = () => new Promise(() => {})`, then restored:

| Check | Result |
|---|---|
| guard `keeping pipelines resilient` (normal model) | `RW How to Design Resilient … [0.71]`, `Restate [0.71]` |
| guard `data pipeline` (normal model) | 3 title/alias rows, then `Overview data pipelines, … [0.78]`, `RW Data Science for Startups Data Pipelines [0.78]` |
| hung model, guard `data orchestrators` | lexical rows at once (`Data Orchestrators [title]`, …); still 3 rows after 2 s |
| hung model, guard `zqyy second hung` (no title match, `semantic` onDemand) | note created after 1,535 ms (the 1.5 s embed timeout); one `[ff10k] smart connections: query embedding timed out …`; note deleted |
| `semantic` = never, guard `data pipeline` | title/alias rows only, `bridge.embed` called 0 times |

## Settings tab, why-ranked, preview keys, help, resume, open all (task 21)

The settings tab is driven through `app.setting` and its DOM (sliders/textarea fire `input`,
key fields validate on `blur`). Settings and the modal cannot be open together, so each change is
checked by reopening the search.

```bash
o eval code="app.setting.open(); app.setting.openTabById('fast-find'); 'ok'"
# a slider / textarea / key field by its setting name
o eval code="const c=app.setting.activeTab.containerEl; const it=[...c.querySelectorAll('.setting-item')].find(i=>i.querySelector('.setting-item-name')?.textContent==='Body weight'); const sl=it.querySelector('input[type=range]'); sl.value='0'; sl.dispatchEvent(new Event('input')); sl.dispatchEvent(new Event('change')); 'ok'"
o eval code="app.setting.close(); app.commands.executeCommandById('fast-find:search'); 'ok'"
# why ranked (Ctrl+/), help (Ctrl+Shift+/ arrives as key '?'), preview scroll, open all
o eval code="document.dispatchEvent(new KeyboardEvent('keydown',{key:'/',ctrlKey:true,bubbles:true,cancelable:true})); 'ok'"
o eval code="document.dispatchEvent(new KeyboardEvent('keydown',{key:'?',code:'Slash',keyCode:191,ctrlKey:true,shiftKey:true,bubbles:true,cancelable:true})); 'ok'"
o eval code="document.dispatchEvent(new KeyboardEvent('keydown',{key:'J',shiftKey:true,bubbles:true,cancelable:true})); document.querySelector('.ff10k-preview').scrollTop"
o eval code="document.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',altKey:true,shiftKey:true,bubbles:true,cancelable:true})); 'sent'"
o command id=fast-find:search-resume
```

An `eval` that triggers open all often prints nothing (the workspace changes under it); read the
result in a second call.

Observed on the test vault, 2026-10-26:

| Check | Result |
|---|---|
| settings tab | sections `Indexing \| Ranking \| Semantic \| Behaviour \| Keys \| Debug`, 10 sliders, 17 key fields |
| `hawaiian goddess`, body weight 10 | `content \| partial`, 16 rows, 1 `body` row |
| body weight → 0 (slider), reopen, same query | `partial` only, 15 rows, 0 `body` rows; `data.json` `body: 0`; Reset weights → 10 |
| `obsidian trouble` + Ctrl+/ | `tier 1 · obsidian→title exact 1.00 · trouble→title prefix 0.58 · fields +158 · proximity +25 · structural +3.6 · total 187` |
| downranked folder `SB_copy/💡 Resources/🗃 Zettelkasten/📬 Inbox` (textarea) | same row `… · penalty ×0.30 · total 56`; the Wiki `Obsidian` (other folder) unchanged at 154 |
| Clear frecency (button) | frecency map 0, IndexedDB `access` 0 rows; empty query falls back to mtime order; opening a note puts it back on top |
| ignored glob `generated/**` (textarea) | rebuild 1 s after the edit: 9,383 → 9,083 docs (the 300 in `generated/`); cleared → 9,383 again |
| Shift+J / Shift+K (`obsidian`, 3rd row, preview 631 px high) | scrollTop 9 → 325 → 9; event `defaultPrevented` true; plain `j` not handled (types into the input) |
| Shift+J with the preview hidden (Ctrl+P) | no scroll |
| Ctrl+? | overlay `Navigate / Open / Create & link / Modes & panels / Help`, e.g. `Ctrl+J / ↓ next result`, `Alt+Shift+⏎ open all shown in tabs (≤ 10)`, `Ctrl+? toggle this help`; Esc hides it and the modal stays; Ctrl+? again hides it |
| togglePreview rebound to `Ctrl+Shift+P` in settings | overlay row `Ctrl+Shift+P toggle preview`; Ctrl+Shift+P hides the preview; `Hyper+P` → red border, `invalid key …; keeping the previous binding`; `Ctrl+J` → `already used by moveDown`; Reset keys → `Ctrl+P` |
| close with `hawaiian goddess`, plugin reload | `lastQuery` `hawaiian goddess` in `data.json` |
| `search-resume` with preview off by default | input `hawaiian goddess`, preview shown, `content \| partial` 16 rows; plain `search` → empty input, preview hidden |
| open all, `obsidian trouble tags` (23 notes) | Notice `Fast Find 10K: Open all works for 10 or fewer results (23 shown)`, modal stays; footer hint hidden |
| open all, `quickshell` (8 notes, hint shown) | 8 new tabs in result order, active leaf unchanged, modal closed, each note +1 access; tabs closed after |

## Performance pass (task 22)

Numbers, method and before/after tables: [perf.md](perf.md). Native measurements go through
`ff10k profile <vault>` (ingest split, engine heap) and `ff10k bench <vault>`, pinned to a fast
core (`taskset -c 2`; this laptop mixes Zen 5 and Zen 5c cores). Worker memory in Obsidian:

```bash
o eval code="const w=app.plugins.plugins['fast-find'].engine.worker; w.addEventListener('message', ev=>{ if(ev.data.t==='stats' && ev.data.id===999999) window.__mem=ev.data }); w.postMessage({t:'stats', id:999999}); 'sent'"
o eval code="JSON.stringify(window.__mem)"          # wasmBytes = wasm linear memory (peak heap)
# engine time of a few queries (worker elapsedUs, includes JSON and the UTF-16 range rewrite)
o eval code="const e=app.plugins.plugins['fast-find'].engine; (async()=>{const out=[]; for (const q of ['data engineer ai','ai','obsidian troubelshoot']) { const r=await e.search(q,'all',50,false); out.push(q+':'+(r.elapsedUs/1000).toFixed(1)); } window.__t=out.join(' | ')})(); 'ok'"
o eval code="window.__t"
# rebuild, end to end
o eval code="window.__rb0=performance.now(); app.plugins.plugins['fast-find'].indexer.rebuild().then(()=>window.__rb=Math.round(performance.now()-window.__rb0)); 'sent'"
o eval code="window.__rb"
```

Observed on the test vault, 2026-10-26 (9,385 notes, Smart Connections vectors and 5,000 ghosts
loaded):

| Check | Result |
|---|---|
| plugin reload ×3 | `indexed 9385 docs in 1245 / 1215 / 1373 ms (warm: 9385 from cache, 0 re-read)` (was ~3,580) |
| Rebuild index (`indexer.rebuild()`) | 1,781 ms end to end; console `indexed 9385 docs in 1344 ms (warm: 0 from cache, 9385 re-read)` (was 7,619) |
| worker memory | `wasmBytes` 164,036,608 (was 181,534,720) |
| first query after reload, `data engineer ai` | 27 ms (was 59–68); repeated 13.5–15.7 ms |
| `ai` / `obsidian troubelshoot` / `path:resources` | 7.8 / 8.1 / 7.5 ms |
| typing `data engineerin` at 60 ms/char | 16/16 painted; engine 3.6–18.1 ms (median ≈ 10), render 2.8–5.3 ms |
| `obsidian trouble` | top hit `Obsidian Troubleshoot` |
| `dev:errors` | nothing from fast-find |
| `main.js` | 1,427,984 bytes |

## Release

`make release-notes VERSION=x.y.z` drafts `RELEASE_NOTES.md` from the commits since the last tag (review and commit it; the release refuses to run while the file is unchanged since the last tag), then `make release VERSION=x.y.z` does steps 1–3 and pushes the tag; the Release workflow (`.github/workflows/release.yml`) then builds the assets from the tag, attaches GitHub artifact attestations and creates or updates the release (`gh workflow run release.yml -f tag=x.y.z` re-runs it for an existing tag; `make release-local` publishes a local build without attestations). By hand:

1. Bump `version` in `manifest.json` (repo root) and `plugin/package.json`; add the version → `minAppVersion` line to `versions.json`.
2. `make build` → `plugin/dist/{main.js,manifest.json,styles.css}`.
3. Commit, tag the version without a `v` prefix (`git tag 0.1.0 && git push --tags`).
4. GitHub release named after the tag with the three files from `plugin/dist` attached as individual assets (not zipped).
5. Test on desktop and mobile through BRAT (`ssp-data/obsidian-fast-find-10k`), then submit to the community plugin list (<https://github.com/obsidianmd/obsidian-releases>): `manifest.json` and `versions.json` are in the repo root for that reason.
