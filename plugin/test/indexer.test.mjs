// Indexer.isExcluded and the glob helper, with a stubbed `app` (no Obsidian runtime).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

/** Replaces the `obsidian` module (types only at runtime) with empty classes. */
const obsidianStub = {
	name: 'obsidian-stub',
	setup(build) {
		build.onResolve({ filter: /^obsidian$/ }, () => ({ path: 'obsidian', namespace: 'stub' }));
		build.onLoad({ filter: /.*/, namespace: 'stub' }, () => ({ contents: 'export class TFile {}', loader: 'js' }));
	},
};

const built = await esbuild.build({ entryPoints: ['src/indexer.ts'], bundle: true, format: 'esm', write: false, plugins: [obsidianStub], absWorkingDir: new URL('..', import.meta.url).pathname });
const { Indexer, globToRegExp } = await import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));

const app = { metadataCache: { isUserIgnored: (p) => p.startsWith('Private/') } };
const indexer = new Indexer(app, {}, {}, { ignoredGlobs: ['templates/**'] });

test('excluded paths', () => {
	for (const p of ['.obsidian/x.md', 'a/b.png', '.smart-env/y.md', 'a/.trash/z.md', 'Private/secret.md', 'templates/daily.md', 'templates/sub/t.md']) {
		assert.equal(indexer.isExcluded(p), true, p);
	}
});

test('kept paths', () => {
	for (const p of ['💡 Resources/x.md', 'Showcase.md', 'a.b/c.md', 'my templates/x.md']) {
		assert.equal(indexer.isExcluded(p), false, p);
	}
});

test('glob helper', () => {
	const m = (g, p) => globToRegExp(g).test(p);
	assert.equal(m('templates/**', 'templates/a.md'), true);
	assert.equal(m('templates/**', 'templates/x/y/a.md'), true);
	assert.equal(m('templates/**', 'other/templates/a.md'), false);
	assert.equal(m('*.excalidraw.md', 'Drawing.excalidraw.md'), true);
	assert.equal(m('*.excalidraw.md', 'deep/dir/Drawing.excalidraw.md'), true);
	assert.equal(m('*.excalidraw.md', 'Drawing.md'), false);
	assert.equal(m('Archive/*', 'Archive/old.md'), true);
	assert.equal(m('Archive/*', 'Archive/sub/old.md'), false);
	assert.equal(m('a/**/b.md', 'a/b.md'), true);
	assert.equal(m('a/**/b.md', 'a/x/y/b.md'), true);
	assert.equal(m('(x)+.md', '(x)+.md'), true);
});
