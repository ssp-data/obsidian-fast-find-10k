// Pure helpers of the modal: title-range mapping and keymap parsing (no Obsidian runtime).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

const obsidianStub = {
	name: 'obsidian-stub',
	setup(build) {
		build.onResolve({ filter: /^obsidian$/ }, () => ({ path: 'obsidian', namespace: 'stub' }));
		build.onLoad({ filter: /.*/, namespace: 'stub' }, () => ({
			contents: 'export class Modal {} export class Notice {} export class MarkdownView {} export class TFile {} export class Scope {} export const Platform = {}; export const normalizePath = (p) => p;',
			loader: 'js',
		}));
	},
};

async function load(entry) {
	const built = await esbuild.build({ entryPoints: [entry], bundle: true, format: 'esm', write: false, plugins: [obsidianStub], absWorkingDir: new URL('..', import.meta.url).pathname });
	return import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));
}

const { mapTitleRanges } = await load('src/ui/results.ts');
const { parseKeySpec } = await load('src/ui/modal.ts');

test('title ranges map case-insensitively onto the display title', () => {
	assert.deepEqual(mapTitleRanges('Obsidian Troubleshoot', 'obsidian troubleshoot', [[0, 8], [9, 14]]), [[0, 8], [9, 14]]);
});

test('title ranges follow shifted positions in the display title', () => {
	// display has an emoji prefix the normalized title lacks
	assert.deepEqual(mapTitleRanges('👉🏻 Data Engineering', 'data engineering', [[0, 4], [5, 13]]), [[5, 9], [10, 18]]);
});

test('a range not found in the display title is skipped, others kept', () => {
	assert.deepEqual(mapTitleRanges('Café Ölmühle', 'cafe olmuhle', [[0, 4], [5, 7]]), []);
	assert.deepEqual(mapTitleRanges('Café AI', 'cafe ai', [[0, 4], [5, 7]]), [[5, 7]]);
});

test('repeated substrings map to successive occurrences', () => {
	assert.deepEqual(mapTitleRanges('AI and AI', 'ai and ai', [[0, 2], [7, 9]]), [[0, 2], [7, 9]]);
});

test('bad ranges never throw', () => {
	assert.deepEqual(mapTitleRanges('abc', 'abc', [[2, 1], [5, 9], [0, 0]]), []);
});

test('key specs split into modifiers and key', () => {
	assert.deepEqual(parseKeySpec('Enter'), { modifiers: [], key: 'Enter' });
	assert.deepEqual(parseKeySpec('Mod+Alt+Enter'), { modifiers: ['Mod', 'Alt'], key: 'Enter' });
	assert.deepEqual(parseKeySpec('Ctrl+/'), { modifiers: ['Ctrl'], key: '/' });
	assert.deepEqual(parseKeySpec('Ctrl+F'), { modifiers: ['Ctrl'], key: 'f' });
	assert.equal(parseKeySpec(''), null);
	assert.equal(parseKeySpec('Hyper+X'), null);
});

// --- create guard: semantic candidates and the lexical-first flow ---
const { semanticGuard, runGuard } = await load('src/ui/modal.ts');

const vecBridge = (embed) => ({ available: () => true, hasVectors: () => true, embed });
const semEngine = { semantic: async () => [{ path: 'x.md', title: 'X', score: 0.812 }] };
const settings = (semantic) => ({ useSmartConnections: true, semantic, minSimilarity: 0.7 });

test('guard semantic rows carry the score; never embeds with semantic = never', async () => {
	let embeds = 0;
	const bridge = vecBridge(async () => (embeds++, new Float32Array([1])));
	assert.deepEqual(await semanticGuard('n', { settings: settings('auto'), bridge, engine: semEngine }), [{ path: 'x.md', title: 'X', note: '0.81' }]);
	assert.equal(embeds, 1);
	assert.deepEqual(await semanticGuard('n', { settings: settings('never'), bridge, engine: semEngine }), []);
	assert.equal(embeds, 1, 'embed not called with never');
});

test('guard shows lexical candidates at once while the embedding hangs, and appends semantic ones later', async () => {
	const shown = [];
	const ui = { current: () => true, show: (items) => shown.push(['show', items.map((i) => i.path)]), update: (items) => shown.push(['update', items.map((i) => i.path)]), create: () => shown.push(['create']) };
	await runGuard([{ path: 'a.md', title: 'A', note: 'title' }], new Promise(() => {}), ui);
	assert.deepEqual(shown, [['show', ['a.md']]]);

	shown.length = 0;
	let resolve;
	await runGuard([{ path: 'a.md', title: 'A', note: 'title' }], new Promise((r) => (resolve = r)), ui);
	resolve([{ path: 'a.md', title: 'A', note: '0.90' }, { path: 'b.md', title: 'B', note: '0.75' }]);
	await new Promise((r) => setTimeout(r, 0));
	assert.deepEqual(shown, [['show', ['a.md']], ['update', ['a.md', 'b.md']]]);
});

test('guard creates when there are no candidates (a timed-out embedding yields none)', async () => {
	const shown = [];
	const ui = { current: () => true, show: () => shown.push('show'), update: () => shown.push('update'), create: () => shown.push('create') };
	await runGuard([], Promise.resolve([]), ui);
	assert.deepEqual(shown, ['create']);
	shown.length = 0;
	await runGuard([], Promise.resolve([{ path: 'b.md', title: 'B', note: '0.75' }]), ui);
	assert.deepEqual(shown, ['show']);
	shown.length = 0;
	await runGuard([], Promise.resolve([]), { ...ui, current: () => false });
	assert.deepEqual(shown, [], 'superseded: nothing happens');
});
