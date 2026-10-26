// The only module that touches Smart Connections (`window.smart_env`, the plugin object).
// Target: Smart Connections 4.x (verified on 4.7.2). Never throws: every access is wrapped, a
// failure logs one warning per kind and turns the bridge off until the next modal open.

import type { App } from 'obsidian';
import type { EngineClient } from './engine-client';
import type { FastFindSettings } from './settings';

const PLUGIN_ID = 'smart-connections';
const SUPPORTED_MAJOR = 4;
const POLL_TOTAL_MS = 60_000;
const SYNC_INTERVAL_MS = 30_000;
const EMBED_CACHE_MAX = 200;
/** Items read per slice while collecting vectors (each `vec` is a getter, ~20 µs). */
const COLLECT_SLICE = 500;
/** Items per slice of the fingerprint pass (plain data, ~4 µs each). */
const FINGERPRINT_SLICE = 2000;

/** Doubling delays from `firstMs` whose sum is exactly `totalMs` (the last one is cut short). */
export function backoffDelays(totalMs = POLL_TOTAL_MS, firstMs = 1000): number[] {
	const out: number[] = [];
	let left = totalMs;
	for (let d = firstMs; left > 0 && d > 0; d *= 2) {
		out.push(Math.min(d, left));
		left -= out[out.length - 1];
	}
	return out;
}

/** Copies every vec of length `dims` into one row-major matrix; others are skipped and counted.
 * An error when not a single item has a usable vec. */
export function packVectors(
	items: { path: string; vec?: ArrayLike<number> | null }[],
	dims: number,
): { paths: string[]; data: Float32Array; skipped: number } | { error: string } {
	if (!(dims > 0)) return { error: `invalid dims ${dims}` };
	const good: { path: string; vec: ArrayLike<number> }[] = [];
	let skipped = 0;
	for (const it of items) {
		if (!it.vec) continue;
		if (it.vec.length === dims) good.push({ path: it.path, vec: it.vec });
		else skipped++;
	}
	if (good.length === 0) return { error: skipped > 0 ? `no vector has ${dims} dims (${skipped} differ)` : 'no vectors' };
	const data = new Float32Array(good.length * dims);
	good.forEach((g, i) => data.set(g.vec, i * dims));
	return { paths: good.map((g) => g.path), data, skipped };
}

/** Cheap change detector: entry count plus FNV-1a over `path \0 hash \n`. */
export function fingerprint(entries: { path: string; hash?: string }[]): string {
	let h = 0x811c9dc5;
	const mix = (s: string) => {
		for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 0x01000193);
	};
	for (const e of entries) {
		mix(e.path);
		mix('\0');
		mix(e.hash ?? '');
		mix('\n');
	}
	return `${entries.length}:${(h >>> 0).toString(16)}`;
}

// Shapes of the Smart Connections 4.7.2 objects we read (all optional: nothing is trusted).
interface ScItem {
	path?: string;
	vec?: unknown;
	data?: { last_read?: { hash?: string }; embedding?: Record<string, unknown> };
}
interface ScEmbedModel {
	dims?: number;
	embed_batch?: (inputs: { embed_input: string; purpose: string }[]) => Promise<{ vec?: unknown }[]>;
}
interface ScSources {
	items?: Record<string, ScItem>;
	embed_model?: ScEmbedModel;
}
interface ScPlugins {
	enabledPlugins?: Set<string>;
	plugins?: Record<string, { manifest?: { version?: string }; _loaded?: boolean } | undefined>;
}

function sources(): ScSources | null {
	const env = (window as unknown as { smart_env?: { smart_sources?: ScSources } }).smart_env;
	const s = env?.smart_sources;
	return s?.items && typeof s.items === 'object' ? s : null;
}

/** Sources loaded and their embeddings readable: one of the first items that claim an embedding has a vec
 * (Smart Connections loads the items before the vector files). */
function vectorsReady(s: ScSources | null): boolean {
	if (!s?.items) return false;
	let claimed = 0;
	for (const k in s.items) {
		const it = s.items[k];
		if (!embedRef(it)) continue;
		if (it.vec instanceof Float32Array) return true;
		if (++claimed >= 20) return false;
	}
	return false;
}

const yieldToUi = () => new Promise<void>((r) => window.setTimeout(r, 0));

