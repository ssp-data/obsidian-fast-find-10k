// Runs the real worker bundle in a Node worker thread with a tiny `self`/`postMessage` shim.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Worker } from 'node:worker_threads';
import { buildWorker } from '../esbuild.config.mjs';

const SHIM = `
const { parentPort } = require('node:worker_threads');
globalThis.self = globalThis;
globalThis.postMessage = (m) => parentPort.postMessage(m);
parentPort.on('message', (data) => globalThis.onmessage({ data }));
`;

const config = {
	weights: { title: 100, alias: 90, h1: 60, h2: 50, h3: 40, tag: 30, link: 20, path: 15, body: 10 },
	fuzzy: { enabled: true, loose: false },
	partial: 'whenFew',
	downrankedFolders: [],
	downrankFactor: 0.3,
	fewThreshold: 5,
};

test('worker bundle answers init, upsert, search, stale and errors', async () => {
	const w = new Worker(SHIM + (await buildWorker()), { eval: true });
	const replies = [];
	const waitFor = (pred, ms = 2000) =>
		new Promise((resolve, reject) => {
			const deadline = Date.now() + ms;
			const check = () => {
				const r = replies.find(pred);
				if (r) resolve(r);
				else if (Date.now() > deadline) reject(new Error(`no matching reply within ${ms} ms`));
				else setTimeout(check, 5);
			};
			check();
		});
	w.on('message', (m) => replies.push(m));
	try {
		w.postMessage({ t: 'init', id: 1, config });
		w.postMessage({
			t: 'upsert',
			docs: [
				{ path: 'a/Obsidian Troubleshooting.md', mtime: 1, text: '# x' },
				{ path: 'b/Søren Café Notes.md', mtime: 1, text: '💡 Grüße zum airflow scheduler' },
			],
		});
		w.postMessage({ t: 'search', id: 2, query: 'obsidian', mode: 'all', limit: 10, debug: false });
		w.postMessage({ t: 'search', id: 3, query: 'obsidian trouble', mode: 'all', limit: 10, debug: false });
		w.postMessage({ t: 'ghosts', ghosts: 'not an array' });
		w.postMessage({ t: 'vectors', dims: 3, paths: ['a'], data: new Float32Array([1, 2]) });
		w.postMessage({ t: 'stats', id: 4 });

		assert.deepEqual(await waitFor((m) => m.t === 'ready'), { t: 'ready', id: 1 });
		assert.deepEqual(await waitFor((m) => m.t === 'upserted'), { t: 'upserted', count: 2, total: 2 });
		assert.equal((await waitFor((m) => m.t === 'results' && m.id === 2)).stale, true);
		const res = (await waitFor((m) => m.t === 'results' && m.id === 3)).res;
		assert.equal(res.hits[0].title, 'Obsidian Troubleshooting');
		assert.ok(res.elapsedUs > 0, `elapsedUs measured: ${res.elapsedUs}`);
		assert.equal(typeof (await waitFor((m) => m.t === 'error')).message, 'string');
		assert.equal((await waitFor((m) => m.t === 'vectorsSet')).ok, false);
		const stats = await waitFor((m) => m.t === 'stats');
		assert.deepEqual([stats.id, stats.docs], [4, 2]);
		assert.ok(stats.wasmBytes >= 1 << 20, `wasm memory reported: ${stats.wasmBytes}`);

		// Ranges arrive as UTF-16 offsets: 'ø' is 2 UTF-8 bytes, '💡' and 'ü'/'ß' shift the excerpt.
		w.postMessage({ t: 'search', id: 5, query: 'cafe', mode: 'all', limit: 10, debug: false });
		const title = (await waitFor((m) => m.t === 'results' && m.id === 5)).res.hits[0];
		assert.equal(title.titleNorm, 'søren cafe notes');
		assert.deepEqual(title.titleRanges.map(([a, b]) => title.titleNorm.slice(a, b)), ['cafe']);
		w.postMessage({ t: 'search', id: 6, query: 'scheduler', mode: 'all', limit: 10, debug: false });
		const body = (await waitFor((m) => m.t === 'results' && m.id === 6)).res.hits[0];
		assert.ok(body.excerpt, 'body hit has an excerpt');
		assert.deepEqual(body.excerpt.ranges.map(([a, b]) => body.excerpt.text.slice(a, b)), ['scheduler']);

		w.postMessage({ t: 'clear' });
		w.postMessage({ t: 'stats', id: 7 });
		assert.equal((await waitFor((m) => m.t === 'stats' && m.id === 7)).docs, 0);

		// After the idle warm-up has run, the engine still answers normally.
		w.postMessage({ t: 'upsert', docs: [{ path: 'c/Warm Note.md', mtime: 1, text: '# warm' }] });
		await new Promise((r) => setTimeout(r, 250));
		w.postMessage({ t: 'search', id: 8, query: 'warm', mode: 'all', limit: 10, debug: false });
		assert.equal((await waitFor((m) => m.t === 'results' && m.id === 8)).res.hits[0].title, 'Warm Note');
		assert.equal(replies.filter((m) => m.t === 'error').length, 1, 'only the bad ghosts message errored');
	} finally {
		await w.terminate();
	}
});
