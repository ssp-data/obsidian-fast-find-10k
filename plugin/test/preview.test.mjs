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

const { classifyLines, segmentLine, queryTerms } = await load('src/ui/preview.ts');
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
	const budget = { left: 200 };
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
	assert.equal(budget.left, 196);
});

test('matches are case-insensitive, non-overlapping and prefer the longest term', () => {
	assert.deepEqual(segmentLine('aaaa', ['aa'], { left: 200 }, false), [seg('aa', null, true), seg('aa', null, true)]);
	assert.deepEqual(segmentLine('Database', ['data', 'database', 'base'], { left: 200 }, false), [seg('Database', null, true)]);
});

test('terms under 2 chars are ignored and the match budget caps marks', () => {
	assert.deepEqual(segmentLine('a b', ['a'], { left: 200 }, false), [seg('a b')]);
	const budget = { left: 1 };
	assert.deepEqual(segmentLine('ai ai', ['ai'], budget, false), [seg('ai', null, true), seg(' ai')]);
	assert.equal(budget.left, 0);
});

test('plain lines (code) get marks but no link or tag tint', () => {
	assert.deepEqual(segmentLine('x = [[a]] #bb', ['bb'], { left: 200 }, true), [seg('x = [[a]] #'), seg('bb', null, true)]);
});

test('a line whose lowercase changes length gets no marks rather than wrong ones', () => {
	assert.deepEqual(segmentLine('İstanbul data', ['data'], { left: 200 }, false), [seg('İstanbul data')]);
});

test('numeric-only hashes are not tags', () => {
	assert.deepEqual(segmentLine('issue #123 and #x1', [], { left: 200 }, false), [seg('issue #123 and '), seg('#x1', 'tag')]);
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
