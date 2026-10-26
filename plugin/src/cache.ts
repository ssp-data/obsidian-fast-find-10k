// Per-note IndexedDB cache (raw file text keyed by path) plus frecency/combo stores.
// Database `ff10k/<appId>`; the cache schema version lives in the `meta` store.

export const CACHE_SCHEMA = 1;

/** `mtime` in seconds (as the engine expects), `size` in bytes. */
export interface NoteRow {
	path: string;
	mtime: number;
	size: number;
	text: string;
}

export interface AccessRow {
	path: string;
	ts: number[];
}

interface QueryRow {
	key: string;
	count: number;
}

const NOTES = 'notes';
const ACCESS = 'access';
const QUERIES = 'queries';
const META = 'meta';

function req<T>(r: IDBRequest<T>): Promise<T> {
	return new Promise((resolve, reject) => {
		r.onsuccess = () => resolve(r.result);
		r.onerror = () => reject(r.error);
	});
}

function done(tx: IDBTransaction): Promise<void> {
	return new Promise((resolve, reject) => {
		tx.oncomplete = () => resolve();
		tx.onerror = () => reject(tx.error);
		tx.onabort = () => reject(tx.error ?? new Error('transaction aborted'));
	});
}

export class NoteCache {
	private constructor(private readonly db: IDBDatabase) {}

	static async open(appId: string): Promise<NoteCache> {
		const open = indexedDB.open(`ff10k/${appId}`, 1);
		open.onupgradeneeded = () => {
			const db = open.result;
			db.createObjectStore(NOTES, { keyPath: 'path' });
			db.createObjectStore(ACCESS, { keyPath: 'path' });
			db.createObjectStore(QUERIES, { keyPath: 'key' });
			db.createObjectStore(META, { keyPath: 'key' });
		};
		const cache = new NoteCache(await req(open));
		const schema = await req(cache.db.transaction(META).objectStore(META).get('schema'));
		if (schema?.value !== CACHE_SCHEMA) {
			await cache.clear();
			await cache.clearAccess();
			const tx = cache.db.transaction(META, 'readwrite');
			tx.objectStore(META).put({ key: 'schema', value: CACHE_SCHEMA });
			await done(tx);
		}
		return cache;
	}

	async getAllMeta(): Promise<Map<string, { mtime: number; size: number }>> {
		const meta = new Map<string, { mtime: number; size: number }>();
		await this.streamAll(500, (rows) => {
			for (const r of rows) meta.set(r.path, { mtime: r.mtime, size: r.size });
		});
		return meta;
	}

	/** Walks every note row with a cursor, handing them out `batch` at a time. */
	streamAll(batch: number, cb: (rows: NoteRow[]) => void): Promise<void> {
		return new Promise((resolve, reject) => {
			const tx = this.db.transaction(NOTES);
			const cursorReq = tx.objectStore(NOTES).openCursor();
			let rows: NoteRow[] = [];
			cursorReq.onerror = () => reject(cursorReq.error);
			cursorReq.onsuccess = () => {
				const cursor = cursorReq.result;
				if (!cursor) {
					if (rows.length) cb(rows);
					resolve();
					return;
				}
				rows.push(cursor.value as NoteRow);
				if (rows.length < batch) {
					cursor.continue();
					return;
				}
				cb(rows);
				rows = [];
				// Yield a microtask between batches; the transaction stays active through it.
				queueMicrotask(() => cursor.continue());
			};
		});
	}

	put(rows: NoteRow[]): Promise<void> {
		if (!rows.length) return Promise.resolve();
		const tx = this.db.transaction(NOTES, 'readwrite');
		const store = tx.objectStore(NOTES);
		for (const r of rows) store.put(r);
		return done(tx);
	}

	delete(paths: string[]): Promise<void> {
		if (!paths.length) return Promise.resolve();
		const tx = this.db.transaction(NOTES, 'readwrite');
		const store = tx.objectStore(NOTES);
		for (const p of paths) store.delete(p);
		return done(tx);
	}

	/** Moves the note row, its access timestamps and its query counts to the new path. */
	async rename(oldPath: string, newPath: string): Promise<void> {
		const tx = this.db.transaction([NOTES, ACCESS, QUERIES], 'readwrite');
		const finished = done(tx);
		for (const name of [NOTES, ACCESS]) {
			const store = tx.objectStore(name);
			const getReq = store.get(oldPath);
			getReq.onsuccess = () => {
				const row = getReq.result as { path: string } | undefined;
				if (!row) return;
				store.delete(oldPath);
				store.put({ ...row, path: newPath });
			};
		}
		const queries = tx.objectStore(QUERIES);
		const suffix = `|${oldPath}`;
		const cursorReq = queries.openCursor();
		cursorReq.onsuccess = () => {
			const cursor = cursorReq.result;
			if (!cursor) return;
			const row = cursor.value as QueryRow;
			if (row.key.endsWith(suffix)) {
				cursor.delete();
				queries.put({ key: row.key.slice(0, -suffix.length) + `|${newPath}`, count: row.count });
			}
			cursor.continue();
		};
		await finished;
	}

	getAccess(): Promise<AccessRow[]> {
		return req(this.db.transaction(ACCESS).objectStore(ACCESS).getAll()) as Promise<AccessRow[]>;
	}

	/** Every stored combo count; keys are `query|path` (a path never contains `|`). */
	async getQueries(): Promise<{ query: string; path: string; count: number }[]> {
		const rows = (await req(this.db.transaction(QUERIES).objectStore(QUERIES).getAll())) as QueryRow[];
		return rows.map((r) => {
			const cut = r.key.lastIndexOf('|');
			return { query: r.key.slice(0, cut), path: r.key.slice(cut + 1), count: r.count };
		});
	}

	putAccess(path: string, ts: number[]): Promise<void> {
		const tx = this.db.transaction(ACCESS, 'readwrite');
		tx.objectStore(ACCESS).put({ path, ts });
		return done(tx);
	}

	/** Increments the open count for (query, path) and returns the new count. */
	async bumpQuery(q: string, path: string): Promise<number> {
		const tx = this.db.transaction(QUERIES, 'readwrite');
		const finished = done(tx);
		const store = tx.objectStore(QUERIES);
		const key = `${q}|${path}`;
		const row = (await req(store.get(key))) as QueryRow | undefined;
		const count = (row?.count ?? 0) + 1;
		store.put({ key, count });
		await finished;
		return count;
	}

	/** Drops every cached note (frecency is kept; see clearAccess). */
	clear(): Promise<void> {
		const tx = this.db.transaction(NOTES, 'readwrite');
		tx.objectStore(NOTES).clear();
		return done(tx);
	}

	clearAccess(): Promise<void> {
		const tx = this.db.transaction([ACCESS, QUERIES], 'readwrite');
		tx.objectStore(ACCESS).clear();
		tx.objectStore(QUERIES).clear();
		return done(tx);
	}

	close(): void {
		this.db.close();
	}
}
