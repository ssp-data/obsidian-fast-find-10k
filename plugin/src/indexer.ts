// Keeps the engine and the note cache in sync with the vault: warm start from the cache,
// diff against the vault, then incremental updates from vault events.

import { type App, type EventRef, type TAbstractFile, TFile } from 'obsidian';
import type { NoteCache, NoteRow } from './cache';
import type { EngineClient } from './engine-client';
import type { DocInput } from './protocol';
import { debugLog } from './debug';
import type { FastFindSettings } from './settings';

export interface IndexStatus {
	/** Docs in the engine (from its latest `upserted` reply). */
	indexed: number;
	/** Markdown files expected after exclusions (0 until the vault was listed). */
	total: number;
	done: boolean;
	/** Duration of the last full run, once `done`. */
	elapsedMs?: number;
}

const STREAM_BATCH = 500;
const READ_BATCH = 200;
const COALESCE_MS = 500;

/** Minimatch-style glob → anchored RegExp: `**` spans folders, `*` stays within one segment.
 * A glob without `/` matches the file name in any folder. */
export function globToRegExp(glob: string): RegExp {
	let re = '';
	for (let i = 0; i < glob.length; i++) {
		const c = glob[i];
		if (c === '*' && glob[i + 1] === '*') {
			const slashAfter = glob[i + 2] === '/';
			re += slashAfter ? '(?:.*/)?' : '.*';
			i += slashAfter ? 2 : 1;
		} else if (c === '*') {
			re += '[^/]*';
		} else {
			re += c.replace(/[.+?^${}()|[\]\\]/g, '\\$&');
		}
	}
	return new RegExp(glob.includes('/') ? `^${re}$` : `(?:^|/)${re}$`);
}

/** Files the index covers: markdown notes, Obsidian Bases (`.base`, YAML), canvases (`.canvas`,
 * converted to markdown by the engine) and Excalidraw drawings (`.excalidraw`, markdown with a
 * drawing blob the engine skips). */
export function isIndexedExtension(ext: string): boolean {
	return ext === 'md' || ext === 'base' || ext === 'canvas' || ext === 'excalidraw';
}

/** Pure exclusion rule: dot-folders/files anywhere, other extensions, Obsidian "Excluded files", plugin globs. */
export function isExcludedPath(path: string, isUserIgnored: (p: string) => boolean, globs: RegExp[]): boolean {
	if (!isIndexedExtension(path.slice(path.lastIndexOf('.') + 1))) return true;
	if (path.startsWith('.') || path.includes('/.')) return true;
	if (isUserIgnored(path)) return true;
	return globs.some((g) => g.test(path));
}

interface MetadataCacheWithIgnore {
	isUserIgnored(path: string): boolean;
}

type Pending = 'upsert' | 'delete';

export class Indexer {
	status: IndexStatus = { indexed: 0, total: 0, done: false };
	private globs: RegExp[];
	private readonly listeners = new Set<(s: IndexStatus) => void>();
	private readonly timers = new Map<string, { kind: Pending; timer: number }>();
	/** Paths currently sent to the engine. */
	private readonly inEngine = new Set<string>();
	private run: Promise<void> = Promise.resolve();
	private disposed = false;

	constructor(
		private readonly app: App,
		private readonly engine: EngineClient,
		private readonly cache: NoteCache,
		settings: Pick<FastFindSettings, 'ignoredGlobs'>,
	) {
		this.globs = settings.ignoredGlobs.map(globToRegExp);
	}

	/** Replaces the ignored globs; takes effect for the index on the next `rebuild()`. */
	setIgnoredGlobs(globs: string[]): void {
		this.globs = globs.map(globToRegExp);
	}

	isExcluded(path: string): boolean {
		const mc = this.app.metadataCache as unknown as MetadataCacheWithIgnore;
		return isExcludedPath(path, (p) => mc.isUserIgnored(p), this.globs);
	}

