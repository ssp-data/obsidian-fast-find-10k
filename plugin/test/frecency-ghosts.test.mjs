// Frecency tracking, unresolved-link ghosts and note-name sanitizing, with stubbed `app`/engine/cache.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

const obsidianStub = {
	name: 'obsidian-stub',
	setup(build) {
		build.onResolve({ filter: /^obsidian$/ }, () => ({ path: 'obsidian', namespace: 'stub' }));
		build.onLoad({ filter: /.*/, namespace: 'stub' }, () => ({
			contents: 'export class Notice { constructor(m) { globalThis.__notices?.push(m); } } export class MarkdownView {} export class TFile {} export const normalizePath = (p) => p;',
			loader: 'js',
		}));
	},
};

async function load(entry) {
	const built = await esbuild.build({ entryPoints: [entry], bundle: true, format: 'esm', write: false, plugins: [obsidianStub], absWorkingDir: new URL('..', import.meta.url).pathname });
	return import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));
}

const { sanitizeName, ghostNotePath } = await load('src/actions.ts');
const { collectGhosts, throttle } = await load('src/unresolved.ts');
const { FrecencyTracker } = await load('src/frecency.ts');

test('sanitizeName strips forbidden characters, collapses whitespace, trims', () => {
	assert.equal(sanitizeName('  AI Data Engineer Workflows  '), 'AI Data Engineer Workflows');
	assert.equal(sanitizeName('a\\b/c:d*e?f"g<h>i|j'), 'abcdefghij');
	assert.equal(sanitizeName('what   is\tthis?'), 'what is this');
	assert.equal(sanitizeName('trailing dots...'), 'trailing dots');
	assert.equal(sanitizeName('  ?? / ** '), '');
	assert.equal(sanitizeName(''), '');
	assert.equal(sanitizeName('x'.repeat(300)).length, 200);
	assert.equal(sanitizeName('Café Ölmühle (draft) #1'), 'Café Ölmühle (draft) #1');
	assert.equal(sanitizeName('. .'), '');
	assert.equal(sanitizeName('..'), '');
	assert.equal(sanitizeName('.notes'), 'notes');
	assert.equal(sanitizeName(' ...hidden. '), 'hidden');
});

test('ghostNotePath keeps folders, strips .md, refuses traversal', () => {
	assert.equal(ghostNotePath('folder/Note'), 'folder/Note.md');
	assert.equal(ghostNotePath('a/b c/Plan: v2?'), 'a/b c/Plan v2.md');
	assert.equal(ghostNotePath('Note.md'), 'Note.md');
	assert.equal(ghostNotePath('Note'), 'Note.md');
	assert.equal(ghostNotePath('/folder/Note'), 'folder/Note.md');
	assert.equal(ghostNotePath('../x'), null);
	assert.equal(ghostNotePath('a/./x'), null);
	assert.equal(ghostNotePath('a//x'), null);
	assert.equal(ghostNotePath('a/.hidden/x'), 'a/hidden/x.md');
	assert.equal(ghostNotePath('folder/'), null);
	assert.equal(ghostNotePath(''), null);
});

test('collectGhosts strips #heading and |alias and counts distinct source files', () => {
	const app = {
		metadataCache: {
			unresolvedLinks: {
				'a.md': { 'AI Data Engineer Workflows#Intro': 1, 'X|alias': 2 },
				'b.md': { 'AI Data Engineer Workflows': 1 },
			},
		},
	};
	assert.deepEqual(collectGhosts(app), [
		{ target: 'AI Data Engineer Workflows', refs: 2 },
		{ target: 'X', refs: 1 },
	]);
});

test('collectGhosts: one source linking a target twice counts once; empty targets skipped; ties by target', () => {
	const app = {
		metadataCache: {
			unresolvedLinks: {
				'a.md': { 'B#x': 1, B: 3, '#only': 1, ' |alias': 1 },
				'c.md': { A: 1 },
				'd.md': {},
			},
		},
	};
	assert.deepEqual(collectGhosts(app), [
		{ target: 'A', refs: 1 },
		{ target: 'B', refs: 1 },
	]);
});

test('collectGhosts skips sources the indexer excludes and attachment targets', () => {
	const app = {
		metadataCache: {
			unresolvedLinks: {
				'templates/daily.md': { '{{date}}': 1 },
				'a.md': { 'missing.png': 1, 'Plan v1.2': 1 },
			},
		},
	};
	assert.deepEqual(collectGhosts(app, (p) => p.startsWith('templates/')), [{ target: 'Plan v1.2', refs: 1 }]);
});

test('throttle runs once per window, trailing', async () => {
	let n = 0;
	const t = throttle(() => n++, 30);
	t.call();
	t.call();
	t.call();
	assert.equal(n, 0);
	await new Promise((r) => setTimeout(r, 50));
	assert.equal(n, 1);
	t.call();
	await new Promise((r) => setTimeout(r, 50));
	assert.equal(n, 2);
	t.call();
	t.cancel();
	await new Promise((r) => setTimeout(r, 50));
	assert.equal(n, 2);
});

