// Plugin settings: interface, defaults and the mapping to the engine config. The tab comes later.

import type { EngineConfig, PartialMode, Weights } from './protocol';

export type FuzzySetting = 'off' | 'normal' | 'loose';

export interface FastFindSettings {
	ignoredGlobs: string[];
	downrankedFolders: string[];
	downrankFactor: number;
	weights: Weights;
	fuzzy: FuzzySetting;
	partial: PartialMode;
	fewThreshold: number;
	useSmartConnections: boolean;
	minSimilarity: number;
	createGuard: boolean;
	previewOpen: boolean;
	debugByDefault: boolean;
	logTimings: boolean;
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
	useSmartConnections: true,
	minSimilarity: 0.7,
	createGuard: true,
	previewOpen: true,
	debugByDefault: false,
	logTimings: false,
};

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
