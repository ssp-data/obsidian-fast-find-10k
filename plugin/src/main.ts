import { Notice, Plugin } from 'obsidian';
import workerSource from 'virtual:worker-source';
import { NoteCache } from './cache';
import { EngineClient } from './engine-client';
import { FrecencyTracker } from './frecency';
import { Indexer } from './indexer';
import type { Mode } from './protocol';
import { DEFAULT_KEYMAP, DEFAULT_SETTINGS, type FastFindSettings, settingsToEngineConfig } from './settings';
import { FastFindModal } from './ui/modal';
import { watchGhosts } from './unresolved';

export default class FastFindPlugin extends Plugin {
	settings: FastFindSettings = { ...DEFAULT_SETTINGS };
	engine: EngineClient | null = null;
	cache: NoteCache | null = null;
	indexer: Indexer | null = null;
	frecency: FrecencyTracker | null = null;
	private stopGhosts: (() => void) | null = null;
	private modal: FastFindModal | null = null;
	private ready: Promise<void> = Promise.resolve();

	async onload(): Promise<void> {
		const saved = (await this.loadData()) as Partial<FastFindSettings> | null;
		this.settings = { ...DEFAULT_SETTINGS, ...saved, keymap: { ...DEFAULT_KEYMAP, ...saved?.keymap } };

		const engine = new EngineClient(workerSource);
		this.engine = engine;
		this.ready = this.startIndexing(engine);
		this.ready.catch((e: unknown) => console.error('Fast Find 10K: startup failed', e));

		this.addCommand({ id: 'search', name: 'Search', callback: () => this.openModal('all') });
		this.addCommand({ id: 'search-files', name: 'Search file names', callback: () => this.openModal('filesOnly') });

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

	private async openModal(mode: Mode): Promise<void> {
		try {
			await this.ready;
		} catch (e) {
			new Notice(`Fast Find 10K: engine unavailable (${String(e)})`);
			return;
		}
		this.modal?.close();
		this.modal = new FastFindModal(this.app, this, mode);
		this.modal.open();
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
		const frecency = new FrecencyTracker(this.app, engine, cache, (p) => indexer.isExcluded(p));
		this.frecency = frecency;
		// before the indexer, so the empty query shows recent notes from the first batch on
		await frecency.start().catch((e: unknown) => console.error('Fast Find 10K: loading frecency failed', e));
		if (!this.engine) return; // unloaded meanwhile
		this.app.workspace.onLayoutReady(() => {
			for (const ref of indexer.handleEvents()) this.registerEvent(ref);
		});
		this.stopGhosts = watchGhosts(this.app, engine, indexer);
		indexer.start().catch((e: unknown) => console.error('Fast Find 10K: indexing failed', e));
	}

	onunload(): void {
		this.modal?.close(); // its engine is about to go away
		this.modal = null;
		this.stopGhosts?.();
		this.stopGhosts = null;
		this.frecency?.dispose();
		this.frecency = null;
		this.indexer?.dispose();
		this.indexer = null;
		this.engine?.dispose();
		this.engine = null;
		this.cache?.close();
		this.cache = null;
	}
}