function fakeTracker(access = [], queries = []) {
	const calls = { track: [], put: [], bump: [], set: [], combos: [], cleared: 0, engineCleared: 0 };
	const engine = {
		setAccess: (e) => calls.set.push(e),
		setCombos: (e) => calls.combos.push(e),
		clearFrecency: () => calls.engineCleared++,
		// stand-in for the engine's Query::parse(..).normalized
		normalizeQuery: async (q) => q.toLowerCase().split(/\s+/).filter(Boolean).join(' '),
		track: (path, now, query) => calls.track.push(query === undefined ? [path, now] : [path, now, query]),
	};
	const cache = {
		getAccess: async () => access,
		getQueries: async () => queries,
		putAccess: async (path, ts) => calls.put.push([path, ts]),
		bumpQuery: async (q, path) => (calls.bump.push([q, path]), 1),
		clearAccess: async () => calls.cleared++,
	};
	const clock = { ms: 1_000_000 };
	const app = { workspace: { on: () => ({}), offref: () => {} } };
	const tracker = new FrecencyTracker(app, engine, cache, (p) => !p.endsWith('.md'), () => clock.ms);
	return { tracker, calls, clock };
}

test('start loads the access and combo stores into the engine', async () => {
	const { tracker, calls } = fakeTracker([{ path: 'a.md', ts: [1, 2] }], [{ query: 'de', path: 'a.md', count: 4 }]);
	await tracker.start();
	assert.deepEqual(calls.set, [[{ path: 'a.md', ts: [1, 2] }]]);
	assert.deepEqual(calls.combos, [[{ query: 'de', path: 'a.md', count: 4 }]]);
});

test('a file-open within 500 ms of a modal open of the same note is not counted twice', async () => {
	const { tracker, calls, clock } = fakeTracker();
	await tracker.start();
	await tracker.noteModalOpen('a.md', 'AI  Data ');
	clock.ms += 100;
	await tracker.recordFileOpen('a.md');
	assert.equal(calls.track.length, 1);
	assert.deepEqual(calls.track[0], ['a.md', 1000, 'ai data']);
	assert.deepEqual(calls.bump, [['ai data', 'a.md']]);
	clock.ms += 600;
	await tracker.recordFileOpen('a.md');
	assert.equal(calls.track.length, 2);
	assert.deepEqual(calls.put.at(-1), ['a.md', [1000, 1000.7]]);
});

test('a file-open of another note right after a modal open is counted', async () => {
	const { tracker, calls } = fakeTracker();
	await tracker.start();
	await tracker.noteModalOpen('a.md', '');
	await tracker.recordFileOpen('b.md');
	assert.deepEqual(calls.track.map((c) => c[0]), ['a.md', 'b.md']);
	assert.deepEqual(calls.bump, []); // empty query: no combo
});

test('excluded paths are not tracked; history keeps the newest 128', async () => {
	const { tracker, calls, clock } = fakeTracker([{ path: 'a.md', ts: Array.from({ length: 128 }, (_, i) => i) }]);
	await tracker.start();
	await tracker.recordFileOpen('img.png');
	assert.equal(calls.track.length, 0);
	await tracker.recordFileOpen('a.md');
	const [, ts] = calls.put.at(-1);
	assert.equal(ts.length, 128);
	assert.equal(ts[0], 1);
	assert.equal(ts.at(-1), clock.ms / 1000);
});

test('clear wipes the stores and sends clearFrecency to the engine', async () => {
	const { tracker, calls } = fakeTracker([{ path: 'a.md', ts: [1] }]);
	await tracker.start();
	await tracker.recordFileOpen('b.md');
	await tracker.clear();
	assert.equal(calls.cleared, 1);
	assert.equal(calls.engineCleared, 1);
	await tracker.recordFileOpen('a.md');
	assert.deepEqual(calls.put.at(-1), ['a.md', [1000]]); // history starts over
});

test('a modal open whose query normalizes to nothing records no combo', async () => {
	const { tracker, calls } = fakeTracker();
	await tracker.start();
	await tracker.noteModalOpen('a.md', '   ');
	assert.deepEqual(calls.track, [['a.md', 1000]]);
	assert.deepEqual(calls.bump, []);
});

test('suppressNext: the next file-open of that path within 500 ms is skipped, once', async () => {
	const { tracker, calls, clock } = fakeTracker();
	await tracker.start();
	tracker.suppressNext('a.md');
	await tracker.recordFileOpen('b.md'); // another path: counted, suppression kept
	clock.ms += 100;
	await tracker.recordFileOpen('a.md'); // consumed
	await tracker.recordFileOpen('a.md'); // counted again
	assert.deepEqual(calls.track.map((c) => c[0]), ['b.md', 'a.md']);
	tracker.suppressNext('a.md');
	clock.ms += 600;
	await tracker.recordFileOpen('a.md'); // too late: counted
	assert.deepEqual(calls.track.map((c) => c[0]), ['b.md', 'a.md', 'a.md']);
});
