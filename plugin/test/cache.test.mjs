// NoteCache against a stubbed `indexedDB` (Node has none): open failure recovery and the
// in-memory fallback.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

const built = await esbuild.build({ entryPoints: ['src/cache.ts'], bundle: true, format: 'esm', write: false, absWorkingDir: new URL('..', import.meta.url).pathname });
const { NoteCache, CACHE_SCHEMA } = await import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));

/** An IDBRequest stand-in that settles on a later tick. */
function request(settle) {
	const r = { result: undefined, error: null, onsuccess: null, onerror: null, onupgradeneeded: null, onblocked: null };
	setTimeout(() => settle(r), 0);
	return r;
}
const succeed = (value) => (r) => {
	r.result = value;
	r.onsuccess?.();
};
const fail = (msg) => (r) => {
	r.error = new Error(msg);
	r.onerror?.();
};

/** A tiny in-memory database: `meta` holds the schema, `notes` the given rows. */
function fakeDb(notes = []) {
	const stores = { meta: new Map([['schema', { key: 'schema', value: CACHE_SCHEMA }]]), notes: new Map(notes.map((n) => [n.path, n])) };
	return {
		closed: false,
		close() {
			this.closed = true;
		},
		transaction() {
			const tx = { oncomplete: null, onerror: null, onabort: null, error: null, aborted: false };
			tx.abort = () => {
				tx.aborted = true;
				setTimeout(() => tx.onabort?.(), 0);
			};
			tx.objectStore = (name) => ({
				get: (key) => request(succeed(stores[name].get(key))),
				openCursor: () => {
					const rows = [...stores[name].values()];
					let i = 0;
					const r = request(() => {});
					const step = () => {
						if (tx.aborted) return;
						r.result = i < rows.length ? { value: rows[i], continue: () => setTimeout(step, 0) } : null;
						i++;
						r.onsuccess?.();
					};
					setTimeout(step, 0);
					return r;
				},
			});
			return tx;
		},
	};
}

function stubIndexedDb(opens) {
	const calls = { open: 0, deleted: [] };
	globalThis.indexedDB = {
		open: () => request(opens[calls.open++] ?? fail('no more opens')),
		deleteDatabase: (name) => {
			calls.deleted.push(name);
			return request(succeed(undefined));
		},
	};
	return calls;
}

function captureWarn() {
	const warns = [];
	const orig = console.warn;
	console.warn = (...a) => warns.push(a.join(' '));
	return { warns, restore: () => (console.warn = orig) };
}

test('open: a failing open deletes the database and reopens once', async () => {
	const db = fakeDb();
	const calls = stubIndexedDb([fail('corrupt LevelDB'), succeed(db)]);
	const w = captureWarn();
	try {
		const cache = await NoteCache.open('app1');
		assert.equal(cache.persistent, true);
		assert.deepEqual(calls.deleted, ['ff10k/app1']);
		assert.equal(calls.open, 2);
		assert.equal(w.warns.length, 1, w.warns.join('\n'));
		assert.match(w.warns[0], /corrupt LevelDB/);
	} finally {
		w.restore();
	}
});

test('open: when the reopen fails too, an in-memory no-op cache keeps the plugin working', async () => {
	const calls = stubIndexedDb([fail('corrupt LevelDB'), fail('still corrupt')]);
	const w = captureWarn();
	try {
		const cache = await NoteCache.open('app2');
		assert.equal(cache.persistent, false);
		assert.deepEqual(calls.deleted, ['ff10k/app2']);
		assert.equal(w.warns.length, 1, w.warns.join('\n'));
		const seen = [];
		await cache.streamAll(10, (rows) => seen.push(rows));
		assert.deepEqual(seen, []);
		assert.deepEqual([...(await cache.getAllMeta())], []);
		await cache.put([{ path: 'a.md', mtime: 1, size: 1, text: 'a' }]);
		await cache.delete(['a.md']);
		await cache.rename('a.md', 'b.md');
		await cache.putAccess('a.md', [1]);
		assert.equal(typeof (await cache.bumpQuery('q', 'a.md')), 'number');
		assert.deepEqual(await cache.getAccess(), []);
		assert.deepEqual(await cache.getQueries(), []);
		await cache.clear();
		await cache.clearAccess();
		cache.close();
	} finally {
		w.restore();
	}
});
