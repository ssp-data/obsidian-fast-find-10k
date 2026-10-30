// Plugin settings: interface, defaults, merging saved data, and the mapping to the engine config.
// The settings tab is in settings-tab.ts.

import { validateBinding } from './keys';
import type { EngineConfig, PartialMode, Weights } from './protocol';

export type FuzzySetting = 'off' | 'normal' | 'loose';
/** Semantic tier: automatically when few lexical hits (and on Ctrl+S), only on Ctrl+S, or never. */
export type SemanticSetting = 'always' | 'auto' | 'onDemand' | 'never';
export type KeyHintsSetting = 'desktop' | 'always' | 'never';

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
	toggleLanguages: string;
	panelRight: string;
	panelLeft: string;
	clearQuery: string;
	nextMatch: string;
	prevMatch: string;
	toggleMatchesOnly: string;
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
	// Ctrl+R is Obsidian's "Reload app"; the modal swallows it while open
	toggleDebug: 'Ctrl+R',
	focusSimilar: 'Tab',
	previewDown: 'Shift+J',
	previewUp: 'Shift+K',
	showHelp: 'Ctrl+Shift+/',
	// Ctrl+Shift+Enter is createTab (Mod+Shift+Enter) off macOS
	openAll: 'Alt+Shift+Enter',
	// Ctrl+L is Obsidian's editor "toggle checkbox"; the modal swallows it while open
	toggleLanguages: 'Ctrl+L',
	// vim h/l: similar panel on the right, results on the left (Tab still toggles)
	panelRight: 'Shift+L',
	panelLeft: 'Shift+H',
	// clears the query (the last query is still there for "Search (resume last)"); with text
	// selected in the field it stays the copy shortcut
	clearQuery: 'Ctrl+C',
	// preview: jump between highlighted matches (vim n / N), and "matches only"
	nextMatch: 'Alt+N',
	prevMatch: 'Alt+Shift+N',
	toggleMatchesOnly: 'Ctrl+M',
};

export interface FastFindSettings {
	ignoredGlobs: string[];
	downrankedFolders: string[];
	downrankFactor: number;
	weights: Weights;
	fuzzy: FuzzySetting;
	partial: PartialMode;
	fewThreshold: number;
	/** Query words that are never required (stopwords, single letters), only scored. */
	optionalTerms: string[];
	/** Abbreviation groups, one per line: `oss: open source, open-source software`. */
	synonyms: string[];
	/** Built-in dictionaries added to the abbreviation groups: `de` = English ↔ German. */
	languages: string[];
	/** Vault path of a note whose `word: other, words` lines join the abbreviation groups ('' = none). */
	dictionaryFile: string;
	semantic: SemanticSetting;
	useSmartConnections: boolean;
	minSimilarity: number;
	/** Cosine floor for the semantic tier and the create guard (a short query sits further from a
	 * note than two notes from each other, so this is lower than `minSimilarity`). */
	semanticMinSimilarity: number;
	createGuard: boolean;
	previewOpen: boolean;
	/** Preview open when the search opens on a phone or tablet (no Ctrl+P there; a pill toggles it). */
	previewOpenMobile: boolean;
	/** Footer key hints: shown on desktop only (default), always, or never. */
	keyHints: KeyHintsSetting;
	/** A ribbon icon (left bar on desktop, the drawer on mobile) that opens the search. */
	ribbonIcon: boolean;
	debugByDefault: boolean;
	logTimings: boolean;
	keymap: Keymap;
	/** Query of the last modal close (non-empty), for "Search (resume last)". */
	lastQuery: string;
}

export const DEFAULT_WEIGHTS: Weights = {
	title: 100,
	alias: 90,
	h1: 105,
	h2: 65,
	h3: 55,
	tag: 30,
	link: 20,
	path: 15,
	body: 10,
	property: 25,
};

