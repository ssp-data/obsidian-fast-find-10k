// Pure helpers of the modal: title ranges, create guard, why-ranked line, open all (no Obsidian runtime).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

const obsidianStub = {
	name: 'obsidian-stub',
	setup(build) {
		build.onResolve({ filter: /^obsidian$/ }, () => ({ path: 'obsidian', namespace: 'stub' }));
		build.onLoad({ filter: /.*/, namespace: 'stub' }, () => ({
			contents: 'export class Modal {} export class Notice {} export class MarkdownView {} export class TFile {} export class Scope {} export class Component { load() {} unload() {} } export class MarkdownRenderer { static async render() {} } export const Platform = {}; export const normalizePath = (p) => p;',
			loader: 'js',
		}));
	},
};

async function load(entry) {
	const built = await esbuild.build({ entryPoints: [entry], bundle: true, format: 'esm', write: false, plugins: [obsidianStub], absWorkingDir: new URL('..', import.meta.url).pathname });
	return import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));
}

const { mapTitleRanges, shortFolder, displayHeading, fileKind } = await load('src/ui/results.ts');

test('file kind labels for non-note results', () => {
	assert.equal(fileKind('a/book.canvas'), 'canvas');
	assert.equal(fileKind('Coffee Beans Base.base'), 'base');
	assert.equal(fileKind('venn.excalidraw'), 'drawing');
	assert.equal(fileKind('x/Diagram.excalidraw.md'), 'drawing');
	assert.equal(fileKind('note.md'), '');
});

test('headings display their link text, or the host for an empty link text', () => {
	assert.equal(displayHeading('[](https://gist.github.com/andersevenrud/015e61af2fd264371032763d4ed965b6#vim)vim'), 'gist.github.com vim');
	assert.equal(displayHeading('See [the docs](https://example.com/x) and [[Note|alias]] and [[Plain]]'), 'See the docs and alias and Plain');
	assert.equal(displayHeading('Use `code` here'), 'Use code here');
	assert.equal(displayHeading('![](img.png) Figure'), 'img.png Figure');
	assert.equal(displayHeading('Plain heading'), 'Plain heading');
});

test('deep folders keep their first and last two segments', () => {
	assert.equal(shortFolder('a/b'), 'a/b');
	assert.equal(shortFolder('a/b/c/d'), 'a/b/c/d');
	assert.equal(shortFolder('💡 Resources/🌍 Wiki 📂 Files/🔗 Data Engineering/BigData/Conferences'), '💡 Resources/…/BigData/Conferences');
	assert.equal(shortFolder('a/b/c/d/e', 1), 'a/…/e');
	assert.equal(shortFolder(''), '');
});

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

// --- create guard: semantic candidates and the lexical-first flow ---
const { semanticGuard, runGuard } = await load('src/ui/modal.ts');

const vecBridge = (embed) => ({ available: () => true, hasVectors: () => true, embed });
const semEngine = { semantic: async () => [{ path: 'x.md', title: 'X', score: 0.812 }] };
const settings = (semantic) => ({ useSmartConnections: true, semantic, semanticMinSimilarity: 0.6 });

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

// --- why-ranked line and open-all eligibility ---
const { formatWhy } = await load('src/ui/results.ts');
const { openAllTargets, OPEN_ALL_MAX } = await load('src/ui/modal.ts');

const dbgHit = (debug, extra = {}) => ({ path: 'a.md', title: 'A', titleNorm: 'a', tier: 1, score: 183.4, badge: 'Title', titleRanges: [], isGhost: false, ghostRefs: 0, similarCount: 0, debug, ...extra });

test('why-ranked: tier, per-term hits, non-zero components, total', () => {
	const hit = dbgHit({
		perTerm: [['obsidian', 'Title', 'Exact', 1], ['trouble', 'Title', 'Prefix', 0.4666]],
		components: [['fields', 163.3], ['titleFactor', 1], ['proximity', 0], ['structural', 8.2], ['penalty', 1], ['frecency', 12.04], ['combo', 0]],
	});
	assert.equal(formatWhy(hit), 'tier 1 · obsidian→title exact 1.00 · trouble→title prefix 0.47 · fields +163 · structural +8.2 · frecency +12 · total 183');
});

test('why-ranked: multipliers below 1 show as ×', () => {
	const hit = dbgHit({ perTerm: [['ai', 'Body', 'Fuzzy', 0.5]], components: [['fields', 5], ['titleFactor', 0.85], ['penalty', 0.3]] }, { tier: 3, score: 1.5 });
	assert.equal(formatWhy(hit), 'tier 3 · ai→body fuzzy 0.50 · fields +5.0 · titleFactor ×0.85 · penalty ×0.30 · total 1.5');
});

