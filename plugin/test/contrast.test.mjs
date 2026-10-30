// Readable theme colours (src/ui/contrast.ts): parsing, WCAG contrast, and the blend toward the
// text colour that stops as soon as the target is met (no DOM; applyContrast needs Obsidian).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import esbuild from 'esbuild';

const built = await esbuild.build({ entryPoints: ['src/ui/contrast.ts'], bundle: true, format: 'esm', write: false, absWorkingDir: new URL('..', import.meta.url).pathname });
const { parseColor, contrast, readable, over, onAccent, toCss, TARGET } = await import('data:text/javascript;base64,' + Buffer.from(built.outputFiles[0].text).toString('base64'));

const hex = (s) => parseColor(s);

test('parses rgb, rgba, color(srgb) and hex', () => {
	assert.deepEqual(parseColor('rgb(255, 0, 10)'), { r: 255, g: 0, b: 10, a: 1 });
	assert.deepEqual(parseColor('rgba(1, 2, 3, 0.5)'), { r: 1, g: 2, b: 3, a: 0.5 });
	assert.deepEqual(parseColor('color(srgb 1 0 0.5)'), { r: 255, g: 0, b: 127.5, a: 1 });
	assert.deepEqual(parseColor('color(srgb 0 0 0 / 0.25)'), { r: 0, g: 0, b: 0, a: 0.25 });
	assert.deepEqual(parseColor('#fff'), { r: 255, g: 255, b: 255, a: 1 });
	assert.deepEqual(parseColor('#1F1F28'), { r: 31, g: 31, b: 40, a: 1 });
	assert.equal(parseColor('oklch(0.5 0.1 20)'), null);
	assert.equal(parseColor(''), null);
});

test('contrast matches the WCAG reference values', () => {
	assert.equal(contrast(hex('#000'), hex('#fff')).toFixed(2), '21.00');
	assert.equal(contrast(hex('#777'), hex('#fff')).toFixed(2), '4.48');
	assert.equal(contrast(hex('#fff'), hex('#000')), contrast(hex('#000'), hex('#fff')));
});

test('a colour that already reads is returned unchanged', () => {
	const text = hex('#DCD7BA');
	const bg = hex('#1F1F28');
	const orange = hex('#d65d0e'); // Kanagawa H2 on its dark background: 4.2:1
	const strong = hex('#ffa066');
	assert.deepEqual(readable(strong, text, [bg], TARGET), { ...strong, a: 1 });
	const fixed = readable(orange, text, [bg], TARGET);
	assert.ok(contrast(fixed, bg) >= TARGET);
	assert.ok(fixed.r > 200 && fixed.g < 200, 'still orange, not the text colour');
});

test('a pale light-theme accent is pushed toward the ink until it reads on both backgrounds', () => {
	const text = hex('#545464'); // Osaka Jade light ink
	const paper = hex('#f2ecbc');
	const panel = hex('#e7dba0');
	const accent = hex('#6f894e'); // lotusGreen: ~3:1 on paper
	const fixed = readable(accent, text, [paper, panel], TARGET);
	assert.ok(contrast(fixed, paper) >= TARGET, `paper ${contrast(fixed, paper)}`);
	assert.ok(contrast(fixed, panel) >= TARGET, `panel ${contrast(fixed, panel)}`);
	assert.ok(fixed.g > fixed.r, 'keeps a green tint');
});

test('translucent colours are measured composited, and the fallback is the text colour itself', () => {
	const text = hex('#23272e');
	const bg = hex('#f8f7f5');
	const faint = over(parseColor('rgba(35, 39, 46, 0.4)'), bg);
	const fixed = readable(parseColor('rgba(35, 39, 46, 0.4)'), text, [bg], TARGET);
	assert.ok(contrast(faint, bg) < TARGET);
	assert.ok(contrast(fixed, bg) >= TARGET);
	// a text colour that cannot reach the target: every step fails, the blend ends on the text colour
	const grey = hex('#888');
	assert.deepEqual(readable(hex('#999'), grey, [hex('#8a8a8a')], 7), { ...grey, a: 1 });
});

test('onAccent picks the side that reads better; toCss rounds', () => {
	assert.deepEqual(onAccent(hex('#76946A'), hex('#1F1F28'), hex('#DCD7BA')), hex('#1F1F28'));
	assert.deepEqual(onAccent(hex('#6f894e'), hex('#f2ecbc'), hex('#545464')), hex('#f2ecbc'));
	assert.equal(toCss({ r: 1.4, g: 2.6, b: 255, a: 1 }), 'rgb(1 3 255)');
});
