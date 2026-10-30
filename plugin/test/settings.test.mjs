// Settings: deep-merge of saved data, engine config mapping, key binding validation, help rows.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

async function load(entry) {
	const built = await esbuild.build({ entryPoints: [entry], bundle: true, format: 'esm', write: false, absWorkingDir: new URL('..', import.meta.url).pathname });
	return import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));
}

const { mergeSettings, settingsToEngineConfig, migrateKeymap, parseDictionaryLines, DEFAULT_SETTINGS, DEFAULT_KEYMAP } = await load('src/settings.ts');
const { parseKeySpec, validateBinding, keyLabel, conflicts, splitSpecs } = await load('src/keys.ts');
const { helpGroups, helpNotes } = await load('src/ui/help.ts');

test('merge: nothing saved gives the defaults (fresh copies)', () => {
	const s = mergeSettings(null);
	assert.deepEqual(s, DEFAULT_SETTINGS);
	s.weights.title = 1;
	s.keymap.open = 'X';
	assert.equal(DEFAULT_SETTINGS.weights.title, 100);
	assert.equal(DEFAULT_SETTINGS.weights.property, 25);
	assert.equal(mergeSettings({ weights: { body: 10 } }).weights.property, 25);
	assert.equal(DEFAULT_SETTINGS.keymap.open, 'Enter');
});

test('merge: enum values outside their allowed set fall back to the default', () => {
	const s = mergeSettings({ fuzzy: 'sloppy', partial: 'sometimes', semantic: 'maybe' });
	assert.equal(s.fuzzy, 'normal');
	assert.equal(s.partial, 'whenFew');
	assert.equal(s.semantic, 'always');
	const ok = mergeSettings({ fuzzy: 'off', partial: 'never', semantic: 'onDemand' });
	assert.deepEqual([ok.fuzzy, ok.partial, ok.semantic], ['off', 'never', 'onDemand']);
});

test('merge: weights are clamped to integers 0-150', () => {
	const s = mergeSettings({ weights: { title: 400, alias: -5, h1: 33.6, h2: NaN, body: Infinity } });
	assert.equal(s.weights.title, 150);
	assert.equal(s.weights.alias, 0);
	assert.equal(s.weights.h1, 34);
	assert.equal(s.weights.h2, 65); // not finite: default
	assert.equal(s.weights.body, 10);
});

test('merge: fewThreshold is clamped to an integer 1-50', () => {
	assert.equal(mergeSettings({ fewThreshold: 0 }).fewThreshold, 1);
	assert.equal(mergeSettings({ fewThreshold: -3 }).fewThreshold, 1);
	assert.equal(mergeSettings({ fewThreshold: 500 }).fewThreshold, 50);
	assert.equal(mergeSettings({ fewThreshold: 7.4 }).fewThreshold, 7);
	assert.equal(mergeSettings({ fewThreshold: NaN }).fewThreshold, 5);
});

test('merge: minSimilarity is clamped to 0.5-0.95', () => {
	assert.equal(mergeSettings({ minSimilarity: 0.1 }).minSimilarity, 0.5);
	assert.equal(mergeSettings({ minSimilarity: 2 }).minSimilarity, 0.95);
	assert.equal(mergeSettings({ minSimilarity: 0.8 }).minSimilarity, 0.8);
	assert.equal(mergeSettings({ minSimilarity: NaN }).minSimilarity, 0.7);
	assert.equal(settingsToEngineConfig(mergeSettings({ minSimilarity: 0.8 })).similarMinScore, 0.8);
	assert.equal(DEFAULT_SETTINGS.semanticMinSimilarity, 0.6);
	assert.equal(mergeSettings({ semanticMinSimilarity: 0.1 }).semanticMinSimilarity, 0.3);
	assert.equal(mergeSettings({ semanticMinSimilarity: 0.75 }).semanticMinSimilarity, 0.75);
	assert.equal(mergeSettings({ semanticMinSimilarity: 'x' }).semanticMinSimilarity, 0.6);
});

test('merge: downrankFactor is clamped to 0-1', () => {
	assert.equal(mergeSettings({ downrankFactor: -1 }).downrankFactor, 0);
	assert.equal(mergeSettings({ downrankFactor: 3 }).downrankFactor, 1);
	assert.equal(mergeSettings({ downrankFactor: 0.5 }).downrankFactor, 0.5);
	assert.equal(mergeSettings({ downrankFactor: NaN }).downrankFactor, 0.3);
});

test('merge: a hand-edited data.json always maps to an engine config the engine accepts', () => {
	const c = settingsToEngineConfig(mergeSettings({ fewThreshold: -2.5, weights: { title: 1e9 }, partial: 'x', downrankFactor: 9 }));
	assert.ok(Number.isInteger(c.fewThreshold) && c.fewThreshold >= 1);
	assert.ok(['whenFew', 'always', 'never'].includes(c.partial));
	assert.equal(c.weights.title, 150);
	assert.equal(c.downrankFactor, 1);
});

