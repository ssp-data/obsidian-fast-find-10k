// Frecency: every note open (workspace `file-open`) is an access; opens from the modal also
// count toward the query→note combo. Persisted in the cache's `access`/`queries` stores.

import { type App, type EventRef, TFile } from 'obsidian';
import type { NoteCache } from './cache';
import type { EngineClient } from './engine-client';

/** A `file-open` of the note the modal opened within this window is the same open. */
export const MODAL_OPEN_WINDOW_MS = 500;
const MAX_TIMESTAMPS = 128;

/** Combo key: lower-cased, diacritics folded, whitespace collapsed. Matches the engine's
 * `Query::normalized` for plain word queries (not for quotes, `-x`, `#tag`, `path:` filters). */
export function normalizeQuery(q: string): string {
	return q
		.normalize('NFKC')
		.toLowerCase()
		.normalize('NFD')
		.replace(/\p{Mn}/gu, '')
		.normalize('NFC')
		.split(/\s+/)
		.filter(Boolean)
		.join(' ');
}

export class FrecencyTracker {
	/** Access timestamps (seconds, ascending) per path, mirrored from the cache. */
	private readonly access = new Map<string, number[]>();
	private lastModalOpen: { path: string; at: number } | null = null;
	private ref: EventRef | null = null;
	private disposed = false;

	constructor(
		private readonly app: App,
		private readonly engine: Pick<EngineClient, 'setAccess' | 'track'>,
		private readonly cache: Pick<NoteCache, 'getAccess' | 'putAccess' | 'bumpQuery' | 'clearAccess'>,
		private readonly isExcluded: (path: string) => boolean,
		private readonly now: () => number = Date.now,
	) {}

	/** Loads the access store into the engine, then starts listening to `file-open`. */
	async start(): Promise<void> {
		const entries = await this.cache.getAccess();
		if (this.disposed) return;
		for (const e of entries) this.access.set(e.path, e.ts);
		this.engine.setAccess(entries);
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

	/** Called by the modal before it opens `path`: one access plus one combo count for the query. */
	async noteModalOpen(path: string, rawQuery: string): Promise<void> {
		this.lastModalOpen = { path, at: this.now() };
		const q = normalizeQuery(rawQuery);
		await this.record(path, q || undefined);
		if (q) await this.cache.bumpQuery(q, path);
	}

	/** Forgets every access and combo count (engine combos stay until the next reload). */
	async clear(): Promise<void> {
		const paths = [...this.access.keys()];
		this.access.clear();
		this.engine.setAccess(paths.map((path) => ({ path, ts: [] })));
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
