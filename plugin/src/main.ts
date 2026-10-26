import { Notice, Plugin } from 'obsidian';
import workerSource from 'virtual:worker-source';
import { NoteCache } from './cache';
import { EngineClient } from './engine-client';
import { Indexer } from './indexer';
import { DEFAULT_SETTINGS, type FastFindSettings, settingsToEngineConfig } from './settings';

export default class FastFindPlugin extends Plugin {
	settings: FastFindSettings = { ...DEFAULT_SETTINGS };
	engine: EngineClient | null = null;
	cache: NoteCache | null = null;
	indexer: Indexer | null = null;
	private ready: Promise<void> = Promise.resolve();

	async onload(): Promise<void> {
		const saved = (await this.loadData()) as Partial<FastFindSettings> | null;
		this.settings = { ...DEFAULT_SETTINGS, ...saved };

		const engine = new EngineClient(workerSource);
		this.engine = engine;
		this.ready = this.startIndexing(engine);
		this.ready.catch((e: unknown) => console.error('Fast Find 10K: startup failed', e));

		this.addCommand({
			id: 'search',
			name: 'Search',
			callback: async () => {
				try {
					await this.ready;
					const s = this.indexer?.status;
					const res = await engine.search('obsidian trouble', 'all', 5, false);
					new Notice(`Fast Find 10K: ${s?.indexed ?? 0}/${s?.total ?? 0} indexed, top "${res.hits[0]?.title ?? '-'}"`);
				} catch (e) {
					new Notice(`Fast Find 10K: engine unavailable (${String(e)})`);
				}
			},
		});

		this.addCommand({
			id: 'rebuild-index',
			name: 'Rebuild index',
			callback: async () => {
				try {
					await this.ready;
					await this.indexer?.rebuild();
					new Notice(`Fast Find 10K: rebuilt, ${this.indexer?.status.indexed ?? 0} docs`);
				} catch (e) {
					new Notice(`Fast Find 10K: rebuild failed (${String(e)})`);
				}
			},
		});
	}

	/** Engine init, cache open, then the indexer; resolves once the engine is searchable. */
	private async startIndexing(engine: EngineClient): Promise<void> {
		await engine.init(settingsToEngineConfig(this.settings));
		const appId = (this.app as unknown as { appId: string }).appId;
		const cache = await NoteCache.open(appId);
		if (!this.engine) return cache.close(); // unloaded meanwhile
		this.cache = cache;
		const indexer = new Indexer(this.app, engine, cache, this.settings);
		this.indexer = indexer;
		this.app.workspace.onLayoutReady(() => {
			for (const ref of indexer.handleEvents()) this.registerEvent(ref);
		});
		indexer.start().catch((e: unknown) => console.error('Fast Find 10K: indexing failed', e));
	}

	onunload(): void {
		this.indexer?.dispose();
		this.indexer = null;
		this.engine?.dispose();
		this.engine = null;
		this.cache?.close();
		this.cache = null;
	}
}
