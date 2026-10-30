// Indexer.isExcluded and the glob helper, with a stubbed `app` (no Obsidian runtime).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

/** Replaces the `obsidian` module (types only at runtime) with empty classes. */
const obsidianStub = {
	name: 'obsidian-stub',
	setup(build) {
		build.onResolve({ filter: /^obsidian$/ }, () => ({ path: 'obsidian', namespace: 'stub' }));
		build.onLoad({ filter: /.*/, namespace: 'stub' }, () => ({ contents: 'export class TFile {}', loader: 'js' }));
	},
};

const built = await esbuild.build({ entryPoints: ['src/indexer.ts'], bundle: true, format: 'esm', write: false, plugins: [obsidianStub], absWorkingDir: new URL('..', import.meta.url).pathname });
const { Indexer, globToRegExp } = await import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));

const app = { metadataCache: { isUserIgnored: (p) => p.startsWith('Private/') } };
const indexer = new Indexer(app, {}, {}, { ignoredGlobs: ['templates/**'] });

test('excluded paths', () => {
	for (const p of ['.obsidian/x.md', 'a/b.png', '.smart-env/y.md', 'a/.trash/z.md', 'Private/secret.md', 'templates/daily.md', 'templates/sub/t.md']) {
		assert.equal(indexer.isExcluded(p), true, p);
	}
});

test('kept paths', () => {
	for (const p of ['💡 Resources/x.md', 'Showcase.md', 'a.b/c.md', 'my templates/x.md', 'Coffee/Coffee Beans Base.base', 'book.canvas', 'venn.excalidraw']) {
		assert.equal(indexer.isExcluded(p), false, p);
	}
});

test('glob helper', () => {
	const m = (g, p) => globToRegExp(g).test(p);
	assert.equal(m('templates/**', 'templates/a.md'), true);
	assert.equal(m('templates/**', 'templates/x/y/a.md'), true);
	assert.equal(m('templates/**', 'other/templates/a.md'), false);
	assert.equal(m('*.excalidraw.md', 'Drawing.excalidraw.md'), true);
	assert.equal(m('*.excalidraw.md', 'deep/dir/Drawing.excalidraw.md'), true);
	assert.equal(m('*.excalidraw.md', 'Drawing.md'), false);
	assert.equal(m('Archive/*', 'Archive/old.md'), true);
	assert.equal(m('Archive/*', 'Archive/sub/old.md'), false);
	assert.equal(m('a/**/b.md', 'a/b.md'), true);
	assert.equal(m('a/**/b.md', 'a/x/y/b.md'), true);
	assert.equal(m('(x)+.md', '(x)+.md'), true);
});

// --- start()/rebuild() with stubbed vault, cache and engine ---

function file(path, mtime = 1000) {
	return { path, extension: 'md', stat: { mtime, size: 10 } };
}

function harness(files, cachedRead) {
	const upserts = [];
	const puts = [];
	const engine = {
		docs: new Set(),
		onProgress() {},
		upsert(docs) {
			for (const d of docs) {
				upserts.push(d);
				this.docs.add(d.path);
			}
		},
		removed: 0,
		cleared: 0,
		remove(paths) {
			this.removed += paths.length;
			for (const p of paths) this.docs.delete(p);
		},
		clear() {
			this.cleared++;
			this.docs.clear();
		},
		stats() {
			return Promise.resolve(this.docs.size);
		},
	};
	const cache = {
		failClear: false,
		streamAll: async () => {},
		put: async (rows) => void puts.push(...rows),
		delete: async () => {},
		clear: async () => {
			if (cache.failClear) throw new Error('clear failed');
		},
	};
	const app = {
		metadataCache: { isUserIgnored: () => false },
		workspace: { onLayoutReady: (cb) => cb() },
		vault: {
			getMarkdownFiles: () => files,
			getFiles: () => files.map((f) => ({ extension: f.path.split('.').pop(), ...f })),
			getFileByPath: (p) => files.find((f) => f.path === p) ?? null,
			cachedRead,
		},
	};
	const ix = new Indexer(app, engine, cache, { ignoredGlobs: [] });
	return { ix, engine, cache, upserts, puts };
}

const quiet = async (fn) => {
	const { error, log } = console;
	console.error = console.log = () => {};
	try {
		return await fn();
	} finally {
		Object.assign(console, { error, log });
	}
};

test('one failed read does not drop the rest of the batch', async () => {
	const files = [file('a.md'), file('b.md'), file('c.md')];
	const h = harness(files, async (f) => {
		if (f.path === 'b.md') throw new Error('ENOENT');
		return `text ${f.path}`;
	});
	await quiet(() => h.ix.start());
	assert.deepEqual(h.upserts.map((d) => d.path).sort(), ['a.md', 'c.md']);
	assert.equal(h.ix.status.done, true);
});

test('a failed cache.put still upserts and finishes', async () => {
	const h = harness([file('a.md')], async () => 'x');
	h.cache.put = async () => {
		throw new Error('QuotaExceededError');
	};
	await quiet(() => h.ix.start());
	assert.deepEqual(h.upserts.map((d) => d.path), ['a.md']);
	assert.equal(h.ix.status.done, true);
});

test('rebuild still runs after a failed run', async () => {
	const h = harness([file('a.md')], async () => 'x');
	h.cache.streamAll = async () => {
		throw new Error('idb broken');
	};
	await quiet(() => h.ix.start()).catch(() => {});
	assert.equal(h.ix.status.done, true);
	await quiet(() => h.ix.rebuild());
	assert.deepEqual(h.upserts.map((d) => d.path), ['a.md']);
	assert.equal(h.ix.status.done, true);
	assert.equal(h.ix.status.indexed, 1);
});

test('rebuild clears the engine at once instead of removing doc by doc', async () => {
	const files = [file('a.md'), file('b.md')];
	const h = harness(files, async (f) => `text ${f.path}`);
	await quiet(() => h.ix.start());
	h.engine.docs.add('stale.md');
	await quiet(() => h.ix.rebuild());
	assert.equal(h.engine.cleared, 1);
	assert.equal(h.engine.removed, 0);
	assert.deepEqual([...h.engine.docs].sort(), ['a.md', 'b.md']);
	assert.equal(h.ix.status.indexed, 2);
});

test('a late startup read does not overwrite a fresher modify flush', async () => {
	const p = file('p.md', 1000);
	let releaseStale;
	let calls = 0;
	const h = harness([p], (f) => {
		calls++;
		if (calls === 1) return new Promise((r) => (releaseStale = () => r('stale')));
		return Promise.resolve('fresh');
	});
	const started = quiet(() => h.ix.start());
	await new Promise((r) => setTimeout(r, 0));
	p.stat.mtime = 2000; // modified on disk
	await h.ix.flush('p.md', 'upsert');
	releaseStale();
	await started;
	const last = (arr) => arr.filter((d) => d.path === 'p.md').at(-1);
	assert.equal(last(h.upserts).text, 'fresh');
	assert.equal(last(h.puts).text, 'fresh');
	assert.equal(last(h.puts).mtime, 2);
});
