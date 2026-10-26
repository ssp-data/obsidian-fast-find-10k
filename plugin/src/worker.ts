// Engine worker: owns the WasmEngine, answers Req messages with Res messages.
// Messages are processed strictly in order on one promise chain; a search that has been
// superseded by a newer one by the time it runs is answered as stale instead of executed.

import init, { normalize_query, WasmEngine } from '../../crates/ff10k-wasm/pkg/ff10k_wasm.js';
import wasmBytes from '../../crates/ff10k-wasm/pkg/ff10k_wasm_bg.wasm';
import type { Req, Res, SearchResponse } from './protocol';

interface WorkerScope {
	onmessage: ((ev: MessageEvent<Req>) => void) | null;
	postMessage(msg: Res): void;
}

const scope = self as unknown as WorkerScope;
let engine: WasmEngine | null = null;
let latestSearchId = 0;
let chain: Promise<void> = Promise.resolve();

const post = (msg: Res) => scope.postMessage(msg);

function eng(): WasmEngine {
	if (!engine) throw 'engine not initialized';
	return engine;
}

/** Rewrites UTF-8 byte ranges into `s` as UTF-16 code-unit ranges (what JS `slice` expects). */
function toUtf16Ranges(s: string, ranges: [number, number][]): [number, number][] {
	if (ranges.length === 0) return ranges;
	const map = new Map<number, number>([[0, 0]]);
	let bytes = 0;
	let units = 0;
	for (const ch of s) {
		const cp = ch.codePointAt(0) ?? 0;
		bytes += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
		units += ch.length;
		map.set(bytes, units);
	}
	const at = (b: number) => map.get(b) ?? units;
	return ranges.map(([a, b]) => [at(a), at(b)]);
}

function rangesToUtf16(res: SearchResponse): void {
	for (const h of res.hits) {
		h.titleRanges = toUtf16Ranges(h.titleNorm, h.titleRanges);
		if (h.excerpt) h.excerpt.ranges = toUtf16Ranges(h.excerpt.text, h.excerpt.ranges);
	}
}

/** Parses engine JSON, turning an `{"error": ...}` payload into a thrown string. */
function parse<T>(json: string): T {
	const v = JSON.parse(json) as T | { error: string };
	if (v !== null && typeof v === 'object' && !Array.isArray(v) && 'error' in v) throw v.error;
	return v as T;
}

async function handle(m: Req): Promise<void> {
	switch (m.t) {
		case 'init': {
			if (!engine) {
				await init({ module_or_path: wasmBytes });
				engine = new WasmEngine();
			}
			engine.set_config(JSON.stringify(m.config));
			post({ t: 'ready', id: m.id });
			return;
		}
		case 'config':
			eng().set_config(JSON.stringify(m.config));
			return;
		case 'upsert': {
			const e = eng();
			const count = e.upsert_batch(JSON.stringify(m.docs));
			post({ t: 'upserted', count, total: e.doc_count() });
			return;
		}
		case 'remove':
			for (const p of m.paths) eng().remove(p);
			return;
		case 'rename':
			eng().rename(m.old, m.new);
			return;
		case 'search': {
			if (m.id < latestSearchId) {
				post({ t: 'results', id: m.id, res: null, stale: true });
				return;
			}
			const opts = { mode: m.mode, limit: m.limit, nowSecs: Date.now() / 1000, debug: m.debug };
			const t0 = performance.now();
			const json = eng().search(m.query, JSON.stringify(opts));
			const elapsedUs = Math.round((performance.now() - t0) * 1000);
			const res = parse<SearchResponse>(json);
			res.elapsedUs = elapsedUs;
			rangesToUtf16(res);
			post({ t: 'results', id: m.id, res });
			return;
		}
		case 'similar':
			post({ t: 'similar', id: m.id, items: parse(eng().similar(m.path, m.limit, m.minScore)) });
			return;
		case 'semantic':
			post({ t: 'semantic', id: m.id, items: parse(eng().semantic(m.vec, m.limit, m.minScore)) });
			return;
		case 'ghosts':
			eng().set_ghosts(JSON.stringify(m.ghosts));
			return;
		case 'access':
			for (const a of m.entries) eng().set_access(a.path, JSON.stringify(a.ts));
			return;
		case 'track':
			eng().record_access(m.path, m.now);
			if (m.query !== undefined) eng().record_query_open(m.query, m.path);
			return;
		case 'combos':
			// record_query_open only counts up; the combo threshold is 3, so 5 is plenty
			for (const c of m.entries) for (let i = 0; i < Math.min(c.count, 5); i++) eng().record_query_open(c.query, c.path);
			return;
		case 'clearFrecency':
			eng().clear_frecency();
			return;
		case 'normalizeQuery':
			eng(); // wasm initialized
			post({ t: 'normalized', id: m.id, query: normalize_query(m.query) });
			return;
		case 'vectors':
			try {
				eng().set_vectors(m.dims, JSON.stringify(m.paths), m.data);
				post({ t: 'vectorsSet', ok: true });
			} catch (e) {
				post({ t: 'vectorsSet', ok: false, error: String(e) });
			}
			return;
		case 'clearVectors':
			eng().clear_vectors();
			return;
		case 'docText':
			post({ t: 'docText', id: m.id, text: eng().doc_text(m.path) ?? null });
			return;
		case 'stats':
			post({ t: 'stats', id: m.id, docs: eng().doc_count() });
			return;
	}
}

scope.onmessage = (ev) => {
	const m = ev.data;
	if (m.t === 'search' && m.id > latestSearchId) latestSearchId = m.id;
	chain = chain.then(() =>
		handle(m).catch((e: unknown) => {
			const id = 'id' in m ? m.id : undefined;
			post({ t: 'error', id, message: e instanceof Error ? e.message : String(e) });
		}),
	);
};
