// Unresolved link targets ("ghosts"): collected from Obsidian's metadata cache and fed to the
// engine, which shows them as tier-1 rows until the note exists.

import type { App, EventRef } from 'obsidian';
import type { EngineClient } from './engine-client';
import type { Indexer } from './indexer';
import type { Ghost } from './protocol';

const MAX_GHOSTS = 5000;
const REFRESH_MS = 2000;
/** Links to missing attachments are not notes to create. */
const ATTACHMENT = /\.(png|jpe?g|gif|bmp|svg|webp|avif|heic|pdf|canvas|base|mp3|m4a|wav|ogg|flac|webm|mp4|mov|mkv|ogv|zip)$/i;

/** Link target without `#heading`/`#^block` and `|alias`, trimmed. */
function cleanTarget(raw: string): string {
	const cut = raw.search(/[#|]/);
	return (cut < 0 ? raw : raw.slice(0, cut)).trim();
}

/** Unresolved targets with the number of distinct source notes linking them, most referenced
 * first, then by target. Sources the indexer excludes are skipped. */
export function collectGhosts(app: App, isExcluded: (path: string) => boolean = () => false): Ghost[] {
	const unresolved = app.metadataCache.unresolvedLinks as Record<string, Record<string, number>>;
	const refs = new Map<string, number>();
	for (const [source, targets] of Object.entries(unresolved)) {
		if (isExcluded(source)) continue;
		const seen = new Set<string>();
		for (const raw of Object.keys(targets)) {
			const t = cleanTarget(raw);
			if (t && !ATTACHMENT.test(t)) seen.add(t);
		}
		for (const t of seen) refs.set(t, (refs.get(t) ?? 0) + 1);
	}
	return [...refs]
		.map(([target, n]) => ({ target, refs: n }))
		.sort((a, b) => b.refs - a.refs || (a.target < b.target ? -1 : a.target > b.target ? 1 : 0));
}

/** Trailing throttle: `call` schedules `fn` once, `ms` later; calls in between are merged. */
export function throttle(fn: () => void, ms: number): { call(): void; cancel(): void } {
	let timer: number | null = null;
	return {
		call() {
			if (timer !== null) return;
			timer = window.setTimeout(() => {
				timer = null;
				fn();
			}, ms);
		},
		cancel() {
			if (timer !== null) window.clearTimeout(timer);
			timer = null;
		},
	};
}

/** Keeps the engine's ghosts current: after each indexer run and on metadata `resolved`
 * (at most every 2 s). Returns the dispose function. */
export function watchGhosts(app: App, engine: Pick<EngineClient, 'setGhosts'>, indexer: Indexer, log: () => boolean = () => false): () => void {
	let capLogged = false;
	const refresh = throttle(() => {
		const ghosts = collectGhosts(app, (p) => indexer.isExcluded(p));
		if (ghosts.length > MAX_GHOSTS && !capLogged && log()) {
			capLogged = true;
			console.log(`[ff10k] ${ghosts.length} unresolved link targets; keeping the ${MAX_GHOSTS} most linked`);
		}
		engine.setGhosts(ghosts.slice(0, MAX_GHOSTS));
	}, REFRESH_MS);
	const unsubscribe = indexer.onStatus((s) => s.done && refresh.call());
	if (indexer.status.done) refresh.call();
	const ref: EventRef = app.metadataCache.on('resolved', () => refresh.call());
	return () => {
		unsubscribe();
		app.metadataCache.offref(ref);
		refresh.cancel();
	};
}
