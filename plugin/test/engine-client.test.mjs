// EngineClient against a browser-like Worker shim backed by worker_threads.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Worker as NodeWorker } from 'node:worker_threads';
import { resolveObjectURL } from 'node:buffer';
import esbuild from 'esbuild';
import { buildWorker } from '../esbuild.config.mjs';

const SHIM = `
const { parentPort } = require('node:worker_threads');
globalThis.self = globalThis;
globalThis.postMessage = (m) => parentPort.postMessage(m);
parentPort.on('message', (data) => globalThis.onmessage({ data }));
`;

/** Minimal web `Worker`: loads the blob URL's text and runs it in a worker thread. */
class ShimWorker {
	onmessage = null;
	onerror = null;
	onmessageerror = null;
	#queue = [];
	#w = null;
	constructor(url) {
		resolveObjectURL(url).text().then((src) => {
			this.#w = new NodeWorker(SHIM + src, { eval: true });
			this.#w.on('message', (data) => this.onmessage?.({ data }));
			this.#w.on('error', (e) => this.onerror?.({ message: e.message }));
			for (const m of this.#queue.splice(0)) this.#w.postMessage(m);
		});
	}
	postMessage(m) {
		if (this.#w) this.#w.postMessage(m);
		else this.#queue.push(m);
	}
	terminate() {
		this.#w?.terminate();
	}
}
globalThis.Worker = ShimWorker;

const built = await esbuild.build({ entryPoints: ['src/engine-client.ts'], bundle: true, format: 'esm', write: false, absWorkingDir: new URL('..', import.meta.url).pathname });
const { EngineClient } = await import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));

const config = {
	weights: { title: 100, alias: 90, h1: 60, h2: 50, h3: 40, tag: 30, link: 20, path: 15, body: 10 },
	fuzzy: { enabled: true, loose: false },
	partial: 'whenFew',
	downrankedFolders: [],
	downrankFactor: 0.3,
	fewThreshold: 5,
};

const within = (p, ms) => Promise.race([p, new Promise((_, rej) => setTimeout(() => rej(new Error(`not settled within ${ms} ms`)), ms))]);

test('init rejects when the worker script throws at top level, later calls reject at once', async () => {
	const logged = [];
	const orig = console.error;
	console.error = (...a) => logged.push(a.join(' '));
	const c = new EngineClient("throw new Error('boom')");
	try {
		await assert.rejects(within(c.init(config), 1000), /boom/);
	} finally {
		console.error = orig;
	}
	assert.match(logged.join('\n'), /worker error: boom/);
	await assert.rejects(within(c.search('x', 'all', 5, false), 100), /boom/);
	assert.equal(await within(c.setVectors(1, ['a'], new Float32Array([1])), 100), false);
	c.dispose();
});

test('search after dispose rejects with disposed', async () => {
	const c = new EngineClient(await buildWorker());
	await within(c.init(config), 2000);
	c.dispose();
	await assert.rejects(within(c.search('x', 'all', 5, false), 100), (e) => e === 'disposed');
});
