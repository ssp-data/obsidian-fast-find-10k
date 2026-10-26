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