export class SmartConnectionsBridge {
	private readonly app: App;
	private readonly engine: EngineClient;
	private readonly settings: FastFindSettings;
	/** Failure kinds already warned about (one warning each per plugin session). */
	private readonly warned = new Set<string>();
	/** Set by a failure; cleared by `onModalOpen`. */
	private failed = false;
	private vectorsLoaded = false;
	private lastFingerprint: string | null = null;
	private lastSyncAt = -Infinity;
	private syncing: Promise<boolean> | null = null;
	private pollTimer = 0;
	private polling = false;
	private disposed = false;
	private readonly embedCache = new Map<string, Float32Array>();

	constructor(app: App, engine: EngineClient, settings: FastFindSettings) {
		this.app = app;
		this.engine = engine;
		this.settings = settings;
	}

	/** Smart Connections running at a supported major version, its sources loaded, no failure since the last modal open. */
	available(): boolean {
		return !this.failed && this.pluginOk() && this.guard('env', () => sources() !== null, false);
	}

	/** True once the engine holds a vector matrix from Smart Connections. */
	hasVectors(): boolean {
		return this.vectorsLoaded;
	}

	/** Polls for the plugin and `smart_env.smart_sources.items` with backoff (~60 s; lazy loaders start
	 * Smart Connections late), then syncs the vectors. */
	start(): void {
		if (this.polling || this.disposed || !this.settings.useSmartConnections) return;
		this.polling = true;
		const delays = backoffDelays();
		const tick = (i: number) => {
			this.pollTimer = 0;
			if (this.disposed) return;
			if (this.pluginOk() && this.guard('env', () => vectorsReady(sources()), false)) {
				this.polling = false;
				void this.syncVectors();
				return;
			}
			if (i >= delays.length) {
				this.polling = false; // give up; the next modal open retries
				if (this.pluginOk()) this.warn('not loaded', 'embeddings not loaded after 60 s'); // not installed: stay quiet
				return;
			}
			this.pollTimer = window.setTimeout(() => tick(i + 1), delays[i]);
		};
		tick(0);
	}

	/** Called on every modal open: clears failures, retries, refreshes vectors (throttled). */
	onModalOpen(): void {
		this.failed = false;
		if (!this.settings.useSmartConnections || !this.pluginOk()) {
			this.dropVectors();
			return;
		}
		if (this.vectorsLoaded || this.lastFingerprint !== null) void this.syncVectors();
		else this.start();
	}

	/** Rebuilds the engine's vector matrix when the item set or a content hash changed; at most once per 30 s.
	 * Resolves true when the engine holds vectors afterwards. */
	syncVectors(): Promise<boolean> {
		if (this.syncing) return this.syncing;
		const now = performance.now();
		if (now - this.lastSyncAt < SYNC_INTERVAL_MS || this.disposed) return Promise.resolve(this.vectorsLoaded);
		this.lastSyncAt = now;
		this.syncing = this.doSync()
			.catch((e: unknown) => {
				this.fail('sync', `reading vectors failed (${String(e)})`);
				return this.vectorsLoaded;
			})
			.finally(() => {
				this.syncing = null;
			});
		return this.syncing;
	}

