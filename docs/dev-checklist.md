# Dev checklist: build, install, reload, verify

Test vault: `/home/sspaeti/git/obsidian/trouble-tags-testvault` (never the real vault).
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
V=/home/sspaeti/git/obsidian/trouble-tags-testvault/.obsidian/plugins/fast-find-10k
mkdir -p $V && ln -sf $PWD/dist/main.js $PWD/dist/manifest.json $PWD/dist/styles.css $V/
```

## Enable and reload

```bash
o() { obsidian vault=trouble-tags-testvault "$@"; }
o eval code="app.plugins.loadManifests().then(()=>0)"   # pick up a newly symlinked plugin
o plugin:enable id=fast-find-10k filter=community        # once
o plugin:reload id=fast-find-10k                         # after every build
o plugin id=fast-find-10k                                # shows "enabled true"
```

## Verify

```bash
o dev:debug on                    # start capturing console messages
o dev:console clear
o plugin:reload id=fast-find-10k
o command id=fast-find-10k:search
o eval code="[...document.querySelectorAll('.notice')].map(n=>n.textContent).join(' | ')"
                                  # → "Fast Find 10K: 9382/9382 indexed, top "Obsidian Troubleshoot""
o dev:console level=error         # nothing from fast-find-10k
o dev:errors                      # nothing from fast-find-10k
o dev:debug off
```

`eval` does not await promises; to check an async result, stash it on `window` and read it in a
second call:

```bash
o eval code="const e=app.plugins.plugins['fast-find-10k'].engine; e.upsert([{path:'x/Obsidian Troubleshooting.md',mtime:1,text:'# x'}]); window.__a=e.search('obs','all',5,false).then(()=>'resolved',x=>x); window.__b=e.search('obsidian trouble','all',5,false).then(r=>r.hits[0].title+' '+r.elapsedUs); Promise.all([window.__a,window.__b]).then(v=>window.__ff=v); 'sent'"
o eval code="JSON.stringify(window.__ff)"   # → ["stale","Obsidian Troubleshooting <elapsedUs>"]
o eval code="app.plugins.plugins['fast-find-10k'].engine.remove(['x/Obsidian Troubleshooting.md']); 'ok'"
```

## Indexing (cache + indexer)

The console always logs one line per full run:
`[ff10k] indexed N docs in X ms (warm: Y from cache, Z re-read)`. Live status:
`app.plugins.plugins['fast-find-10k'].indexer.status` → `{indexed, total, done, elapsedMs}`.
Cache: IndexedDB `ff10k/<appId>` (stores `notes`, `access`, `queries`, `meta`).

```bash
o command id=fast-find-10k:rebuild-index      # clear note cache, re-read every file (cold)
o dev:console | grep ff10k                    # → indexed 9382 docs ... (warm: 0 from cache, 9382 re-read)
o plugin:reload id=fast-find-10k              # warm start
o dev:console | grep ff10k                    # → ... (warm: 9382 from cache, 0 re-read)
# onload → done, measured around disable/enable:
o eval code="app.plugins.disablePlugin('fast-find-10k').then(()=>{window.__t1=performance.now(); return app.plugins.enablePlugin('fast-find-10k')}).then(()=>{const p=app.plugins.plugins['fast-find-10k']; const iv=setInterval(()=>{ if(p.indexer?.status.done){clearInterval(iv); window.__r=Math.round(performance.now()-window.__t1)} },5)}); 'sent'"
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
o eval code="app.plugins.plugins['fast-find-10k'].settings.logTimings=true; 'ok'"   # session only
o command id=fast-find-10k:search                 # or search-files (opens in files-only mode)
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
| Tab / Shift+Enter / Alt+Enter / Ctrl+S | Notice "coming in a later task" |
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
o command id=fast-find-10k:search
o eval code="const i=document.querySelector('.ff10k-input'); i.value='RW MLOps is mostly data engineering'; i.dispatchEvent(new Event('input')); 'ok'"
o dev:screenshot path=$S/f.png; K ArrowDown; o dev:screenshot path=$S/f.png
# similar rows: title ~ path ~ reason
o eval code="[...document.querySelectorAll('.ff10k-similar-row')].map(r=>[...r.children].map(c=>c.textContent).join(' ~ ')).join('\n')"
# preview: first visible line (heading hits scroll their heading to the top)
o eval code="const p=document.querySelector('.ff10k-preview'); const l=[...p.querySelectorAll('.ff10k-line')]; const t=l.find(x=>x.offsetTop-p.offsetTop>=p.scrollTop-1); l.indexOf(t)+' '+t.className+' '+t.textContent"
# coalescing: wrap docText/similar with counters, then 20 moves in one burst
o eval code="const e=app.plugins.plugins['fast-find-10k'].engine; e.__o=e.docText; window.__c={doc:0}; e.docText=function(p){window.__c.doc++; return e.__o.call(this,p)}; for(let n=0;n<20;n++) document.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true,cancelable:true})); 'ok'"
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
