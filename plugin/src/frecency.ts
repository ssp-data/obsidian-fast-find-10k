// Frecency: every note open (workspace `file-open`) is an access; opens from the modal also
// count toward the query→note combo. Persisted in the cache's `access`/`queries` stores.

import { type App, type EventRef, TFile } from 'obsidian';
import type { NoteCache } from './cache';
import type { EngineClient } from './engine-client';

/** A `file-open` of the note the modal opened within this window is the same open. */
export const MODAL_OPEN_WINDOW_MS = 500;
const MAX_TIMESTAMPS = 128;

export class FrecencyTracker {
	/** Access timestamps (seconds, ascending) per path, mirrored from the cache. */
	private readonly access = new Map<string, number[]>();
	private lastModalOpen: { path: string; at: number } | null = null;
	private ref: EventRef | null = null;
	private disposed = false;

	constructor(
		private readonly app: App,
		private readonly engine: Pick<EngineClient, 'setAccess' | 'setCombos' | 'clearFrecency' | 'normalizeQuery' | 'track'>,
		private readonly cache: Pick<NoteCache, 'getAccess' | 'getQueries' | 'putAccess' | 'bumpQuery' | 'clearAccess'>,
		private readonly isExcluded: (path: string) => boolean,
		private readonly now: () => number = Date.now,
	) {}

	/** Loads the access and combo stores into the engine, then starts listening to `file-open`. */
	async start(): Promise<void> {
		const [entries, combos] = await Promise.all([this.cache.getAccess(), this.cache.getQueries()]);
		if (this.disposed) return;
		for (const e of entries) this.access.set(e.path, e.ts);
		this.engine.setAccess(entries);
		this.engine.setCombos(combos);
		this.ref = this.app.workspace.on('file-open', (file) => {
			if (file instanceof TFile) void this.recordFileOpen(file.path).catch((e: unknown) => console.error('[ff10k] frecency', e));
		});
	}

	dispose(): void {
		this.disposed = true;
		if (this.ref) this.app.workspace.offref(this.ref);
		this.ref = null;
	}

	/** One access, unless the modal opened this note within the last 500 ms (already recorded). */
	async recordFileOpen(path: string): Promise<void> {
		if (this.isExcluded(path)) return;
		const m = this.lastModalOpen;
		if (m && m.path === path && this.now() - m.at < MODAL_OPEN_WINDOW_MS) return;
		await this.record(path);
	}

	/** Called by the modal before it opens `path`: one access plus one combo count for the query
	 * (keyed by the engine's normalized form; none when that is empty or the engine is gone). */
	async noteModalOpen(path: string, rawQuery: string): Promise<void> {
		this.lastModalOpen = { path, at: this.now() };
		const q = rawQuery.trim() ? await this.engine.normalizeQuery(rawQuery).catch(() => '') : '';
		await this.record(path, q || undefined);
		if (q) await this.cache.bumpQuery(q, path);
	}

	/** Forgets every access and combo count, in the engine and in the cache. */
	async clear(): Promise<void> {
		this.access.clear();
		this.engine.clearFrecency();
		await this.cache.clearAccess();
	}

	private async record(path: string, query?: string): Promise<void> {
		const now = this.now() / 1000;
		const ts = [...(this.access.get(path) ?? []), now].slice(-MAX_TIMESTAMPS);
		this.access.set(path, ts);
		this.engine.track(path, now, query);
		await this.cache.putAccess(path, ts);
	}
}