test('merge: a missing weight or key keeps its default, saved ones win', () => {
	const s = mergeSettings({ weights: { body: 0, title: 120 }, keymap: { open: 'Ctrl+O' }, fuzzy: 'loose', lastQuery: 'abc' });
	assert.equal(s.weights.body, 0);
	assert.equal(s.weights.title, 120);
	assert.equal(s.weights.alias, 90);
	assert.equal(s.keymap.open, 'Ctrl+O');
	assert.equal(s.keymap.openTab, 'Mod+Enter');
	assert.equal(s.keymap.previewDown, 'Alt+J');
	assert.equal(s.fuzzy, 'loose');
	assert.equal(s.lastQuery, 'abc');
});

test('merge: unknown keys are dropped, wrongly typed values fall back', () => {
	const s = mergeSettings({ bogus: 1, weights: { nope: 5, h1: 'x' }, keymap: { fly: 'F' }, ignoredGlobs: 'notalist', minSimilarity: 0.8 });
	assert.equal('bogus' in s, false);
	assert.equal('nope' in s.weights, false);
	assert.equal(s.weights.h1, 105);
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

test('abbreviations: empty by default, saved lines kept, sent to the engine as a copy', () => {
	assert.deepEqual(DEFAULT_SETTINGS.synonyms, []);
	const s = mergeSettings({ synonyms: ['oss: open source, open-source software', 'k8s: kubernetes'] });
	assert.deepEqual(s.synonyms, ['oss: open source, open-source software', 'k8s: kubernetes']);
	assert.deepEqual(mergeSettings({ synonyms: 'oss: open source' }).synonyms, []);
	assert.deepEqual(mergeSettings({}).synonyms, []);
	const c = settingsToEngineConfig(s);
	assert.deepEqual(c.synonyms, s.synonyms);
	c.synonyms.push('x');
	assert.equal(s.synonyms.length, 2);
	mergeSettings(null).synonyms.push('y');
	assert.equal(DEFAULT_SETTINGS.synonyms.length, 0);
});

test('dictionary note: default none, lines parsed from markdown, merged into the engine synonyms', () => {
	assert.equal(DEFAULT_SETTINGS.dictionaryFile, '');
	assert.equal(mergeSettings({ dictionaryFile: 'Dictionary.md' }).dictionaryFile, 'Dictionary.md');
	assert.equal(mergeSettings({ dictionaryFile: 3 }).dictionaryFile, '');
	const text = '---\ntags: [dict]\n---\n# My words\n\n- school: schule\n* data: daten, angaben\n1. oss: open source\nno colon here\n  meeting: sitzung  \n#comment: x\n';
	assert.deepEqual(parseDictionaryLines(text), ['school: schule', 'data: daten, angaben', 'oss: open source', 'meeting: sitzung']);
	assert.deepEqual(parseDictionaryLines(''), []);
	const s = mergeSettings({ synonyms: ['k8s: kubernetes'] });
	const c = settingsToEngineConfig(s, ['school: schule'], true);
	assert.deepEqual(c.synonyms, ['k8s: kubernetes', 'school: schule']);
	assert.deepEqual(s.synonyms, ['k8s: kubernetes']);
	// Note lines belong to language expansion: not sent while it is off.
	assert.deepEqual(settingsToEngineConfig(s, ['school: schule']).synonyms, ['k8s: kubernetes']);
	assert.deepEqual(settingsToEngineConfig(s).synonyms, ['k8s: kubernetes']);
});

test('languages: empty by default, saved list kept, only sent while language expansion is active', () => {
	assert.deepEqual(DEFAULT_SETTINGS.languages, []);
	const s = mergeSettings({ languages: ['de'] });
	assert.deepEqual(s.languages, ['de']);
	assert.deepEqual(mergeSettings({ languages: 'de' }).languages, []);
	// Inactive (the default session state): no languages, no dictionary-note lines.
	assert.deepEqual(settingsToEngineConfig(s, ['school: schule']).languages, []);
	assert.deepEqual(settingsToEngineConfig(s, ['school: schule']).synonyms, []);
	// Active: the saved list, or the built-in German list when none is saved, plus the note's lines.
	const c = settingsToEngineConfig(s, ['school: schule'], true);
	assert.deepEqual(c.languages, ['de']);
	assert.deepEqual(c.synonyms, ['school: schule']);
	assert.deepEqual(settingsToEngineConfig(mergeSettings(null), [], true).languages, ['de']);
	c.languages.push('xx');
	assert.deepEqual(s.languages, ['de']);
	mergeSettings(null).languages.push('y');
	assert.equal(DEFAULT_SETTINGS.languages.length, 0);
	assert.equal(DEFAULT_KEYMAP.toggleLanguages, 'Ctrl+L');
	assert.equal(DEFAULT_SETTINGS.previewOpenMobile, false);
	assert.equal(DEFAULT_SETTINGS.keyHints, 'desktop');
	assert.equal(DEFAULT_SETTINGS.ribbonIcon, true);
	assert.equal(mergeSettings({ ribbonIcon: false }).ribbonIcon, false);
	assert.equal(mergeSettings({ keyHints: 'sometimes' }).keyHints, 'desktop');
	assert.equal(mergeSettings({ keyHints: 'never' }).keyHints, 'never');
	assert.equal(DEFAULT_KEYMAP.panelRight, 'Alt+L');
	assert.equal(DEFAULT_KEYMAP.panelLeft, 'Alt+H');
	assert.equal(DEFAULT_KEYMAP.clearQuery, 'Ctrl+C');
	assert.equal(DEFAULT_KEYMAP.nextMatch, 'Alt+N');
	assert.equal(DEFAULT_KEYMAP.prevMatch, 'Alt+Shift+N | Alt+P');
	assert.equal(DEFAULT_KEYMAP.toggleMatchesOnly, 'Ctrl+M');
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

test('bindings with alternatives: split, labelled, validated and bound without clashes', () => {
	assert.deepEqual(splitSpecs('Alt+Shift+N | Alt+P'), ['Alt+Shift+N', 'Alt+P']);
	assert.deepEqual(splitSpecs(' Alt+P '), ['Alt+P']);
	assert.equal(keyLabel('Alt+Shift+N | Alt+P', false), 'Alt+Shift+N / Alt+P');
	const km = { ...DEFAULT_KEYMAP };
	assert.equal(validateBinding(km, 'prevMatch', 'Alt+Shift+N | Alt+P', false), null);
	assert.match(validateBinding(km, 'prevMatch', 'Alt+P | Alt+N', false), /already used by nextMatch/);
	assert.match(validateBinding(km, 'prevMatch', 'Alt+P | Alt+P', false), /listed twice/);
	assert.match(validateBinding(km, 'prevMatch', 'Alt+P | Bogus+X', false), /invalid key/);
	assert.deepEqual(conflicts(km, false), []);
	assert.deepEqual(conflicts({ ...km, openAll: 'Alt+P' }, false), [['openAll', 'prevMatch']]);
});

test('keymap migration: old defaults move to the new ones (Shift+J/K/L/H → Alt, Ctrl+/ → Ctrl+R, prevMatch gains Alt+P)', () => {
	const old = { ...DEFAULT_KEYMAP, previewDown: 'Shift+J', previewUp: 'Shift+K', panelRight: 'Shift+L', panelLeft: 'Shift+H', prevMatch: 'Alt+Shift+N' };
	assert.equal(migrateKeymap(old, false), true);
	assert.deepEqual([old.previewDown, old.previewUp, old.panelRight, old.panelLeft, old.prevMatch], ['Alt+J', 'Alt+K', 'Alt+L', 'Alt+H', 'Alt+Shift+N | Alt+P']);
	assert.equal(migrateKeymap(old, false), false, 'idempotent');
	// a user binding on the new key keeps the old one in place
	const taken = { ...DEFAULT_KEYMAP, previewDown: 'Shift+J', openAll: 'Alt+J' };
	migrateKeymap(taken, false);
	assert.equal(taken.previewDown, 'Shift+J');
	// a user's own choice is never touched
	const custom = { ...DEFAULT_KEYMAP, previewDown: 'Ctrl+Shift+J' };
	assert.equal(migrateKeymap(custom, false), false);
});

test('keymap migration: the old why-ranked default Ctrl+/ becomes Ctrl+R once, unless Ctrl+R is taken', () => {
	const old = mergeSettings({ keymap: { toggleDebug: 'Ctrl+/' } }).keymap;
	assert.equal(migrateKeymap(old, false), true);
	assert.equal(old.toggleDebug, 'Ctrl+R');
	assert.equal(migrateKeymap(old, false), false); // already migrated
	const taken = mergeSettings({ keymap: { toggleDebug: 'Ctrl+/', openAll: 'Ctrl+R' } }).keymap;
	assert.equal(migrateKeymap(taken, false), false);
	assert.equal(taken.toggleDebug, 'Ctrl+/');
	const custom = mergeSettings({ keymap: { toggleDebug: 'Alt+D' } }).keymap;
	assert.equal(migrateKeymap(custom, false), false);
	assert.equal(custom.toggleDebug, 'Alt+D');
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
	assert.deepEqual(row('previewDown').keys, ['Alt+J']);
	assert.deepEqual(row('prevMatch').keys, ['Alt+Shift+N', 'Alt+P']);
	assert.equal(groups.find((g) => g.title === 'Open').rows.some((r) => r.action === 'openAll'), true);
	assert.equal(groups.find((g) => g.title === 'Help').rows.some((r) => r.action === 'showHelp'), true);
	assert.deepEqual(row('close').keys, ['Escape']);
});

test('help overlay: a Shift+letter binding notes that the capital letter cannot be typed', () => {
	assert.deepEqual(helpNotes(DEFAULT_KEYMAP, false), [], 'the defaults take no capital letter');
	const notes = helpNotes({ ...DEFAULT_KEYMAP, previewDown: 'Shift+J', previewUp: 'Shift+K' }, false);
	assert.equal(notes.length, 1);
	assert.match(notes[0], /Shift\+J, Shift\+K/);
	assert.match(notes[0], /capital J, K/);
	assert.match(notes[0], /Alt\+J/);
});