	/** Subscribes to status changes; returns the unsubscribe function. */
	onStatus(cb: (s: IndexStatus) => void): () => void {
		this.listeners.add(cb);
		return () => this.listeners.delete(cb);
	}

	/** Warm start: cache → engine, then diff the vault and re-read what changed. */
	start(): Promise<void> {
		return this.enqueue(() => this.fullRun(false));
	}

	/** Drops the note cache and re-reads every file. */
	rebuild(): Promise<void> {
		return this.enqueue(async () => {
			try {
				await this.cache.clear();
			} catch (e) {
				console.error('[ff10k] clearing the note cache failed', e);
			}
			this.engine.clear();
			this.inEngine.clear();
			await this.fullRun(true);
		});
	}

	/** Serializes full runs; a failed run never blocks the next one (Rebuild is the recovery path). */
	private enqueue(job: () => Promise<void>): Promise<void> {
		this.run = this.run.catch(() => {}).then(job);
		return this.run;
	}

	/** Vault event handlers; the caller registers the returned refs (plugin.registerEvent). */
	handleEvents(): EventRef[] {
		const vault = this.app.vault;
		const isNote = (f: TAbstractFile): f is TFile => f instanceof TFile && isIndexedExtension(f.extension);
		return [
			vault.on('create', (f) => isNote(f) && this.schedule(f.path, 'upsert')),
			vault.on('modify', (f) => isNote(f) && this.schedule(f.path, 'upsert')),
			vault.on('delete', (f) => isNote(f) && this.schedule(f.path, 'delete')),
			vault.on('rename', (f, oldPath) => {
				if (f instanceof TFile) this.onRename(oldPath, f.path);
			}),
		];
	}

	dispose(): void {
		this.disposed = true;
		for (const { timer } of this.timers.values()) window.clearTimeout(timer);
		this.timers.clear();
		this.listeners.clear();
	}

	/** Never rejects: on failure it logs and still marks the run done. */
	private async fullRun(fromScratch: boolean): Promise<void> {
		const t0 = performance.now();
		this.setStatus({ indexed: this.status.indexed, total: 0, done: false });
		this.engine.onProgress((_count, total) => this.setStatus({ ...this.status, indexed: total }));
		try {
			await this.indexVault(fromScratch, t0);
		} catch (e) {
			console.error('[ff10k] indexing failed', e);
		} finally {
			if (!this.status.done && !this.disposed) {
				this.setStatus({ ...this.status, done: true, elapsedMs: Math.round(performance.now() - t0) });
			}
		}
	}

	private async indexVault(fromScratch: boolean, t0: number): Promise<void> {

		// 1. Cache → engine; searchable as soon as the first batch lands.
		const meta = new Map<string, { mtime: number; size: number }>();
		if (!fromScratch) {
			await this.cache.streamAll(STREAM_BATCH, (rows) => {
				if (this.disposed) return;
				const docs: DocInput[] = [];
				for (const r of rows) {
					meta.set(r.path, { mtime: r.mtime, size: r.size });
					if (this.isExcluded(r.path)) continue;
					docs.push({ path: r.path, mtime: r.mtime, text: r.text });
					this.inEngine.add(r.path);
				}
				if (docs.length) this.engine.upsert(docs);
			});
		}
		const fromCache = this.inEngine.size;

		// 2. Diff against the vault (the file list is complete once the layout is ready).
		await new Promise<void>((resolve) => this.app.workspace.onLayoutReady(resolve));
		if (this.disposed) return;
		const files = this.app.vault.getFiles().filter((f) => isIndexedExtension(f.extension) && !this.isExcluded(f.path));
		this.setStatus({ ...this.status, total: files.length });
		const present = new Set(files.map((f) => f.path));
		const gone = [...meta.keys()].filter((p) => !present.has(p) || this.isExcluded(p));
		if (gone.length) {
			this.engine.remove(gone.filter((p) => this.inEngine.delete(p)));
			await this.cache.delete(gone);
		}
		const changed = files.filter((f) => {
			const m = meta.get(f.path);
			return !m || m.mtime !== f.stat.mtime / 1000 || m.size !== f.stat.size;
		});
		for (let i = 0; i < changed.length && !this.disposed; i += READ_BATCH) {
			await this.indexFiles(changed.slice(i, i + READ_BATCH));
		}
		if (this.disposed) return;

		// 3. Done once the worker has processed everything sent so far.
		const docs = await this.engine.stats();
		const elapsedMs = Math.round(performance.now() - t0);
		this.setStatus({ indexed: docs, total: files.length, done: true, elapsedMs });
		debugLog(`indexed ${docs} docs in ${elapsedMs} ms (warm: ${fromCache} from cache, ${changed.length} re-read)`);
	}

