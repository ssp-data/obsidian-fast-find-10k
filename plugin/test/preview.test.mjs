// Pure helpers of the preview pane and the similar panel (no Obsidian runtime).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

const obsidianStub = {
	name: 'obsidian-stub',
	setup(build) {
		build.onResolve({ filter: /^obsidian$/ }, () => ({ path: 'obsidian', namespace: 'stub' }));
		build.onLoad({ filter: /.*/, namespace: 'stub' }, () => ({
			contents: 'export class Modal {} export class Notice {} export class MarkdownView {} export class TFile {} export class Scope {} export const Platform = {};',
			loader: 'js',
		}));
	},
};

async function load(entry) {
	const built = await esbuild.build({ entryPoints: [entry], bundle: true, format: 'esm', write: false, plugins: [obsidianStub], absWorkingDir: new URL('..', import.meta.url).pathname });
	return import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));
}

const { classifyLines, segmentLine, queryTerms, MAX_LINE_CHARS, MAX_LINE_STYLES } = await load('src/ui/preview.ts');
const { reasonLabel } = await load('src/ui/similar.ts');

const seg = (text, style = null, mark = false) => ({ text, style, mark });

test('heading lines get h1-h3 kinds with markers stripped; fenced code stays plain', () => {
	const lines = ['# Title', '## Sub', '### Deep', '#### Four', '#tag line', '```js', '# not a heading', '```', '~~~', '## also code', '~~~', 'text'];
	assert.deepEqual(classifyLines(lines), [
		{ kind: 'h1', text: 'Title' },
		{ kind: 'h2', text: 'Sub' },
		{ kind: 'h3', text: 'Deep' },
		{ kind: 'text', text: '#### Four' },
		{ kind: 'text', text: '#tag line' },
		{ kind: 'code', text: '```js' },
		{ kind: 'code', text: '# not a heading' },
		{ kind: 'code', text: '```' },
		{ kind: 'code', text: '~~~' },
		{ kind: 'code', text: '## also code' },
		{ kind: 'code', text: '~~~' },
		{ kind: 'text', text: 'text' },
	]);
});

test('links, tags and query terms split a line into segments', () => {
	const budget = { marks: 200, styles: 2000 };
	assert.deepEqual(segmentLine('See [[Data Engineering]] and #ai for AI data', ['data', 'ai'], budget, false), [
		seg('See '),
		seg('[[', 'link'),
		seg('Data', 'link', true),
		seg(' Engineering]]', 'link'),
		seg(' and '),
		seg('#', 'tag'),
		seg('ai', 'tag', true),
		seg(' for '),
		seg('AI', null, true),
		seg(' '),
		seg('data', null, true),
	]);
	assert.equal(budget.marks, 196);
});

test('matches are case-insensitive, non-overlapping and prefer the longest term', () => {
	assert.deepEqual(segmentLine('aaaa', ['aa'], { marks: 200, styles: 2000 }, false), [seg('aa', null, true), seg('aa', null, true)]);
	assert.deepEqual(segmentLine('Database', ['data', 'database', 'base'], { marks: 200, styles: 2000 }, false), [seg('Database', null, true)]);
});

test('terms under 2 chars are ignored and the match budget caps marks', () => {
	assert.deepEqual(segmentLine('a b', ['a'], { marks: 200, styles: 2000 }, false), [seg('a b')]);
	const budget = { marks: 1, styles: 2000 };
	assert.deepEqual(segmentLine('ai ai', ['ai'], budget, false), [seg('ai', null, true), seg(' ai')]);
	assert.equal(budget.marks, 0);
});

test('plain lines (code) get marks but no link or tag tint', () => {
	assert.deepEqual(segmentLine('x = [[a]] #bb', ['bb'], { marks: 200, styles: 2000 }, true), [seg('x = [[a]] #'), seg('bb', null, true)]);
});

test('a line whose lowercase changes length gets no marks rather than wrong ones', () => {
	assert.deepEqual(segmentLine('İstanbul data', ['data'], { marks: 200, styles: 2000 }, false), [seg('İstanbul data')]);
});

test('numeric-only hashes are not tags', () => {
	assert.deepEqual(segmentLine('issue #123 and #x1', [], { marks: 200, styles: 2000 }, false), [seg('issue #123 and '), seg('#x1', 'tag')]);
});

test('a huge line is cut at MAX_LINE_CHARS with a trailing ellipsis', () => {
	const out = segmentLine('a'.repeat(MAX_LINE_CHARS + 500), [], { marks: 200, styles: 2000 }, false);
	assert.deepEqual(out, [seg('a'.repeat(MAX_LINE_CHARS) + '…')]);
});

test('styled ranges are capped per line and by the per-note budget', () => {
	const line = Array.from({ length: 80 }, (_, i) => `#t${i}`).join(' ');
	const budget = { marks: 200, styles: 2000 };
	assert.equal(segmentLine(line, [], budget, false).filter((x) => x.style).length, MAX_LINE_STYLES);
	assert.equal(budget.styles, 2000 - MAX_LINE_STYLES);
	const small = { marks: 200, styles: 3 };
	assert.equal(segmentLine('#a #b #c #d #e', [], small, false).filter((x) => x.style).length, 3);
	assert.equal(small.styles, 0);
});

test('a 500 KB line of 75k tags segments fast and within the caps', () => {
	const tags = Array(75000).fill('#a').join(' ');
	const line = tags + ' ' + 'z'.repeat(500_000 - tags.length - 1);
	const t0 = performance.now();
	const out = segmentLine(line, ['a', 'zz'], { marks: 200, styles: 2000 }, false);
	const ms = performance.now() - t0;
	assert.ok(ms < 50, `took ${ms} ms`);
	assert.ok(out.filter((x) => x.style).length <= MAX_LINE_STYLES);
	assert.ok(out.reduce((n, x) => n + x.text.length, 0) <= MAX_LINE_CHARS + 1);
});

test('100k unclosed [[ segment fast with no links', () => {
	const line = '[['.repeat(100_000);
	const t0 = performance.now();
	const out = segmentLine(line, [], { marks: 200, styles: 2000 }, false);
	const ms = performance.now() - t0;
	assert.ok(ms < 20, `took ${ms} ms`);
	assert.equal(out.filter((x) => x.style).length, 0);
});

test('embeds, empty links and tags around links', () => {
	assert.deepEqual(segmentLine('![[img.png]] [[]] (#x) a#b [[n#h]]', [], { marks: 200, styles: 2000 }, false), [
		seg('![[img.png]]', 'link'),
		seg(' [[]] ('),
		seg('#x', 'tag'),
		seg(') a#b '),
		seg('[[n#h]]', 'link'),
	]);
});

test('query terms drop filters and quotes, lowercase and dedupe', () => {
	assert.deepEqual(queryTerms('Data  -draft #ai path:foo "exact phrase" ENG data'), ['data', 'exact', 'phrase', 'eng']);
	assert.deepEqual(queryTerms('   '), []);
	assert.deepEqual(queryTerms('- "" x'), ['x']);
});

test('similar reasons map to panel labels, including object variants', () => {
	assert.equal(reasonLabel('sameWordsReordered'), 'same words, different order');
	assert.equal(reasonLabel('copySuffix'), 'copy / punctuation variant');
	assert.equal(reasonLabel('sameTitleDifferentFolder'), 'same title, other folder');
	assert.equal(reasonLabel({ similarTitle: 0.65 }), 'similar title');
	assert.equal(reasonLabel({ semantic: 0.834 }), '0.83');
});
