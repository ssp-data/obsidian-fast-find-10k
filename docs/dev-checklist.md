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
                                  # → "Fast Find 10K ready, 0 docs"
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

Pre-existing errors from other plugins in the test vault (smart-connections "Ribbon actions require
converged env.config.actions", cursor-smith watchdog) are unrelated.
