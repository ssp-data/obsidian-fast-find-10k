// The plugin's settings tab (Obsidian `Setting` API only). Ranking changes go to the engine at
// once; ignored globs rebuild the index after a 1 s pause; keys apply on the next modal open.

import { type App, Notice, Platform, PluginSettingTab, Setting } from 'obsidian';
import { validateBinding } from './keys';
import type FastFindPlugin from './main';
import type { PartialMode, Weights } from './protocol';
import { DEFAULT_KEYMAP, DEFAULT_WEIGHTS, type FuzzySetting, type Keymap, type SemanticSetting } from './settings';
import { helpGroups } from './ui/help';

const WEIGHT_NAMES: Record<keyof Weights, string> = {
	title: 'Title',
	alias: 'Alias',
	h1: 'Heading 1',
	h2: 'Heading 2',
	h3: 'Heading 3',
	tag: 'Tag',
	link: 'Link text',
	path: 'Path segment',
	body: 'Body',
};

const lines = (v: string) =>
	v
		.split('\n')
		.map((l) => l.trim())
		.filter((l) => l.length > 0);

export class FastFindSettingTab extends PluginSettingTab {
	private readonly plugin: FastFindPlugin;

	constructor(app: App, plugin: FastFindPlugin) {
		super(app, plugin);
		this.plugin = plugin;
	}

	display(): void {
		const { containerEl } = this;
		containerEl.empty();
		this.indexing(containerEl);
		this.ranking(containerEl);
		this.semantic(containerEl);
		this.behaviour(containerEl);
		this.keys(containerEl);
		this.debug(containerEl);
	}

	private get s() {
		return this.plugin.settings;
	}

	private save(): void {
		void this.plugin.saveSettings();
	}

	/** Saves and hands the ranking settings to the engine. */
	private saveRanking(): void {
		this.save();
		this.plugin.applyEngineConfig();
	}

	private indexing(el: HTMLElement): void {
		new Setting(el).setName('Indexing').setHeading();
		new Setting(el)
			.setName('Ignored folders and globs')
			.setDesc('One per line, in addition to Obsidian\'s "Excluded files" (e.g. Archive/ or **/drafts/*). The index rebuilds 1 s after the last edit.')
			.addTextArea((t) =>
				t.setValue(this.s.ignoredGlobs.join('\n')).onChange((v) => {
					this.s.ignoredGlobs = lines(v);
					this.save();
					this.plugin.scheduleIgnoredRebuild();
				}),
			);
		new Setting(el)
			.setName('Downranked folders')
			.setDesc('One per line. Notes in these folders score × 0.3.')
			.addTextArea((t) =>
				t.setValue(this.s.downrankedFolders.join('\n')).onChange((v) => {
					this.s.downrankedFolders = lines(v);
					this.saveRanking();
				}),
			);
		new Setting(el)
			.setName('Rebuild index')
			.setDesc('Drops the note cache and re-reads every note.')
			.addButton((b) => b.setButtonText('Rebuild index').onClick(() => void this.plugin.rebuildIndex()));
	}

	private ranking(el: HTMLElement): void {
		new Setting(el).setName('Ranking').setHeading();
		for (const k of Object.keys(DEFAULT_WEIGHTS) as (keyof Weights)[]) {
			new Setting(el)
				.setName(`${WEIGHT_NAMES[k]} weight`)
				.setDesc(k === 'body' ? `Default ${DEFAULT_WEIGHTS[k]}. 0 turns the field off.` : `Default ${DEFAULT_WEIGHTS[k]}.`)
				.addSlider((sl) =>
					sl
						.setLimits(0, 150, 5)
						.setValue(this.s.weights[k])
						.setDynamicTooltip()
						.onChange((v) => {
							this.s.weights[k] = v;
							this.saveRanking();
						}),
				);
		}
		new Setting(el).addButton((b) =>
			b.setButtonText('Reset weights').onClick(() => {
				this.s.weights = { ...DEFAULT_WEIGHTS };
				this.saveRanking();
				this.display();
			}),
		);
		new Setting(el)
			.setName('Fuzzy typo tolerance')
			.addDropdown((d) =>
				d
					.addOptions({ off: 'Off', normal: 'Normal', loose: 'Loose' })
					.setValue(this.s.fuzzy)
					.onChange((v) => {
						this.s.fuzzy = v as FuzzySetting;
						this.saveRanking();
					}),
			);
		new Setting(el)
			.setName('Partial-match tier')
			.setDesc('Notes matching only some of the words.')
			.addDropdown((d) =>
				d
					.addOptions({ whenFew: 'Only when few results', always: 'Always', never: 'Never' })
					.setValue(this.s.partial)
					.onChange((v) => {
						this.s.partial = v as PartialMode;
						this.saveRanking();
					}),
			);
	}

