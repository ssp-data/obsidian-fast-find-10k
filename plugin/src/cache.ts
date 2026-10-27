// Per-note IndexedDB cache (raw file text keyed by path) plus frecency/combo stores.
// Database `ff10k/<appId>`; the cache schema version lives in the `meta` store.
// When the database cannot be opened (e.g. corrupt LevelDB) it is deleted and reopened once;
// if that fails too, the cache runs in memory as a no-op (no warm start, no persisted frecency)
// so the vault is still indexed from its files and search works.

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

/** Deletes a database; a blocked delete (another connection open) counts as a failure. */
function deleteDb(name: string): Promise<void> {
	return new Promise((resolve, reject) => {
		const r = indexedDB.deleteDatabase(name);
		r.onsuccess = () => resolve();
		r.onerror = () => reject(r.error);
		r.onblocked = () => reject(new Error('delete blocked'));
	});
}

async function openDb(name: string): Promise<IDBDatabase> {
	const open = indexedDB.open(name, 1);
	open.onupgradeneeded = () => {
		const db = open.result;
		db.createObjectStore(NOTES, { keyPath: 'path' });
		db.createObjectStore(ACCESS, { keyPath: 'path' });
		db.createObjectStore(QUERIES, { keyPath: 'key' });
		db.createObjectStore(META, { keyPath: 'key' });
	};
	const db = await req(open);
	try {
		const schema = await req(db.transaction(META).objectStore(META).get('schema'));
		if (schema?.value !== CACHE_SCHEMA) {
			const tx = db.transaction([NOTES, ACCESS, QUERIES, META], 'readwrite');
			for (const store of [NOTES, ACCESS, QUERIES]) tx.objectStore(store).clear();
			tx.objectStore(META).put({ key: 'schema', value: CACHE_SCHEMA });
			await done(tx);
		}
		return db;
	} catch (e) {
		db.close();
		throw e;
	}
}

export class NoteCache {
	/** `db` null: the in-memory fallback, where every read is empty and every write a no-op. */
	private constructor(private readonly db: IDBDatabase | null) {}

	/** Opens `ff10k/<appId>`; never rejects (see the file header for the fallback). */
	static async open(appId: string): Promise<NoteCache> {
		const name = `ff10k/${appId}`;
		let first: unknown;
		try {
			return new NoteCache(await openDb(name));
		} catch (e) {
			first = e;
		}
		try {
			await deleteDb(name);
			const cache = new NoteCache(await openDb(name));
			console.warn(`Fast Find 10K: the note cache could not be opened (${String(first)}); recreated it, every note is re-read once`);
			return cache;
		} catch (e) {
			console.warn(
				`Fast Find 10K: the note cache could not be opened (${String(first)}; after deleting it: ${String(e)}); ` +
					'running without it: every start re-reads all notes and opens are not remembered across restarts',
			);
			return new NoteCache(null);
		}
	}

	/** False for the in-memory fallback. */
	get persistent(): boolean {
		return this.db !== null;
	}

	async getAllMeta(): Promise<Map<string, { mtime: number; size: number }>> {
		const meta = new Map<string, { mtime: number; size: number }>();
		await this.streamAll(500, (rows) => {
			for (const r of rows) meta.set(r.path, { mtime: r.mtime, size: r.size });
		});
		return meta;
	}

	/** Walks every note row with a cursor, handing them out `batch` at a time. Rejects (and
	 * aborts the walk) when `cb` throws or the transaction fails. */
	streamAll(batch: number, cb: (rows: NoteRow[]) => void): Promise<void> {
		const db = this.db;
		if (!db) return Promise.resolve();
		return new Promise((resolve, reject) => {
			const tx = db.transaction(NOTES);
			const cursorReq = tx.objectStore(NOTES).openCursor();
			let rows: NoteRow[] = [];
			let settled = false;
			const fail = (e: unknown) => {
				if (settled) return;
				settled = true;
				reject(e);
			};
			/** Hands out `rows`; false (after aborting) when `cb` threw. */
			const emit = (): boolean => {
				try {
					cb(rows);
				} catch (e) {
					fail(e);
					try {
						tx.abort();
					} catch {
						// already finished
					}
					return false;
				}
				rows = [];
				return true;
			};
			tx.onabort = () => fail(tx.error ?? new Error('transaction aborted'));
			tx.onerror = () => fail(tx.error);
			cursorReq.onerror = () => fail(cursorReq.error);
			cursorReq.onsuccess = () => {
				if (settled) return;
				const cursor = cursorReq.result;
				if (!cursor) {
					if (rows.length && !emit()) return;
					settled = true;
					resolve();
					return;
				}
				rows.push(cursor.value as NoteRow);
				if (rows.length < batch) {
					cursor.continue();
					return;
				}
				if (!emit()) return;
				// Yield a microtask between batches; the transaction stays active through it.
				queueMicrotask(() => cursor.continue());
			};
		});
	}

	put(rows: NoteRow[]): Promise<void> {
		if (!rows.length || !this.db) return Promise.resolve();
		const tx = this.db.transaction(NOTES, 'readwrite');
		const store = tx.objectStore(NOTES);
		for (const r of rows) store.put(r);
		return done(tx);
	}

	delete(paths: string[]): Promise<void> {
		if (!paths.length || !this.db) return Promise.resolve();
		const tx = this.db.transaction(NOTES, 'readwrite');
		const store = tx.objectStore(NOTES);
		for (const p of paths) store.delete(p);
		return done(tx);
	}

	/** Moves the note row, its access timestamps and its query counts to the new path. */
	async rename(oldPath: string, newPath: string): Promise<void> {
		if (!this.db) return;
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
		if (!this.db) return Promise.resolve([]);
		return req(this.db.transaction(ACCESS).objectStore(ACCESS).getAll()) as Promise<AccessRow[]>;
	}

	/** Every stored combo count; keys are `query|path` (a path never contains `|`). */
	async getQueries(): Promise<{ query: string; path: string; count: number }[]> {
		if (!this.db) return [];
		const rows = (await req(this.db.transaction(QUERIES).objectStore(QUERIES).getAll())) as QueryRow[];
		return rows.map((r) => {
			const cut = r.key.lastIndexOf('|');
			return { query: r.key.slice(0, cut), path: r.key.slice(cut + 1), count: r.count };
		});
	}

	putAccess(path: string, ts: number[]): Promise<void> {
		if (!this.db) return Promise.resolve();
		const tx = this.db.transaction(ACCESS, 'readwrite');
		tx.objectStore(ACCESS).put({ path, ts });
		return done(tx);
	}

	/** Increments the open count for (query, path) and returns the new count (0 in memory). */
	async bumpQuery(q: string, path: string): Promise<number> {
		if (!this.db) return 0;
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
		if (!this.db) return Promise.resolve();
		const tx = this.db.transaction(NOTES, 'readwrite');
		tx.objectStore(NOTES).clear();
		return done(tx);
	}

	clearAccess(): Promise<void> {
		if (!this.db) return Promise.resolve();
		const tx = this.db.transaction([ACCESS, QUERIES], 'readwrite');
		tx.objectStore(ACCESS).clear();
		tx.objectStore(QUERIES).clear();
		return done(tx);
	}

	close(): void {
		this.db?.close();
	}
}
