// Plugin settings: interface, defaults, merging saved data, and the mapping to the engine config.
// The settings tab is in settings-tab.ts.

import type { EngineConfig, PartialMode, Weights } from './protocol';

export type FuzzySetting = 'off' | 'normal' | 'loose';
/** Semantic tier: automatically when few lexical hits (and on Ctrl+S), only on Ctrl+S, or never. */
export type SemanticSetting = 'auto' | 'onDemand' | 'never';

/** Modal key bindings as `Mod+Alt+Enter` style strings (`Mod` = Cmd on macOS, Ctrl elsewhere). */
export interface Keymap {
	open: string;
	openTab: string;
	openSplit: string;
	create: string;
	createTab: string;
	filesOnly: string;
	semantic: string;
	moveDown: string;
	moveUp: string;
	togglePreview: string;
	toggleDebug: string;
	focusSimilar: string;
	previewDown: string;
	previewUp: string;
	showHelp: string;
	openAll: string;
}

export const DEFAULT_KEYMAP: Keymap = {
	open: 'Enter',
	openTab: 'Mod+Enter',
	openSplit: 'Mod+Alt+Enter',
	create: 'Shift+Enter',
	createTab: 'Mod+Shift+Enter',
	filesOnly: 'Ctrl+F',
	semantic: 'Ctrl+S',
	moveDown: 'Ctrl+J',
	moveUp: 'Ctrl+K',
	togglePreview: 'Ctrl+P',
	toggleDebug: 'Ctrl+/',
	focusSimilar: 'Tab',
	previewDown: 'Shift+J',
	previewUp: 'Shift+K',
	showHelp: 'Ctrl+Shift+/',
	// Ctrl+Shift+Enter is createTab (Mod+Shift+Enter) off macOS
	openAll: 'Alt+Shift+Enter',
};

export interface FastFindSettings {
	ignoredGlobs: string[];
	downrankedFolders: string[];
	downrankFactor: number;
	weights: Weights;
	fuzzy: FuzzySetting;
	partial: PartialMode;
	fewThreshold: number;
	semantic: SemanticSetting;
	useSmartConnections: boolean;
	minSimilarity: number;
	createGuard: boolean;
	previewOpen: boolean;
	debugByDefault: boolean;
	logTimings: boolean;
	keymap: Keymap;
	/** Query of the last modal close (non-empty), for "Search (resume last)". */
	lastQuery: string;
}

export const DEFAULT_WEIGHTS: Weights = {
	title: 100,
	alias: 90,
	h1: 60,
	h2: 50,
	h3: 40,
	tag: 30,
	link: 20,
	path: 15,
	body: 10,
};

export const DEFAULT_SETTINGS: FastFindSettings = {
	ignoredGlobs: [],
	downrankedFolders: [],
	downrankFactor: 0.3,
	weights: { ...DEFAULT_WEIGHTS },
	fuzzy: 'normal',
	partial: 'whenFew',
	fewThreshold: 5,
	semantic: 'auto',
	useSmartConnections: true,
	minSimilarity: 0.7,
	createGuard: true,
	previewOpen: true,
	debugByDefault: false,
	logTimings: false,
	keymap: { ...DEFAULT_KEYMAP },
	lastQuery: '',
};

/** Keeps the keys of `defaults`, taking each saved value whose type matches the default's
 * (arrays: arrays of strings). Unknown keys are dropped. */
function pick<T extends object>(defaults: T, saved: unknown): T {
	const out = { ...defaults };
	if (typeof saved !== 'object' || saved === null) return out;
	const src = saved as Record<string, unknown>;
	for (const k of Object.keys(defaults) as (keyof T & string)[]) {
		const v = src[k];
		const d = defaults[k];
		if (Array.isArray(d) ? Array.isArray(v) && v.every((x) => typeof x === 'string') : typeof v === typeof d) out[k] = v as T[typeof k];
	}
	return out;
}

/** Saved plugin data merged over the defaults, deep for `weights` and `keymap`: a key missing
 * from the saved data keeps its default; unknown or mistyped keys are dropped. */
export function mergeSettings(saved: unknown): FastFindSettings {
	const s = pick(DEFAULT_SETTINGS, saved);
	const src = (typeof saved === 'object' && saved !== null ? saved : {}) as Record<string, unknown>;
	s.weights = pick(DEFAULT_WEIGHTS, src.weights);
	s.keymap = pick(DEFAULT_KEYMAP, src.keymap);
	s.ignoredGlobs = [...s.ignoredGlobs];
	s.downrankedFolders = [...s.downrankedFolders];
	return s;
}

export function settingsToEngineConfig(s: FastFindSettings): EngineConfig {
	return {
		weights: { ...s.weights },
		fuzzy: { enabled: s.fuzzy !== 'off', loose: s.fuzzy === 'loose' },
		partial: s.partial,
		downrankedFolders: [...s.downrankedFolders],
		downrankFactor: s.downrankFactor,
		fewThreshold: s.fewThreshold,
	};
}
