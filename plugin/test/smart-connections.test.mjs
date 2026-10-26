// Pure helpers of the Smart Connections bridge: vector packing, change fingerprint, poll backoff.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

async function load(entry) {
	const built = await esbuild.build({ entryPoints: [entry], bundle: true, format: 'esm', write: false, absWorkingDir: new URL('..', import.meta.url).pathname });
	return import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));
}

const { packVectors, fingerprint, backoffDelays } = await load('src/smart-connections-bridge.ts');

test('packVectors keeps items with a vec of the right length, in order, contiguous', () => {
	const res = packVectors(
		[
			{ path: 'a.md', vec: new Float32Array([1, 2, 3]) },
			{ path: 'none.md' },
			{ path: 'null.md', vec: null },
			{ path: 'b.md', vec: new Float32Array([4, 5, 6]) },
		],
		3,
	);
	assert.deepEqual(res.paths, ['a.md', 'b.md']);
	assert.deepEqual([...res.data], [1, 2, 3, 4, 5, 6]);
	assert.equal(res.skipped, 0);
});

test('packVectors skips and counts a vec of the wrong length', () => {
	const res = packVectors(
		[
			{ path: 'a.md', vec: new Float32Array([1, 2, 3]) },
			{ path: 'short.md', vec: new Float32Array([9, 9]) },
			{ path: 'long.md', vec: new Float32Array([9, 9, 9, 9]) },
		],
		3,
	);
	assert.deepEqual(res.paths, ['a.md']);
	assert.deepEqual([...res.data], [1, 2, 3]);
	assert.equal(res.skipped, 2);
});

test('packVectors errors when no item has a vec of the right length', () => {
	const wrong = packVectors([{ path: 'a.md', vec: new Float32Array([1, 2]) }], 3);
	assert.equal(typeof wrong.error, 'string');
	const none = packVectors([{ path: 'a.md' }], 3);
	assert.equal(typeof none.error, 'string');
	assert.equal(typeof packVectors([], 384).error, 'string');
	assert.equal(typeof packVectors([{ path: 'a.md', vec: new Float32Array([1]) }], 0).error, 'string');
});

test('fingerprint is stable for equal content and changes with count, path or hash', () => {
	const base = [
		{ path: 'a.md', hash: 'h1' },
		{ path: 'b.md', hash: 'h2' },
	];
	const fp = fingerprint(base);
	assert.equal(fingerprint(base.map((e) => ({ ...e }))), fp);
	assert.notEqual(fingerprint([...base, { path: 'c.md', hash: 'h3' }]), fp);
	assert.notEqual(fingerprint([base[0], { path: 'b.md', hash: 'h2x' }]), fp);
	assert.notEqual(fingerprint([base[0], { path: 'bb.md', hash: 'h2' }]), fp);
	// boundaries between path and hash matter
	assert.notEqual(fingerprint([{ path: 'ab', hash: 'c' }]), fingerprint([{ path: 'a', hash: 'bc' }]));
	assert.equal(typeof fp, 'string');
});

test('backoff doubles from 1 s and stops at 60 s total', () => {
	const d = backoffDelays();
	assert.deepEqual(d, [1000, 2000, 4000, 8000, 16000, 29000]);
	assert.equal(d.reduce((a, b) => a + b, 0), 60000);
	assert.deepEqual(backoffDelays(5000, 1000), [1000, 2000, 2000]);
});
