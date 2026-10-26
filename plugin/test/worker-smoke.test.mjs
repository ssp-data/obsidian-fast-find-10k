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
	const waitFor = (pred) =>
		new Promise((resolve) => {
			const check = () => {
				const r = replies.find(pred);
				if (r) resolve(r);
				else setTimeout(check, 5);
			};
			check();
		});
	w.on('message', (m) => replies.push(m));
	try {
		w.postMessage({ t: 'init', id: 1, config });
		w.postMessage({ t: 'upsert', docs: [{ path: 'a/Obsidian Troubleshooting.md', mtime: 1, text: '# x' }] });
		w.postMessage({ t: 'search', id: 2, query: 'obsidian', mode: 'all', limit: 10, debug: false });
		w.postMessage({ t: 'search', id: 3, query: 'obsidian trouble', mode: 'all', limit: 10, debug: false });
		w.postMessage({ t: 'ghosts', ghosts: 'not an array' });
		w.postMessage({ t: 'vectors', dims: 3, paths: ['a'], data: new Float32Array([1, 2]) });
		w.postMessage({ t: 'stats', id: 4 });

		assert.deepEqual(await waitFor((m) => m.t === 'ready'), { t: 'ready', id: 1 });
		assert.deepEqual(await waitFor((m) => m.t === 'upserted'), { t: 'upserted', count: 1, total: 1 });
		assert.equal((await waitFor((m) => m.t === 'results' && m.id === 2)).stale, true);
		const res = (await waitFor((m) => m.t === 'results' && m.id === 3)).res;
		assert.equal(res.hits[0].title, 'Obsidian Troubleshooting');
		assert.ok(res.elapsedUs > 0, `elapsedUs measured: ${res.elapsedUs}`);
		assert.equal(typeof (await waitFor((m) => m.t === 'error')).message, 'string');
		assert.equal((await waitFor((m) => m.t === 'vectorsSet')).ok, false);
		assert.deepEqual(await waitFor((m) => m.t === 'stats'), { t: 'stats', id: 4, docs: 1 });
	} finally {
		await w.terminate();
	}
});