test('why-ranked: empty without debug info', () => {
	assert.equal(formatWhy(dbgHit(null)), '');
	assert.equal(formatWhy(dbgHit(undefined)), '');
});

test('open all: up to 10 distinct notes, ghosts excluded', () => {
	const h = (path, isGhost = false) => ({ path, isGhost });
	assert.equal(OPEN_ALL_MAX, 10);
	const few = [h('a.md'), h('ghost', true), h('b.md'), h('a.md')];
	assert.deepEqual(openAllTargets(few), { eligible: true, count: 2, paths: ['a.md', 'b.md'] });
	const ten = Array.from({ length: 10 }, (_, i) => h(`${i}.md`)).concat([h('g', true)]);
	assert.equal(openAllTargets(ten).eligible, true);
	const eleven = Array.from({ length: 11 }, (_, i) => h(`${i}.md`));
	assert.deepEqual(openAllTargets(eleven), { eligible: false, count: 11, paths: [] });
	assert.deepEqual(openAllTargets([h('g', true)]), { eligible: false, count: 0, paths: [] });
});

// --- modal start state and open-all runner ---
const { startState, createOpenAll } = await load('src/ui/modal.ts');

test('start state: resume prefills and forces the preview; null or empty resume is a plain search', () => {
	assert.deepEqual(startState('ai', false, 'sel'), { query: 'ai', previewOpen: true });
	assert.deepEqual(startState(null, false, 'sel'), { query: 'sel', previewOpen: false });
	assert.deepEqual(startState('', false, 'sel'), { query: 'sel', previewOpen: false });
	assert.deepEqual(startState('', true, ''), { query: '', previewOpen: true });
});

function openAllDeps(openFile) {
	const log = [];
	return {
		log,
		deps: {
			resolve: (p) => (p === 'gone.md' ? null : { path: p }),
			noteOpen: (p) => log.push(['note', p]),
			openInTab: async (f) => {
				log.push(['open', f.path]);
				await openFile(f);
			},
			restore: () => log.push(['restore']),
			close: () => log.push(['close']),
			notice: (m) => log.push(['notice', m]),
		},
	};
}

test('open all: opens in order, restores focus, closes; a second press while opening is ignored', async () => {
	let release;
	const gate = new Promise((r) => (release = r));
	const { log, deps } = openAllDeps(() => gate);
	const run = createOpenAll(deps);
	const first = run(['a.md', 'gone.md', 'b.md']);
	const second = run(['c.md']);
	await second;
	assert.deepEqual(log, [['note', 'a.md'], ['open', 'a.md']]);
	release();
	await first;
	assert.deepEqual(log.slice(2), [['note', 'b.md'], ['open', 'b.md'], ['restore'], ['close']]);
	await run(['c.md']); // usable again afterwards
	assert.deepEqual(log.slice(-3), [['open', 'c.md'], ['restore'], ['close']]);
});

test('open all: a failing open shows a Notice, restores focus and still closes', async () => {
	const { log, deps } = openAllDeps(async (f) => {
		if (f.path === 'b.md') throw new Error('boom');
	});
	await createOpenAll(deps)(['a.md', 'b.md', 'c.md']);
	assert.deepEqual(log, [['note', 'a.md'], ['open', 'a.md'], ['note', 'b.md'], ['open', 'b.md'], ['notice', 'Fast Find 10K: open all failed (Error: boom)'], ['restore'], ['close']]);
});

const { isCloseSwipe } = await load('src/ui/modal.ts');

test('a quick, mostly horizontal swipe left closes; taps, scrolls, right and slow swipes do not', () => {
	assert.equal(isCloseSwipe(-120, 10, 200), true);
	assert.equal(isCloseSwipe(-80, 40, 599), true);
	assert.equal(isCloseSwipe(-5, 3, 100), false); // tap
	assert.equal(isCloseSwipe(-60, 0, 200), false); // too short
	assert.equal(isCloseSwipe(-100, 300, 200), false); // vertical scroll
	assert.equal(isCloseSwipe(120, 0, 200), false); // swipe right
	assert.equal(isCloseSwipe(-120, 0, 900), false); // slow drag
});

const { swallowKey } = await load('src/ui/modal.ts');

test('a modal key handler runs its action and swallows the event (Ctrl+R never reaches Obsidian reload)', () => {
	let ran = 0;
	const calls = [];
	const evt = { preventDefault: () => calls.push('preventDefault'), stopPropagation: () => calls.push('stopPropagation') };
	const handler = swallowKey(() => ran++);
	assert.equal(handler(evt), false);
	assert.equal(ran, 1);
	assert.deepEqual(calls.sort(), ['preventDefault', 'stopPropagation']);
});
