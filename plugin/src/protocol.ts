// Message protocol between the UI thread (EngineClient) and the engine worker,
// plus TS mirrors of the ff10k-core serde types (camelCase; Field/MatchKind are PascalCase).

export type Field = 'Title' | 'Alias' | 'H1' | 'H2' | 'H3' | 'Tag' | 'Link' | 'Path' | 'Body';
export type MatchKind = 'Exact' | 'Prefix' | 'Fuzzy';
export type Mode = 'all' | 'filesOnly';
export type PartialMode = 'whenFew' | 'always' | 'never';

export interface Weights {
	title: number;
	alias: number;
	h1: number;
	h2: number;
	h3: number;
	tag: number;
	link: number;
	path: number;
	body: number;
}

export interface EngineConfig {
	weights: Weights;
	fuzzy: { enabled: boolean; loose: boolean };
	partial: PartialMode;
	downrankedFolders: string[];
	downrankFactor: number;
	fewThreshold: number;
	/** Query words never required, only scored when they hit (compared normalized). */
	optionalTerms: string[];
	/** Abbreviation groups, one per line (`oss: open source, open-source software`). */
	synonyms: string[];
	/** Built-in dictionaries added to the groups (`de`: English ↔ German). */
	languages: string[];
}

export interface SearchOptions {
	mode: Mode;
	limit: number;
	nowSecs: number;
	debug: boolean;
}

export interface HeadingHit {
	text: string;
	line: number;
	level: number;
}

/** `ranges`: UTF-16 code-unit offsets into `text` (the worker converts from Rust's UTF-8 bytes). */
export interface Excerpt {
	text: string;
	ranges: [number, number][];
}

export interface DebugInfo {
	perTerm: [string, Field, MatchKind, number][];
	components: [string, number][];
}

/** `titleRanges`: UTF-16 code-unit offsets into `titleNorm` as delivered to the UI (converted by the worker). */
export interface Hit {
	path: string;
	title: string;
	titleNorm: string;
	tier: number;
	score: number;
	badge: Field;
	titleRanges: [number, number][];
	heading?: HeadingHit | null;
	excerpt?: Excerpt | null;
	line?: number | null;
	isGhost: boolean;
	ghostRefs: number;
	similarCount: number;
	debug?: DebugInfo | null;
}

export interface SearchResponse {
	hits: Hit[];
	totalDocs: number;
	candidates: number;
	elapsedUs: number;
}

/** Externally tagged serde enum: unit variants are strings, newtype variants single-key objects. */
export type SimilarReason =
	| 'sameWordsReordered'
	| 'copySuffix'
	| 'sameTitleDifferentFolder'
	| { similarTitle: number }
	| { semantic: number };

export interface SimilarHit {
	path: string;
	title: string;
	reason: SimilarReason;
	score: number;
}

export interface SemanticHit {
	path: string;
	title: string;
	score: number;
}

export interface Ghost {
	target: string;
	refs: number;
}

export interface DocInput {
	path: string;
	mtime: number;
	text: string;
}

export interface AccessEntry {
	path: string;
	ts: number[];
}

/** `count` opens of `path` from the query whose normalized form is `query`. */
export interface ComboEntry {
	query: string;
	path: string;
	count: number;
}

export type Req =
	| { t: 'init'; id: number; config: EngineConfig }
	| { t: 'config'; config: EngineConfig }
	| { t: 'upsert'; docs: DocInput[] }
	| { t: 'remove'; paths: string[] }
	| { t: 'clear' }
	| { t: 'rename'; old: string; new: string }
	| { t: 'search'; id: number; query: string; mode: Mode; limit: number; debug: boolean }
	| { t: 'similar'; id: number; path: string; limit: number; minScore: number }
	| { t: 'semantic'; id: number; vec: Float32Array; limit: number; minScore: number }
	| { t: 'ghosts'; ghosts: Ghost[] }
	| { t: 'access'; entries: AccessEntry[] }
	| { t: 'track'; path: string; now: number; query?: string }
	| { t: 'combos'; entries: ComboEntry[] }
	| { t: 'clearFrecency' }
	| { t: 'normalizeQuery'; id: number; query: string }
	| { t: 'vectors'; dims: number; paths: string[]; data: Float32Array }
	| { t: 'clearVectors' }
	| { t: 'docText'; id: number; path: string }
	| { t: 'stats'; id: number };

export type Res =
	| { t: 'ready'; id: number }
	| { t: 'upserted'; count: number; total: number }
	| { t: 'results'; id: number; res: SearchResponse; stale?: false }
	| { t: 'results'; id: number; res: null; stale: true }
	| { t: 'similar'; id: number; items: SimilarHit[] }
	| { t: 'semantic'; id: number; items: SemanticHit[] }
	| { t: 'docText'; id: number; text: string | null }
	/** `wasmBytes`: size of the engine's wasm linear memory (diagnostics). */
	| { t: 'stats'; id: number; docs: number; wasmBytes: number }
	| { t: 'normalized'; id: number; query: string }
	| { t: 'vectorsSet'; ok: boolean; error?: string }
	| { t: 'error'; id?: number; message: string };
