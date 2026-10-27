import init, { WasmEngine, normalize_query } from '../crates/ff10k-wasm/pkg/ff10k_wasm.js';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const bytes = readFileSync(new URL('../crates/ff10k-wasm/pkg/ff10k_wasm_bg.wasm', import.meta.url));
const t0 = performance.now();
await init({ module_or_path: bytes });
console.error(`init: ${(performance.now() - t0).toFixed(1)} ms`);

const e = new WasmEngine();
e.upsert('a/Obsidian Troubleshooting.md', 1, '# x');
e.upsert('a/Obsidian Setup.md', 1, '# y\nobsidian troubleshooting setup notes');
const res = JSON.parse(e.search('obsidian trouble', JSON.stringify({ mode: 'all', limit: 10, nowSecs: 0, debug: false })));
assert.equal(res.hits[0].title, 'Obsidian Troubleshooting');
assert.equal(res.hits[0].tier, 1);
assert.equal(res.hits[0].badge, 'Title');
console.log(res.hits[0].title);
assert.equal(e.doc_count(), 2);

e.set_vectors(2, JSON.stringify(['a/Obsidian Troubleshooting.md', 'a/Obsidian Setup.md']), new Float32Array([1, 0, 0.9, 0.1]));
assert.equal(e.has_vectors(), true);
for (const json of [e.similar('a/Obsidian Troubleshooting.md', 5, 0), e.semantic(new Float32Array([1, 0]), 5, 0)]) {
  const arr = JSON.parse(json);
  assert.ok(Array.isArray(arr) && arr.length > 0, json);
  assert.equal(typeof arr[0].path, 'string');
  assert.equal(typeof arr[0].score, 'number');
}
assert.equal(typeof JSON.parse(e.search('x', '{')).error, 'string');
assert.throws(() => e.set_ghosts('not json'));
assert.throws(() => e.set_access('a', 'not json'));
assert.throws(() => e.upsert_batch('not json'));
assert.equal(e.upsert_batch('[{"path":"b.md","mtime":1,"text":"z"}]'), 1);
assert.equal(normalize_query('  Café   ÖLMÜHLE  -x #tag '), 'cafe olmuhle');
e.record_query_open('obsidian trouble', 'a/Obsidian Setup.md');
e.record_access('a/Obsidian Setup.md', 100);
e.clear_frecency();
console.log('smoke ok');