	private async doSync(): Promise<boolean> {
		if (!this.settings.useSmartConnections || !this.pluginOk()) return false;
		const s = sources();
		if (!s?.items) return false;
		const items = s.items;
		const keys = Object.keys(items);
		// fingerprint from plain data (path, read hash, embedding ref), no vec getter; sliced like the vec reads
		const entries: { path: string; hash: string }[] = [];
		for (let i = 0; i < keys.length; i++) {
			if (i > 0 && i % FINGERPRINT_SLICE === 0) {
				await yieldToUi();
				if (this.disposed) return false;
			}
			const it = items[keys[i]];
			entries.push({ path: typeof it?.path === 'string' ? it.path : keys[i], hash: `${it?.data?.last_read?.hash ?? ''}|${embedRef(it)}` });
		}
		const fp = fingerprint(entries);
		if (fp === this.lastFingerprint) return this.vectorsLoaded;

		const collected: { path: string; vec?: ArrayLike<number> | null }[] = [];
		let firstLen = 0;
		for (let i = 0; i < keys.length; i++) {
			if (i > 0 && i % COLLECT_SLICE === 0) {
				await yieldToUi();
				if (this.disposed) return false;
			}
			const vec = items[keys[i]]?.vec;
			if (vec instanceof Float32Array) {
				collected.push({ path: entries[i].path, vec });
				if (!firstLen) firstLen = vec.length;
			}
		}
		const modelDims = s.embed_model?.dims;
		const dims = typeof modelDims === 'number' && modelDims > 0 ? modelDims : firstLen;
		const packed = packVectors(collected, dims);
		if ('error' in packed) {
			// an empty read (embeddings still loading) is retried; wrong dims wait for a change
			this.lastFingerprint = collected.length > 0 ? fp : null;
			this.dropVectors();
			if (collected.length > 0) this.warn('dims', packed.error); // empty index: stay quiet
			return false;
		}
		const ok = await this.engine.setVectors(dims, packed.paths, packed.data);
		if (!ok) {
			this.fail('setVectors', 'engine rejected the vectors');
			return this.vectorsLoaded;
		}
		this.vectorsLoaded = true;
		this.lastFingerprint = fp;
		console.log(`[ff10k] vectors: ${packed.paths.length} notes (dims ${dims}, skipped ${packed.skipped})`);
		return true;
	}

	/** Query embedding through Smart Connections' own model (memoized per normalized query). Null on any failure. */
	async embed(query: string): Promise<Float32Array | null> {
		const key = query.trim().replace(/\s+/g, ' ').toLowerCase();
		if (!key || !this.available()) return null;
		const hit = this.embedCache.get(key);
		if (hit) return hit;
		try {
			const model = sources()?.embed_model;
			if (typeof model?.embed_batch !== 'function') throw new Error('no embed_batch');
			const out = await model.embed_batch([{ embed_input: query.trim(), purpose: 'query' }]);
			const vec = Array.isArray(out) ? out[0]?.vec : undefined;
			if (!(vec instanceof Float32Array) || vec.length === 0) throw new Error('no vec in the result');
			if (this.embedCache.size >= EMBED_CACHE_MAX) this.embedCache.delete(this.embedCache.keys().next().value as string);
			this.embedCache.set(key, vec);
			return vec;
		} catch (e) {
			this.fail('embed', `query embedding failed (${String(e)})`);
			return null;
		}
	}

	dispose(): void {
		this.disposed = true;
		window.clearTimeout(this.pollTimer);
		this.embedCache.clear();
	}

	private pluginOk(): boolean {
		return this.guard(
			'plugin',
			() => {
				const plugins = (this.app as unknown as { plugins?: ScPlugins }).plugins;
				const sc = plugins?.plugins?.[PLUGIN_ID];
				// lazy loaders (e.g. Lazy Plugin Loader) start it without listing it in enabledPlugins
				if (!sc || !(plugins?.enabledPlugins?.has(PLUGIN_ID) || sc._loaded === true)) return false;
				const version = sc.manifest?.version ?? '';
				if (Number.parseInt(version, 10) === SUPPORTED_MAJOR) return true;
				this.warn('version', `unsupported version ${version || '?'} (need ${SUPPORTED_MAJOR}.x)`);
				return false;
			},
			false,
		);
	}

	private dropVectors(): void {
		if (!this.vectorsLoaded) return;
		this.vectorsLoaded = false;
		this.lastFingerprint = null;
		this.engine.clearVectors();
	}

	private guard<T>(kind: string, fn: () => T, fallback: T): T {
		try {
			return fn();
		} catch (e) {
			this.fail(kind, String(e));
			return fallback;
		}
	}

	private fail(kind: string, message: string): void {
		this.failed = true;
		this.warn(kind, message);
	}

	private warn(kind: string, message: string): void {
		if (this.warned.has(kind)) return;
		this.warned.add(kind);
		console.warn(`[ff10k] smart connections: ${message}`);
	}
}

/** The item's default embedding reference (`file:file_i:read_hash`), so a fresh embedding changes the fingerprint. */
function embedRef(it: ScItem | undefined): string {
	const def = it?.data?.embedding?.default;
	if (!def || typeof def !== 'object') return '';
	return Object.values(def as Record<string, { file?: string; file_i?: number; read_hash?: string }>)
		.map((r) => `${r?.file ?? ''}:${r?.file_i ?? ''}:${r?.read_hash ?? ''}`)
		.join(',');
}
