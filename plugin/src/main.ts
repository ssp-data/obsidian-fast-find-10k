import { Notice, Plugin } from 'obsidian';
import workerSource from 'virtual:worker-source';
import { EngineClient } from './engine-client';
import { DEFAULT_SETTINGS, type FastFindSettings, settingsToEngineConfig } from './settings';

export default class FastFindPlugin extends Plugin {
	settings: FastFindSettings = { ...DEFAULT_SETTINGS };
	engine: EngineClient | null = null;
	private ready: Promise<void> = Promise.resolve();

	async onload(): Promise<void> {
		const saved = (await this.loadData()) as Partial<FastFindSettings> | null;
		this.settings = { ...DEFAULT_SETTINGS, ...saved };

		const engine = new EngineClient(workerSource);
		this.engine = engine;
		this.ready = engine.init(settingsToEngineConfig(this.settings));
		this.ready.catch((e: unknown) => console.error('Fast Find 10K: engine init failed', e));

		this.addCommand({
			id: 'search',
			name: 'Search',
			callback: async () => {
				try {
					await this.ready;
					const docs = await engine.stats();
					new Notice(`Fast Find 10K ready, ${docs} docs`);
				} catch (e) {
					new Notice(`Fast Find 10K: engine unavailable (${String(e)})`);
				}
			},
		});
	}

	onunload(): void {
		this.engine?.dispose();
		this.engine = null;
	}
}