export const DEFAULT_SETTINGS: FastFindSettings = {
	ignoredGlobs: [],
	downrankedFolders: [],
	downrankFactor: 0.3,
	weights: { ...DEFAULT_WEIGHTS },
	fuzzy: 'normal',
	partial: 'whenFew',
	fewThreshold: 5,
	optionalTerms: ['a', 'an', 'the', 'of', 'to', 'in', 'on', 'i'],
	synonyms: [],
	languages: [],
	dictionaryFile: '',
	semantic: 'always',
	useSmartConnections: true,
	minSimilarity: 0.7,
	semanticMinSimilarity: 0.6,
	createGuard: true,
	previewOpen: true,
	previewOpenMobile: false,
	keyHints: 'desktop',
	ribbonIcon: true,
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

const FUZZY_VALUES: readonly FuzzySetting[] = ['off', 'normal', 'loose'];
const PARTIAL_VALUES: readonly PartialMode[] = ['whenFew', 'always', 'never'];
const SEMANTIC_VALUES: readonly SemanticSetting[] = ['always', 'auto', 'onDemand', 'never'];
const KEY_HINTS_VALUES: readonly KeyHintsSetting[] = ['desktop', 'always', 'never'];

/** `v` when it is one of `allowed`, else `fallback`. */
function oneOf<T extends string>(v: T, allowed: readonly T[], fallback: T): T {
	return allowed.includes(v) ? v : fallback;
}

/** `v` clamped to `[lo, hi]` (rounded when `int`); `fallback` when not finite. */
function clamp(v: number, lo: number, hi: number, fallback: number, int = false): number {
	if (!Number.isFinite(v)) return fallback;
	const c = Math.min(hi, Math.max(lo, v));
	return int ? Math.round(c) : c;
}

/** Saved plugin data merged over the defaults, deep for `weights` and `keymap`: a key missing
 * from the saved data keeps its default; unknown or mistyped keys are dropped. Enum values
 * outside their set fall back to the default and numbers are clamped to the ranges the settings
 * tab offers, so a hand-edited data.json cannot make the engine reject its config. */
export function mergeSettings(saved: unknown): FastFindSettings {
	const s = pick(DEFAULT_SETTINGS, saved);
	const src = (typeof saved === 'object' && saved !== null ? saved : {}) as Record<string, unknown>;
	const d = DEFAULT_SETTINGS;
	s.weights = pick(DEFAULT_WEIGHTS, src.weights);
	for (const k of Object.keys(DEFAULT_WEIGHTS) as (keyof Weights)[]) {
		s.weights[k] = clamp(s.weights[k], 0, 150, DEFAULT_WEIGHTS[k], true);
	}
	s.fuzzy = oneOf(s.fuzzy, FUZZY_VALUES, d.fuzzy);
	s.partial = oneOf(s.partial, PARTIAL_VALUES, d.partial);
	s.semantic = oneOf(s.semantic, SEMANTIC_VALUES, d.semantic);
	s.keyHints = oneOf(s.keyHints, KEY_HINTS_VALUES, d.keyHints);
	s.fewThreshold = clamp(s.fewThreshold, 1, 50, d.fewThreshold, true);
	s.minSimilarity = clamp(s.minSimilarity, 0.5, 0.95, d.minSimilarity);
	s.semanticMinSimilarity = clamp(s.semanticMinSimilarity, 0.3, 0.95, d.semanticMinSimilarity);
	s.downrankFactor = clamp(s.downrankFactor, 0, 1, d.downrankFactor);
	s.keymap = pick(DEFAULT_KEYMAP, src.keymap);
	s.ignoredGlobs = [...s.ignoredGlobs];
	s.downrankedFolders = [...s.downrankedFolders];
	s.optionalTerms = [...s.optionalTerms];
	s.synonyms = [...s.synonyms];
	s.languages = [...s.languages];
	return s;
}

/** Moves a saved why-ranked binding still on the old default `Ctrl+/` to the new `Ctrl+R`,
 * unless another saved binding already uses Ctrl+R. True when it changed `km` (then save). */
export function migrateKeymap(km: Keymap, isMac: boolean): boolean {
	if (km.toggleDebug !== 'Ctrl+/' || validateBinding(km, 'toggleDebug', 'Ctrl+R', isMac) !== null) return false;
	km.toggleDebug = 'Ctrl+R';
	return true;
}

/** The abbreviation lines of a dictionary note: lines with a `:` (list markers and surrounding
 * whitespace stripped), skipping blank lines, `#` headings/comments and frontmatter. */
export function parseDictionaryLines(text: string): string[] {
	let body = text;
	if (body.startsWith('---\n')) {
		const end = body.indexOf('\n---', 4);
		if (end >= 0) body = body.slice(end + 4);
	}
	const out: string[] = [];
	for (const raw of body.split('\n')) {
		const line = raw.trim().replace(/^[-*+]\s+/, '').replace(/^\d+[.)]\s+/, '').trim();
		if (!line || line.startsWith('#') || !line.includes(':')) continue;
		out.push(line);
	}
	return out;
}

/** The engine config for `s`; `languages` (the built-in lists and the dictionary note's lines)
 * only when language expansion is active (Ctrl+L in the search, or the setting at startup). */
export function settingsToEngineConfig(s: FastFindSettings, extraSynonyms: readonly string[] = [], languagesActive = false): EngineConfig {
	return {
		weights: { ...s.weights },
		fuzzy: { enabled: s.fuzzy !== 'off', loose: s.fuzzy === 'loose' },
		partial: s.partial,
		downrankedFolders: [...s.downrankedFolders],
		downrankFactor: s.downrankFactor,
		fewThreshold: s.fewThreshold,
		optionalTerms: [...s.optionalTerms],
		synonyms: languagesActive ? [...s.synonyms, ...extraSynonyms] : [...s.synonyms],
		languages: languagesActive ? (s.languages.length > 0 ? [...s.languages] : ['de']) : [],
		similarMinScore: s.minSimilarity,
	};
}
