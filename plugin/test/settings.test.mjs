// Settings: deep-merge of saved data, engine config mapping, key binding validation, help rows.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

async function load(entry) {
	const built = await esbuild.build({ entryPoints: [entry], bundle: true, format: 'esm', write: false, absWorkingDir: new URL('..', import.meta.url).pathname });
	return import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));
}

const { mergeSettings, settingsToEngineConfig, DEFAULT_SETTINGS, DEFAULT_KEYMAP } = await load('src/settings.ts');
const { parseKeySpec, validateBinding, keyLabel, conflicts } = await load('src/keys.ts');
const { helpGroups } = await load('src/ui/help.ts');

test('merge: nothing saved gives the defaults (fresh copies)', () => {
	const s = mergeSettings(null);
	assert.deepEqual(s, DEFAULT_SETTINGS);
	s.weights.title = 1;
	s.keymap.open = 'X';
	assert.equal(DEFAULT_SETTINGS.weights.title, 100);
	assert.equal(DEFAULT_SETTINGS.keymap.open, 'Enter');
});

test('merge: a missing weight or key keeps its default, saved ones win', () => {
	const s = mergeSettings({ weights: { body: 0, title: 120 }, keymap: { open: 'Ctrl+O' }, fuzzy: 'loose', lastQuery: 'abc' });
	assert.equal(s.weights.body, 0);
	assert.equal(s.weights.title, 120);
	assert.equal(s.weights.alias, 90);
	assert.equal(s.keymap.open, 'Ctrl+O');
	assert.equal(s.keymap.openTab, 'Mod+Enter');
	assert.equal(s.keymap.previewDown, 'Shift+J');
	assert.equal(s.fuzzy, 'loose');
	assert.equal(s.lastQuery, 'abc');
});

test('merge: unknown keys are dropped, wrongly typed values fall back', () => {
	const s = mergeSettings({ bogus: 1, weights: { nope: 5, h1: 'x' }, keymap: { fly: 'F' }, ignoredGlobs: 'notalist', minSimilarity: 0.8 });
	assert.equal('bogus' in s, false);
	assert.equal('nope' in s.weights, false);
	assert.equal(s.weights.h1, 60);
	assert.equal('fly' in s.keymap, false);
	assert.deepEqual(s.ignoredGlobs, []);
	assert.equal(s.minSimilarity, 0.8);
	assert.deepEqual(Object.keys(s).sort(), Object.keys(DEFAULT_SETTINGS).sort());
});

test('engine config: fuzzy, partial, folders, factor, threshold, weights', () => {
	const base = mergeSettings(null);
	const c = settingsToEngineConfig({ ...base, fuzzy: 'off', partial: 'always', downrankedFolders: ['Archive'] });
	assert.deepEqual(c.fuzzy, { enabled: false, loose: false });
	assert.equal(c.partial, 'always');
	assert.deepEqual(c.downrankedFolders, ['Archive']);
	assert.equal(c.downrankFactor, 0.3);
	assert.equal(c.fewThreshold, 5);
	assert.deepEqual(c.weights, DEFAULT_SETTINGS.weights);
	assert.deepEqual(settingsToEngineConfig({ ...base, fuzzy: 'normal' }).fuzzy, { enabled: true, loose: false });
	assert.deepEqual(settingsToEngineConfig({ ...base, fuzzy: 'loose' }).fuzzy, { enabled: true, loose: true });
	assert.equal(settingsToEngineConfig({ ...base, partial: 'never' }).partial, 'never');
});

test('optional words: default list, saved list wins, sent to the engine as a copy', () => {
	assert.deepEqual(DEFAULT_SETTINGS.optionalTerms, ['a', 'an', 'the', 'of', 'to', 'in', 'on', 'i']);
	const s = mergeSettings({ optionalTerms: ['the', 'I'] });
	assert.deepEqual(s.optionalTerms, ['the', 'I']);
	assert.deepEqual(mergeSettings({ optionalTerms: 'the' }).optionalTerms, DEFAULT_SETTINGS.optionalTerms);
	const c = settingsToEngineConfig(s);
	assert.deepEqual(c.optionalTerms, ['the', 'I']);
	c.optionalTerms.push('x');
	assert.deepEqual(s.optionalTerms, ['the', 'I']);
	mergeSettings(null).optionalTerms.push('y');
	assert.equal(DEFAULT_SETTINGS.optionalTerms.length, 8);
});

