import { Notice, Platform, Plugin } from 'obsidian';
import workerSource from 'virtual:worker-source';
import { NoteCache } from './cache';
import { EngineClient } from './engine-client';
import { FrecencyTracker } from './frecency';
import { Indexer } from './indexer';
import type { Mode } from './protocol';
import { DEFAULT_SETTINGS, type FastFindSettings, mergeSettings, migrateKeymap, parseDictionaryLines, settingsToEngineConfig } from './settings';
import { FastFindSettingTab } from './settings-tab';
import { SmartConnectionsBridge } from './smart-connections-bridge';
import { FastFindModal } from './ui/modal';
import { watchGhosts } from './unresolved';

export default class FastFindPlugin extends Plugin {
	settings: FastFindSettings = { ...DEFAULT_SETTINGS };
	engine: EngineClient | null = null;
	cache: NoteCache | null = null;
	indexer: Indexer | null = null;
	frecency: FrecencyTracker | null = null;
	bridge: SmartConnectionsBridge | null = null;
	private stopGhosts: (() => void) | null = null;
	private modal: FastFindModal | null = null;
	private ready: Promise<void> = Promise.resolve();
	private saveTimer = 0;
	private globsTimer = 0;
	/** Lines of the dictionary note (settings.dictionaryFile), merged into the abbreviations. */
	private dictionaryLines: string[] = [];
	/** Language expansion (German list + dictionary note) for this session; Ctrl+L flips it. */
	languagesActive = false;

	async onload(): Promise<void> {
		// one object for the plugin's lifetime: the bridge and the indexer keep a reference
		this.settings = mergeSettings(await this.loadData());
		if (migrateKeymap(this.settings.keymap, Platform.isMacOS)) await this.saveSettings();
		this.languagesActive = this.settings.languages.length > 0;

		const engine = new EngineClient(workerSource);
		this.engine = engine;
		this.bridge = new SmartConnectionsBridge(this.app, engine, this.settings);
		this.ready = this.startIndexing(engine);
		this.ready.catch((e: unknown) => console.error('Fast Find 10K: startup failed', e));

		this.addCommand({ id: 'search', name: 'Search', callback: () => this.openModal('all') });
		this.addCommand({ id: 'search-files', name: 'Search file names', callback: () => this.openModal('filesOnly') });
		this.addCommand({ id: 'search-resume', name: 'Search (resume last)', callback: () => this.openModal('all', this.settings.lastQuery || null) });
		this.addSettingTab(new FastFindSettingTab(this.app, this));

		this.addCommand({
			id: 'rebuild-index',
			name: 'Rebuild index',
			callback: () => this.rebuildIndex(),
		});
	}

	async saveSettings(): Promise<void> {
		window.clearTimeout(this.saveTimer);
		this.saveTimer = 0;
		await this.saveData(this.settings);
	}

	/** Pushes the ranking settings to the engine (no reindex). */
	applyEngineConfig(): void {
		this.engine?.setConfig(settingsToEngineConfig(this.settings, this.dictionaryLines, this.languagesActive));
	}

	/** Flips language expansion for this session and pushes the config; returns the new state. */
	toggleLanguages(): boolean {
		this.languagesActive = !this.languagesActive;
		this.applyEngineConfig();
		return this.languagesActive;
	}

	/** Reads the dictionary note named in the settings (if any) and pushes the config. */
	async reloadDictionary(): Promise<number> {
		const path = this.settings.dictionaryFile.trim();
		let lines: string[] = [];
		if (path) {
			try {
				if (await this.app.vault.adapter.exists(path)) lines = parseDictionaryLines(await this.app.vault.adapter.read(path));
				else new Notice(`Fast Find 10K: dictionary note not found: ${path}`);
			} catch (e) {
				console.error('Fast Find 10K: reading the dictionary note failed', e);
			}
		}
		this.dictionaryLines = lines;
		this.applyEngineConfig();
		return lines.length;
	}

	/** After an edit of the ignored globs: rebuilds the index once the edits pause for 1 s. */
	scheduleIgnoredRebuild(): void {
		window.clearTimeout(this.globsTimer);
		this.globsTimer = window.setTimeout(() => {
			this.globsTimer = 0;
			void this.rebuildIndex();
		}, 1000);
	}

	async rebuildIndex(): Promise<void> {
		try {
			await this.ready;
			this.indexer?.setIgnoredGlobs(this.settings.ignoredGlobs);
			new Notice('Fast Find 10K: rebuilding the index…');
			await this.indexer?.rebuild();
			new Notice(`Fast Find 10K: rebuilt, ${this.indexer?.status.indexed ?? 0} docs`);
		} catch (e) {
			new Notice(`Fast Find 10K: rebuild failed (${String(e)})`);
		}
	}

	/** Called on every modal close: keeps a non-empty query for "resume last" (saved 1 s later). */
	rememberQuery(query: string): void {
		if (!query.trim() || query === this.settings.lastQuery) return;
		this.settings.lastQuery = query;
		window.clearTimeout(this.saveTimer);
		this.saveTimer = window.setTimeout(() => void this.saveSettings(), 1000);
	}

	/** `resume`: prefill this query with the preview open (the normal search prefills the selection). */
	private async openModal(mode: Mode, resume: string | null = null): Promise<void> {
		try {
			await this.ready;
		} catch (e) {
			new Notice(`Fast Find 10K: engine unavailable (${String(e)})`);
			return;
		}
		this.modal?.close();
		this.modal = new FastFindModal(this.app, this, mode, resume);
		this.modal.open();
		this.bridge?.onModalOpen(); // after the modal is up: the vector check yields between slices
	}

	/** Engine init, cache open, then the indexer; resolves once the engine is searchable. */
	private async startIndexing(engine: EngineClient): Promise<void> {
		await engine.init(settingsToEngineConfig(this.settings, this.dictionaryLines, this.languagesActive));
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
			// the dictionary note: load once the vault is ready, reload when it is edited
			void this.reloadDictionary();
			this.registerEvent(
				this.app.vault.on('modify', (f) => {
					if (f.path === this.settings.dictionaryFile.trim()) void this.reloadDictionary();
				}),
			);
		});
		this.stopGhosts = watchGhosts(this.app, engine, indexer, () => this.settings.logTimings);
		indexer.start().catch((e: unknown) => console.error('Fast Find 10K: indexing failed', e));
		// Smart Connections loads its env late; the bridge polls for it and then hands over the vectors
		this.app.workspace.onLayoutReady(() => this.bridge?.start());
	}

	onunload(): void {
		this.modal?.close(); // its engine is about to go away
		this.modal = null;
		window.clearTimeout(this.globsTimer);
		if (this.saveTimer) void this.saveSettings(); // a pending lastQuery
		this.bridge?.dispose();
		this.bridge = null;
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