	/** Reads the files, sends them to the engine and stores them in the cache.
	 * Unreadable files are skipped; a row whose file changed during the read, or that has a
	 * pending event, is dropped because a newer flush owns that path. */
	private async indexFiles(files: TFile[]): Promise<void> {
		const read = await Promise.all(
			files.map(async (f): Promise<NoteRow | null> => {
				const mtime = f.stat.mtime / 1000;
				const size = f.stat.size;
				try {
					const text = await this.app.vault.cachedRead(f);
					return { path: f.path, mtime, size, text };
				} catch {
					return null;
				}
			}),
		);
		if (this.disposed) return;
		const failed = read.filter((r) => r === null).length;
		if (failed) console.error(`[ff10k] ${failed} of ${files.length} files could not be read`);
		const rows = read.filter(
			(r, i): r is NoteRow => r !== null && files[i].stat.mtime / 1000 === r.mtime && !this.timers.has(r.path),
		);
		if (!rows.length) return;
		this.engine.upsert(rows.map((r) => ({ path: r.path, mtime: r.mtime, text: r.text })));
		for (const r of rows) this.inEngine.add(r.path);
		try {
			await this.cache.put(rows);
		} catch (e) {
			console.error('[ff10k] caching notes failed', e);
		}
	}

	private schedule(path: string, kind: Pending): void {
		if (this.isExcluded(path)) return;
		const prev = this.timers.get(path);
		if (prev) window.clearTimeout(prev.timer);
		const timer = window.setTimeout(() => {
			this.timers.delete(path);
			void this.flush(path, kind).catch((e: unknown) => console.error('[ff10k] update failed', path, e));
		}, COALESCE_MS);
		this.timers.set(path, { kind, timer });
	}

	private async flush(path: string, kind: Pending): Promise<void> {
		if (this.disposed) return;
		if (kind === 'delete') {
			this.inEngine.delete(path);
			this.engine.remove([path]);
			await this.cache.delete([path]);
			return;
		}
		const f = this.app.vault.getFileByPath(path);
		if (f) await this.indexFiles([f]);
	}

	private onRename(oldPath: string, newPath: string): void {
		const oldIn = !this.isExcluded(oldPath);
		const newIn = !this.isExcluded(newPath);
		const pending = this.timers.get(oldPath);
		if (pending) {
			window.clearTimeout(pending.timer);
			this.timers.delete(oldPath);
		}
		if (oldIn && newIn) {
			this.inEngine.delete(oldPath);
			this.inEngine.add(newPath);
			this.engine.rename(oldPath, newPath);
			void this.cache.rename(oldPath, newPath).catch((e: unknown) => console.error('[ff10k] rename failed', e));
			if (pending?.kind === 'upsert') this.schedule(newPath, 'upsert');
		} else if (oldIn) {
			this.schedule(oldPath, 'delete');
		} else if (newIn) {
			this.schedule(newPath, 'upsert');
		}
	}

	private setStatus(s: IndexStatus): void {
		this.status = s;
		for (const cb of this.listeners) cb(s);
	}
}