	private semantic(el: HTMLElement): void {
		new Setting(el).setName('Semantic').setHeading();
		new Setting(el)
			.setName('Use Smart Connections embeddings')
			.setDesc('Semantic results and similar notes from the Smart Connections plugin, when it is installed.')
			.addToggle((t) =>
				t.setValue(this.s.useSmartConnections).onChange((v) => {
					this.s.useSmartConnections = v;
					this.save();
				}),
			);
		new Setting(el)
			.setName('Minimum similarity shown')
			.setDesc('Cosine score, as Smart Connections shows it. Default 0.70.')
			.addSlider((sl) =>
				sl
					.setLimits(0.5, 0.95, 0.01)
					.setValue(this.s.minSimilarity)
					.setDynamicTooltip()
					.onChange((v) => {
						this.s.minSimilarity = v;
						this.save();
					}),
			);
		new Setting(el)
			.setName('Semantic tier')
			.addDropdown((d) =>
				d
					.addOptions({ auto: 'Automatically when few results', onDemand: 'On demand only', never: 'Never' })
					.setValue(this.s.semantic)
					.onChange((v) => {
						this.s.semantic = v as SemanticSetting;
						this.save();
					}),
			);
	}

	private behaviour(el: HTMLElement): void {
		new Setting(el).setName('Behaviour').setHeading();
		new Setting(el)
			.setName('Create guard')
			.setDesc('Before creating a note, show existing notes with a similar name ("Already have?").')
			.addToggle((t) =>
				t.setValue(this.s.createGuard).onChange((v) => {
					this.s.createGuard = v;
					this.save();
				}),
			);
		new Setting(el)
			.setName('Preview open by default')
			.addToggle((t) =>
				t.setValue(this.s.previewOpen).onChange((v) => {
					this.s.previewOpen = v;
					this.save();
				}),
			);
	}

	private keys(el: HTMLElement): void {
		new Setting(el).setName('Keys').setHeading().setDesc('Like Mod+Shift+Enter (Mod is Cmd on macOS, Ctrl elsewhere). Applies the next time the search opens.');
		const labels = new Map(helpGroups(this.s.keymap).flatMap((g) => g.rows.map((r) => [r.action, r.label])));
		for (const action of Object.keys(DEFAULT_KEYMAP) as (keyof Keymap)[]) {
			const setting = new Setting(el).setName(labels.get(action) ?? action);
			setting.addText((t) => {
				t.setValue(this.s.keymap[action]);
				t.inputEl.addEventListener('blur', () => {
					const spec = t.getValue().trim();
					if (spec === this.s.keymap[action]) return this.markKey(setting, t.inputEl, null);
					const err = validateBinding(this.s.keymap, action, spec, Platform.isMacOS);
					this.markKey(setting, t.inputEl, err); // invalid: the previous binding stays
					if (err) return;
					this.s.keymap[action] = spec;
					this.save();
				});
			});
		}
		new Setting(el).addButton((b) =>
			b.setButtonText('Reset keys').onClick(() => {
				this.s.keymap = { ...DEFAULT_KEYMAP };
				this.save();
				this.display();
			}),
		);
	}

	private markKey(setting: Setting, input: HTMLInputElement, err: string | null): void {
		input.toggleClass('ff10k-invalid', err !== null);
		setting.setDesc(err ? `${err}; keeping the previous binding` : '');
	}

	private debug(el: HTMLElement): void {
		new Setting(el).setName('Debug').setHeading();
		new Setting(el)
			.setName('Show "why ranked" by default')
			.setDesc('The score breakdown under each result (toggle in the search with its key).')
			.addToggle((t) =>
				t.setValue(this.s.debugByDefault).onChange((v) => {
					this.s.debugByDefault = v;
					this.save();
				}),
			);
		new Setting(el)
			.setName('Log engine timings')
			.setDesc('One console line per search.')
			.addToggle((t) =>
				t.setValue(this.s.logTimings).onChange((v) => {
					this.s.logTimings = v;
					this.save();
				}),
			);
		new Setting(el)
			.setName('Clear frecency')
			.setDesc('Forgets which notes you opened and from which queries.')
			.addButton((b) =>
				b
					.setButtonText('Clear frecency')
					.setWarning()
					.onClick(async () => {
						const frecency = this.plugin.frecency;
						if (!frecency) return void new Notice('Fast Find 10K: still starting, try again in a moment');
						try {
							await frecency.clear();
							new Notice('Fast Find 10K: frecency cleared');
						} catch (e) {
							new Notice(`Fast Find 10K: clearing frecency failed (${String(e)})`);
						}
					}),
			);
	}
}
