// The plugin's settings tab. On Obsidian 1.13+ the tab is declarative (`getSettingDefinitions`,
// so every setting is found by the settings search); older versions get the same tab rendered
// with the `Setting` API (`legacyDisplay`). Ranking changes go to the engine at once; ignored
// globs rebuild the index after a 1 s pause; keys apply on the next modal open.

import { type App, Notice, Platform, PluginSettingTab, requireApiVersion, Setting, type SettingDefinition, type SettingDefinitionItem } from 'obsidian';
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

type ListKey = 'ignoredGlobs' | 'downrankedFolders' | 'optionalTerms' | 'synonyms';
/** Settings edited as one entry per line. */
const LIST_KEYS: ReadonlySet<string> = new Set<ListKey>(['ignoredGlobs', 'downrankedFolders', 'optionalTerms', 'synonyms']);
/** Plain settings the engine must hear about at once. */
const RANKING_KEYS: ReadonlySet<string> = new Set(['fuzzy', 'partial']);

export class FastFindSettingTab extends PluginSettingTab {
	private readonly plugin: FastFindPlugin;

	constructor(app: App, plugin: FastFindPlugin) {
		super(app, plugin);
		this.plugin = plugin;
	}

	display(): void {
		if (requireApiVersion('1.13.0')) {
			super.display(); // renders getSettingDefinitions()
			return;
		}
		this.legacyDisplay();
	}

