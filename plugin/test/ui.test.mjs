// Pure helpers of the modal: title-range mapping and keymap parsing (no Obsidian runtime).
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
