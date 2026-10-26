// UI-thread handle on the engine worker. Owns the Worker, correlates replies by id.

import type {
	AccessEntry,
	DocInput,
	EngineConfig,
	Ghost,
	Mode,
	Req,
	Res,
	SearchResponse,
	SemanticHit,
	SimilarHit,
} from './protocol';

interface Pending {
	resolve: (v: unknown) => void;
	reject: (e: unknown) => void;
}

export class EngineClient {
	private readonly worker: Worker;
	private readonly url: string;
	private nextId = 1;
	private latestSearchId = 0;
	private readonly pending = new Map<number, Pending>();
	private readonly searchIds = new Set<number>();
	/** set_vectors replies carry no id; the worker answers in order. */
	private readonly vectorWaiters: ((ok: boolean) => void)[] = [];
	private progress: ((count: number, total: number) => void) | null = null;
	/** Set once the worker died or was disposed; every later request rejects with it. */
	private dead: string | null = null;

	constructor(workerSource: string) {
		this.url = URL.createObjectURL(new Blob([workerSource], { type: 'text/javascript' }));
		this.worker = new Worker(this.url);
		this.worker.onmessage = (ev: MessageEvent<Res>) => this.onMessage(ev.data);
		this.worker.onerror = (ev) => this.fail(`worker error: ${ev.message}`);
		this.worker.onmessageerror = () => this.fail('worker message could not be deserialized');
	}

	init(config: EngineConfig): Promise<void> {
		const id = this.nextId++;
		return this.request<void>(id, { t: 'init', id, config });
	}

	setConfig(config: EngineConfig): void {
		this.send({ t: 'config', config });
	}

	upsert(docs: DocInput[]): void {
		this.send({ t: 'upsert', docs });
	}

	remove(paths: string[]): void {
		this.send({ t: 'remove', paths });
	}

	rename(oldPath: string, newPath: string): void {
		this.send({ t: 'rename', old: oldPath, new: newPath });
	}

	/** Resolves only for the newest search; every older pending search rejects with 'stale'. */
	search(query: string, mode: Mode, limit: number, debug: boolean): Promise<SearchResponse> {
		const id = this.nextId++;
		this.latestSearchId = id;
		for (const old of this.searchIds) this.reject(old, 'stale');
		this.searchIds.add(id);
		return this.request<SearchResponse>(id, { t: 'search', id, query, mode, limit, debug });
	}

	similar(path: string, limit: number, minScore: number): Promise<SimilarHit[]> {
		const id = this.nextId++;
		return this.request<SimilarHit[]>(id, { t: 'similar', id, path, limit, minScore });
	}

	semantic(vec: Float32Array, limit: number, minScore: number): Promise<SemanticHit[]> {
		const id = this.nextId++;
		return this.request<SemanticHit[]>(id, { t: 'semantic', id, vec, limit, minScore });
	}

	setGhosts(ghosts: Ghost[]): void {
		this.send({ t: 'ghosts', ghosts });
	}

	setAccess(entries: AccessEntry[]): void {
		this.send({ t: 'access', entries });
	}

	track(path: string, now: number, query?: string): void {
		this.send(query === undefined ? { t: 'track', path, now } : { t: 'track', path, now, query });
	}

	setVectors(dims: number, paths: string[], data: Float32Array): Promise<boolean> {
		if (this.dead) return Promise.resolve(false);
		return new Promise((resolve) => {
			this.vectorWaiters.push(resolve);
			this.send({ t: 'vectors', dims, paths, data });
		});
	}

	clearVectors(): void {
		this.send({ t: 'clearVectors' });
	}

	docText(path: string): Promise<string | null> {
		const id = this.nextId++;
		return this.request<string | null>(id, { t: 'docText', id, path });
	}

	/** Number of indexed docs. */
	stats(): Promise<number> {
		const id = this.nextId++;
		return this.request<number>(id, { t: 'stats', id });
	}

	onProgress(cb: (count: number, total: number) => void): void {
		this.progress = cb;
	}

	dispose(): void {
		this.fail('disposed');
		URL.revokeObjectURL(this.url);
	}

	/** Marks the client dead: terminates the worker and settles everything still waiting. */
	private fail(reason: string): void {
		if (this.dead) return;
		this.dead = reason;
		if (reason !== 'disposed') console.error('Fast Find 10K:', reason);
		this.worker.terminate();
		for (const id of [...this.pending.keys()]) this.reject(id, reason);
		for (const w of this.vectorWaiters.splice(0)) w(false);
	}

	private send(msg: Req): void {
		if (!this.dead) this.worker.postMessage(msg);
	}

	private request<T>(id: number, msg: Req): Promise<T> {
		if (this.dead) return Promise.reject(this.dead);
		return new Promise<T>((resolve, reject) => {
			this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
			this.send(msg);
		});
	}

	/** Removes and returns the pending request `id` (undefined if already settled). */
	private take(id: number): Pending | undefined {
		const p = this.pending.get(id);
		this.pending.delete(id);
		this.searchIds.delete(id);
		return p;
	}

	private resolve(id: number, value: unknown): void {
		this.take(id)?.resolve(value);
	}

	private reject(id: number, reason: unknown): void {
		this.take(id)?.reject(reason);
	}

	private onMessage(m: Res): void {
		switch (m.t) {
			case 'ready':
				return this.resolve(m.id, undefined);
			case 'upserted':
				this.progress?.(m.count, m.total);
				return;
			case 'results':
				if (m.stale || m.id < this.latestSearchId) return this.reject(m.id, 'stale');
				return this.resolve(m.id, m.res);
			case 'similar':
			case 'semantic':
				return this.resolve(m.id, m.items);
			case 'docText':
				return this.resolve(m.id, m.text);
			case 'stats':
				return this.resolve(m.id, m.docs);
			case 'vectorsSet':
				if (!m.ok) console.error('Fast Find 10K: set_vectors failed', m.error);
				this.vectorWaiters.shift()?.(m.ok);
				return;
			case 'error':
				if (m.id !== undefined && this.pending.has(m.id)) return this.reject(m.id, m.message);
				console.error('Fast Find 10K: engine error', m.message);
				return;
		}
	}
}