test('key specs split into modifiers and key', () => {
	assert.deepEqual(parseKeySpec('Enter'), { modifiers: [], key: 'Enter' });
	assert.deepEqual(parseKeySpec('Mod+Alt+Enter'), { modifiers: ['Mod', 'Alt'], key: 'Enter' });
	assert.deepEqual(parseKeySpec('Ctrl+/'), { modifiers: ['Ctrl'], key: '/' });
	assert.deepEqual(parseKeySpec('Ctrl+F'), { modifiers: ['Ctrl'], key: 'f' });
	assert.equal(parseKeySpec(''), null);
	assert.equal(parseKeySpec('Hyper+X'), null);
});

test('binding validation: syntax and clashes with other actions (Mod = Ctrl off macOS)', () => {
	const km = { ...DEFAULT_KEYMAP };
	assert.equal(validateBinding(km, 'openAll', 'Alt+Shift+O', false), null);
	assert.match(validateBinding(km, 'openAll', 'Hyper+O', false), /invalid/);
	assert.match(validateBinding(km, 'openAll', '', false), /invalid/);
	// Ctrl+Shift+Enter is createTab (Mod+Shift+Enter) on Linux/Windows, free on macOS
	assert.match(validateBinding(km, 'openAll', 'Ctrl+Shift+Enter', false), /createTab/);
	assert.equal(validateBinding(km, 'openAll', 'Ctrl+Shift+Enter', true), null);
	// modifier order and letter case do not matter; its own current binding is no clash
	assert.match(validateBinding(km, 'openAll', 'Alt+Mod+Enter', false), /openSplit/);
	assert.match(validateBinding(km, 'openAll', 'Ctrl+j', false), /moveDown/);
	assert.equal(validateBinding(km, 'moveDown', 'Ctrl+J', false), null);
});

test('insert link is gone: no default binding, a saved one is dropped', () => {
	assert.equal('insertLink' in DEFAULT_KEYMAP, false);
	assert.equal('insertLink' in mergeSettings({ keymap: { insertLink: 'Alt+Enter' } }).keymap, false);
});

test('why ranked defaults to Ctrl+R; a saved binding keeps its value', () => {
	assert.equal(DEFAULT_KEYMAP.toggleDebug, 'Ctrl+R');
	assert.equal(mergeSettings({ keymap: { toggleDebug: 'Ctrl+/' } }).keymap.toggleDebug, 'Ctrl+/');
});

test('the default keymap has no clashes on either platform', () => {
	assert.deepEqual(conflicts(DEFAULT_KEYMAP, false), []);
	assert.deepEqual(conflicts(DEFAULT_KEYMAP, true), []);
});

test('key labels: platform symbols, Shift+/ shows as ?', () => {
	assert.equal(keyLabel('Mod+Enter', false), 'Ctrl+⏎');
	assert.equal(keyLabel('Mod+Enter', true), '⌘⏎');
	assert.equal(keyLabel('Ctrl+Shift+/', false), 'Ctrl+?');
	assert.equal(keyLabel('Shift+J', false), 'Shift+J');
});

test('help overlay: every action once, in its group, with its current binding', () => {
	const km = { ...DEFAULT_KEYMAP, openTab: 'Ctrl+T' };
	const groups = helpGroups(km);
	assert.deepEqual(groups.map((g) => g.title), ['Navigate', 'Open', 'Create', 'Modes & panels', 'Help']);
	const actions = groups.flatMap((g) => g.rows.map((r) => r.action));
	for (const a of Object.keys(DEFAULT_KEYMAP)) assert.equal(actions.filter((x) => x === a).length, 1, a);
	const row = (a) => groups.flatMap((g) => g.rows).find((r) => r.action === a);
	assert.deepEqual(row('openTab').keys, ['Ctrl+T']);
	assert.deepEqual(row('moveDown').keys, ['Ctrl+J', 'ArrowDown']);
	assert.deepEqual(row('previewDown').keys, ['Shift+J']);
	assert.equal(groups.find((g) => g.title === 'Open').rows.some((r) => r.action === 'openAll'), true);
	assert.equal(groups.find((g) => g.title === 'Help').rows.some((r) => r.action === 'showHelp'), true);
	assert.deepEqual(row('close').keys, ['Escape']);
});