	private legacyDisplay(): void {
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

	// ---- declarative tab (Obsidian 1.13+) ----

	getSettingDefinitions(): SettingDefinitionItem[] {
		const labels = new Map(helpGroups(this.s.keymap).flatMap((g) => g.rows.map((r) => [r.action, r.label])));
		const weights: SettingDefinition[] = (Object.keys(DEFAULT_WEIGHTS) as (keyof Weights)[]).map((k) => ({
			name: `${WEIGHT_NAMES[k]} weight`,
			desc: k === 'body' ? `Default ${DEFAULT_WEIGHTS[k]}. 0 turns the field off.` : `Default ${DEFAULT_WEIGHTS[k]}.`,
			control: { type: 'slider', key: `weights.${k}`, min: 0, max: 150, step: 5 },
		}));
		const keys: SettingDefinition[] = (Object.keys(DEFAULT_KEYMAP) as (keyof Keymap)[]).map((action) => ({
			name: labels.get(action) ?? action,
			aliases: ['key', 'shortcut', 'hotkey'],
			control: {
				type: 'text',
				key: `keymap.${action}`,
				validate: (v: string) => validateBinding(this.s.keymap, action, v.trim(), Platform.isMacOS) ?? undefined,
			},
		}));
		return [
			{
				type: 'group',
				heading: 'Indexing',
				items: [
					{
						name: 'Ignored folders and globs',
						desc: 'One per line, in addition to Obsidian\'s "Excluded files" (e.g. Archive/ or **/drafts/*). The index rebuilds 1 s after the last edit.',
						control: { type: 'textarea', key: 'ignoredGlobs' },
					},
					{ name: 'Downranked folders', desc: 'One per line. Notes in these folders score × 0.3.', control: { type: 'textarea', key: 'downrankedFolders' } },
					{ name: 'Rebuild index', desc: 'Drops the note cache and re-reads every note.', action: () => void this.plugin.rebuildIndex() },
				],
			},
			{
				type: 'group',
				heading: 'Ranking',
				items: [
					...weights,
					{
						name: 'Reset weights',
						desc: 'Back to the defaults.',
						searchable: false,
						action: () => {
							this.s.weights = { ...DEFAULT_WEIGHTS };
							this.saveRanking();
							this.update();
						},
					},
					{ name: 'Fuzzy typo tolerance', control: { type: 'dropdown', key: 'fuzzy', options: { off: 'Off', normal: 'Normal', loose: 'Loose' } } },
					{
						name: 'Partial-match tier',
						desc: 'Notes matching only some of the words.',
						control: { type: 'dropdown', key: 'partial', options: { whenFew: 'Only when few results', always: 'Always', never: 'Never' } },
					},
					{
						name: 'Optional words (never required)',
						desc: 'One per line. A query word on this list need not match; where it does, it still adds to the score. Default: a an the of to in on i.',
						control: { type: 'textarea', key: 'optionalTerms' },
					},
					{
						name: 'Abbreviations and synonyms',
						desc: 'One group per line, members separated by commas (the first may end in a colon). Typing a single-word member also finds the others: with "oss: open source, open-source software", the query oss matches notes with OSS, "open source" or "open-source software" in the title, headings or body. Several-word members are found, not expanded: "open source" does not search for oss.',
						aliases: ['synonym', 'abbreviation', 'expansion'],
						control: { type: 'textarea', key: 'synonyms', placeholder: 'oss: open source, open-source software\nk8s: kubernetes' },
					},
					{
						name: 'Dictionary note',
						desc: 'Path of a note in this vault (for example "Dictionary.md") whose lines "english: german, german" join the abbreviations above, so the list can be kept in Obsidian. Headings, blank lines and lines without a colon are ignored; list markers are fine. Reloaded when the note is edited.',
						aliases: ['translation', 'language'],
						control: { type: 'text', key: 'dictionaryFile', placeholder: 'Dictionary.md' },
					},
					{
						name: 'German words on at startup (English ↔ German)',
						desc: 'Language expansion adds a built-in list of about 400 everyday English–German word pairs and the dictionary note to the abbreviations, both directions: school finds Schule and Schulen, Kinder finds children. Umlauts are folded. In the search, Ctrl+L (the "languages" key) switches it on or off for the session; this toggle sets the state at startup. Off by default.',
						aliases: ['language', 'deutsch', 'translation'],
						control: { type: 'toggle', key: 'languagesDe' },
					},
				],
			},
			{
				type: 'group',
				heading: 'Semantic',
				items: [
					{
						name: 'Use Smart Connections embeddings',
						desc: 'Semantic results and similar notes from the Smart Connections plugin, when it is installed.',
						control: { type: 'toggle', key: 'useSmartConnections' },
					},
					{
						name: 'Minimum similarity shown',
						desc: 'Cosine score for the similar-notes panel (note to note), as Smart Connections shows it. Default 0.70.',
						control: { type: 'slider', key: 'minSimilarity', min: 0.5, max: 0.95, step: 0.01 },
					},
					{
						name: 'Minimum similarity for semantic results',
						desc: 'Cosine floor for the semantic tier (Ctrl+S or automatic) and the create guard. A short query sits further from a note than two notes from each other, so this is lower: typical top hits score 0.65–0.8. Default 0.60.',
						control: { type: 'slider', key: 'semanticMinSimilarity', min: 0.3, max: 0.95, step: 0.01 },
					},
					{
						name: 'Semantic tier',
						desc: 'Smart Connections neighbours of the query as the last group. Always: after every query (a few ms). When few results: only with fewer than the "few results" threshold of full matches. On demand: Ctrl+S.',
						control: { type: 'dropdown', key: 'semantic', options: { always: 'Always', auto: 'Only when few results', onDemand: 'On demand only (Ctrl+S)', never: 'Never' } },
					},
				],
			},
			{
				type: 'group',
				heading: 'Behaviour',
				items: [
					{ name: 'Create guard', desc: 'Before creating a note, show existing notes with a similar name ("Already have?").', control: { type: 'toggle', key: 'createGuard' } },
					{ name: 'Preview open by default', control: { type: 'toggle', key: 'previewOpen' } },
				],
			},
			{
				type: 'group',
				heading: 'Keys',
				items: [
					{ name: 'Keys', desc: 'Like Mod+Shift+Enter (Mod is Cmd on macOS, Ctrl elsewhere). Applies the next time the search opens.', searchable: false },
					...keys,
					{
						name: 'Reset keys',
						desc: 'Back to the default bindings.',
						searchable: false,
						action: () => {
							this.s.keymap = { ...DEFAULT_KEYMAP };
							this.save();
							this.update();
						},
					},
				],
			},
			{
				type: 'group',
				heading: 'Debug',
				items: [
					{ name: 'Show "why ranked" by default', desc: 'The score breakdown under each result (toggle in the search with its key).', control: { type: 'toggle', key: 'debugByDefault' } },
					{ name: 'Log engine timings', desc: 'One console line per search, plus index and vector load summaries.', control: { type: 'toggle', key: 'logTimings' } },
					{ name: 'Clear frecency', desc: 'Forgets which notes you opened and from which queries.', action: () => void this.clearFrecency() },
				],
			},
		];
	}

	/** Declarative controls read through here: dotted keys reach into weights/keymap, lists are one entry per line. */
	getControlValue(key: string): unknown {
		if (key === 'languagesDe') return this.s.languages.includes('de');
		if (LIST_KEYS.has(key)) return (this.s[key as ListKey] as string[]).join('\n');
		if (key.startsWith('weights.')) return this.s.weights[key.slice(8) as keyof Weights];
		if (key.startsWith('keymap.')) return this.s.keymap[key.slice(7) as keyof Keymap];
		return (this.s as unknown as Record<string, unknown>)[key];
	}

	setControlValue(key: string, value: unknown): void {
		if (key === 'languagesDe') {
			this.s.languages = value ? [...this.s.languages.filter((l) => l !== 'de'), 'de'] : this.s.languages.filter((l) => l !== 'de');
			return this.saveRanking();
		}
		if (LIST_KEYS.has(key)) {
			(this.s as unknown as Record<string, string[]>)[key] = lines(String(value ?? ''));
			if (key === 'ignoredGlobs') {
				this.save();
				return this.plugin.scheduleIgnoredRebuild();
			}
			return this.saveRanking();
		}
		if (key.startsWith('weights.')) {
			const n = Number(value);
			this.s.weights[key.slice(8) as keyof Weights] = Number.isFinite(n) ? Math.min(150, Math.max(0, Math.round(n))) : 0;
			return this.saveRanking();
		}
		if (key.startsWith('keymap.')) {
			this.s.keymap[key.slice(7) as keyof Keymap] = String(value ?? '').trim();
			return this.save();
		}
		if (key === 'dictionaryFile') {
			this.s.dictionaryFile = String(value ?? '').trim();
			this.save();
			void this.plugin.reloadDictionary().then((n) => {
				if (this.s.dictionaryFile) new Notice(`Fast Find 10K: ${n} dictionary lines loaded`);
			});
			return;
		}
		(this.s as unknown as Record<string, unknown>)[key] = value;
		if (RANKING_KEYS.has(key)) this.saveRanking();
		else this.save();
	}

	private async clearFrecency(): Promise<void> {
		const frecency = this.plugin.frecency;
		if (!frecency) return void new Notice('Fast Find 10K: still starting, try again in a moment');
		try {
			await frecency.clear();
			new Notice('Fast Find 10K: frecency cleared');
		} catch (e) {
			new Notice(`Fast Find 10K: clearing frecency failed (${String(e)})`);
		}
	}

	// ---- imperative tab (Obsidian < 1.13) ----

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
				this.legacyDisplay();
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
		new Setting(el)
			.setName('Optional words (never required)')
			.setDesc('One per line. A query word on this list need not match; where it does, it still adds to the score. Default: a an the of to in on i.')
			.addTextArea((t) =>
				t.setValue(this.s.optionalTerms.join('\n')).onChange((v) => {
					this.s.optionalTerms = lines(v);
					this.saveRanking();
				}),
			);
		new Setting(el)
			.setName('Abbreviations and synonyms')
			.setDesc(
				'One group per line, members separated by commas (the first may end in a colon). Typing a single-word member also finds the others: with "oss: open source, open-source software", the query oss matches notes with OSS, "open source" or "open-source software" in the title, headings or body. Several-word members are found, not expanded: "open source" does not search for oss.',
			)
			.addTextArea((t) => {
				t.setPlaceholder('oss: open source, open-source software\nk8s: kubernetes');
				t.setValue(this.s.synonyms.join('\n')).onChange((v) => {
					this.s.synonyms = lines(v);
					this.saveRanking();
				});
			});
		new Setting(el)
			.setName('Dictionary note')
			.setDesc(
				'Path of a note in this vault (for example "Dictionary.md") whose lines "english: german, german" join the abbreviations above, so the list can be kept in Obsidian. Headings, blank lines and lines without a colon are ignored; list markers are fine. Reloaded when the note is edited.',
			)
			.addText((t) => {
				t.setPlaceholder('Dictionary.md');
				t.setValue(this.s.dictionaryFile).onChange((v) => {
					this.s.dictionaryFile = v.trim();
					this.save();
					void this.plugin.reloadDictionary().then((n) => {
						if (this.s.dictionaryFile) new Notice(`Fast Find 10K: ${n} dictionary lines loaded`);
					});
				});
			});
		new Setting(el)
			.setName('German words on at startup (English ↔ German)')
			.setDesc(
				'Language expansion adds a built-in list of about 400 everyday English–German word pairs and the dictionary note to the abbreviations, both directions: school finds Schule and Schulen, Kinder finds children. Umlauts are folded. In the search, Ctrl+L (the "languages" key) switches it on or off for the session; this toggle sets the state at startup. Off by default.',
			)
			.addToggle((t) =>
				t.setValue(this.s.languages.includes('de')).onChange((v) => {
					this.s.languages = v ? [...this.s.languages.filter((l) => l !== 'de'), 'de'] : this.s.languages.filter((l) => l !== 'de');
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
			.setDesc('Cosine score for the similar-notes panel (note to note), as Smart Connections shows it. Default 0.70.')
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
			.setName('Minimum similarity for semantic results')
			.setDesc('Cosine floor for the semantic tier (Ctrl+S or automatic) and the create guard; lower than the panel\'s, since a short query sits further from a note. Default 0.60.')
			.addSlider((sl) =>
				sl
					.setLimits(0.3, 0.95, 0.01)
					.setValue(this.s.semanticMinSimilarity)
					.setDynamicTooltip()
					.onChange((v) => {
						this.s.semanticMinSimilarity = v;
						this.save();
					}),
			);
		new Setting(el)
			.setName('Semantic tier')
			.addDropdown((d) =>
				d
					.addOptions({ always: 'Always', auto: 'Only when few results', onDemand: 'On demand only (Ctrl+S)', never: 'Never' })
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
				this.legacyDisplay();
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
			.addButton((b) => b.setButtonText('Clear frecency').setWarning().onClick(() => void this.clearFrecency()));
	}
}
