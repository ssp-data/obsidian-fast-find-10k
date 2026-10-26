import init, { WasmEngine } from '../crates/ff10k-wasm/pkg/ff10k_wasm.js';
import { readFileSync } from 'node:fs';

const bytes = readFileSync(new URL('../crates/ff10k-wasm/pkg/ff10k_wasm_bg.wasm', import.meta.url));
const t0 = performance.now();
await init({ module_or_path: bytes });
console.error(`init: ${(performance.now() - t0).toFixed(1)} ms`);

const e = new WasmEngine();
e.upsert('a/Obsidian Troubleshooting.md', 1, '# x');
e.upsert('a/Obsidian Setup.md', 1, '# y\nobsidian troubleshooting setup notes');
const res = JSON.parse(e.search('obsidian trouble', JSON.stringify({ mode: 'all', limit: 10, nowSecs: 0, debug: false })));
console.log(res.hits[0].title);
console.log('doc_count', e.doc_count());
e.set_vectors(2, JSON.stringify(['a/Obsidian Troubleshooting.md', 'a/Obsidian Setup.md']), new Float32Array([1, 0, 0.9, 0.1]));
console.log('has_vectors', e.has_vectors());
console.log('similar', e.similar('a/Obsidian Troubleshooting.md', 5, 0));
console.log('semantic', e.semantic(new Float32Array([1, 0]), 5, 0));
console.log('bad opts', e.search('x', '{'));
