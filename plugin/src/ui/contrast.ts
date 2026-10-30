// Readable theme colours. The modal's palette comes from the theme (--text-accent, --text-title-h*,
// --text-faint, …), and many themes set those too pale for small text: Osaka Jade light's accent
// reads 2:1 on its paper, Kanagawa's faint text 3.3:1. CSS cannot compare colours, so at open each
// role is blended toward the theme's own text colour just far enough to reach its target contrast
// against the list and panel backgrounds, and written as custom properties on the modal. Hue stays
// where the theme already reads; only pale colours move. Runs once per open, a few µs.

export interface Rgb {
	r: number;
	g: number;
	b: number;
	a: number;
}

/** `rgb()`, `rgba()`, `color(srgb …)` (what Chrome returns for color-mix) and `#hex`; null otherwise. */
export function parseColor(s: string): Rgb | null {
	const t = s.trim();
	let m = /^rgba?\(([^)]+)\)$/.exec(t);
	if (m) {
		const p = m[1].split(/[\s,/]+/).filter(Boolean).map(Number);
		if (p.length < 3 || p.some(Number.isNaN)) return null;
		return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 };
	}
	m = /^color\(srgb\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*([\d.]+%?))?\)$/.exec(t);
	if (m) {
		const a = m[4] === undefined ? 1 : m[4].endsWith('%') ? parseFloat(m[4]) / 100 : parseFloat(m[4]);
		return { r: +m[1] * 255, g: +m[2] * 255, b: +m[3] * 255, a };
	}
	m = /^#([0-9a-f]{3,8})$/i.exec(t);
	if (m) {
		let h = m[1];
		if (h.length === 3 || h.length === 4) h = [...h].map((c) => c + c).join('');
		if (h.length !== 6 && h.length !== 8) return null;
		const n = parseInt(h, 16);
		const a = h.length === 8 ? (n & 0xff) / 255 : 1;
		const rgb = h.length === 8 ? n >>> 8 : n;
		return { r: (rgb >> 16) & 0xff, g: (rgb >> 8) & 0xff, b: rgb & 0xff, a };
	}
	return null;
}

/** `top` composited over an opaque `bottom`. */
export function over(top: Rgb, bottom: Rgb): Rgb {
	const a = top.a;
	return { r: top.r * a + bottom.r * (1 - a), g: top.g * a + bottom.g * (1 - a), b: top.b * a + bottom.b * (1 - a), a: 1 };
}

function luminance(c: Rgb): number {
	const f = (v: number) => {
		v /= 255;
		return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
	};
	return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b);
}

/** WCAG contrast ratio, 1–21. */
export function contrast(a: Rgb, b: Rgb): number {
	const la = luminance(a);
	const lb = luminance(b);
	return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

export function mix(from: Rgb, to: Rgb, t: number): Rgb {
	return { r: from.r + (to.r - from.r) * t, g: from.g + (to.g - from.g) * t, b: from.b + (to.b - from.b) * t, a: 1 };
}

/** `color` blended toward `text` in 5 % steps until it reads at `target` on every background
 * (or all the way to `text` when even that is not enough). Alpha is composited over the first
 * background first, so translucent theme colours (`color-mix(… transparent)`) are measured as seen. */
export function readable(color: Rgb, text: Rgb, backgrounds: Rgb[], target: number): Rgb {
	const base = color.a < 1 ? over(color, backgrounds[0]) : color;
	const ok = (c: Rgb) => backgrounds.every((bg) => contrast(c, bg) >= target);
	for (let t = 0; t <= 1; t += 0.05) {
		const c = mix(base, text, t);
		if (ok(c)) return c;
	}
	return mix(base, text, 1);
}

export function toCss(c: Rgb): string {
	return `rgb(${Math.round(c.r)} ${Math.round(c.g)} ${Math.round(c.b)})`;
}

/** Target contrast for small coloured text (badges, folders, tier labels, highlights). */
export const TARGET = 4.5;
/** Target for UI marks that carry no text: the selection bar and tint, the active pill's ring. */
export const TARGET_UI = 3;

/** Roles: the theme variable each comes from (Obsidian's defaults as fallback) and its target. */
export const ROLES: Record<string, { from: string; target: number }> = {
	accent: { from: 'var(--text-accent)', target: TARGET },
	h1: { from: 'var(--text-title-h1, var(--color-red))', target: TARGET },
	h2: { from: 'var(--text-title-h2, var(--color-orange))', target: TARGET },
	h3: { from: 'var(--text-title-h3, var(--color-yellow))', target: TARGET },
	faint: { from: 'var(--text-faint)', target: TARGET },
	muted: { from: 'var(--text-muted)', target: TARGET },
	orange: { from: 'var(--color-orange)', target: TARGET },
	selection: { from: 'var(--interactive-accent)', target: TARGET_UI },
};

/** Share of the selection colour in the selected row's background tint. */
export const SELECTED_TINT = 0.16;

/** Writes `--ff10k-<role>` for every role, `--ff10k-selected-bg` (the selection tint) and
 * `--ff10k-on-accent` (the text colour that reads on a block of the accent) as inline custom
 * properties on `modalEl`. Colours are resolved through a probe element, so `var()`,
 * `color-mix()` and theme aliases all come back as plain rgb. Text roles are tuned against the
 * primary and secondary backgrounds and the selected row's tint. Leaves the stylesheet's
 * fallbacks in place for a colour that cannot be resolved. */
export function applyContrast(modalEl: HTMLElement): void {
	const probe = modalEl.createSpan();
	probe.style.display = 'none';
	const resolve = (expr: string): Rgb | null => {
		probe.style.color = '';
		probe.style.color = expr;
		return parseColor(getComputedStyle(probe).color);
	};
	try {
		const text = resolve('var(--text-normal)');
		const primary = resolve('var(--background-primary)');
		const secondary = resolve('var(--background-secondary)');
		if (!text || !primary) return;
		const backgrounds = [primary, secondary ?? primary];
		const rawSelection = resolve(ROLES.selection.from);
		if (rawSelection) {
			const selection = readable(rawSelection, text, backgrounds, ROLES.selection.target);
			modalEl.style.setProperty('--ff10k-selection', toCss(selection));
			const tint = mix(primary, selection, SELECTED_TINT);
			modalEl.style.setProperty('--ff10k-selected-bg', toCss(tint));
			backgrounds.push(tint);
		}
		let accent: Rgb | null = null;
		for (const [role, { from, target }] of Object.entries(ROLES)) {
			if (role === 'selection') continue;
			const c = resolve(from);
			if (!c) continue;
			const fixed = readable(c, text, backgrounds, target);
			if (role === 'accent') accent = fixed;
			modalEl.style.setProperty(`--ff10k-${role}`, toCss(fixed));
		}
		if (accent) modalEl.style.setProperty('--ff10k-on-accent', toCss(onAccent(accent, primary, text)));
	} finally {
		probe.detach();
	}
}

/** The text colour for a block filled with `accent`: the background or the text colour, whichever reads better. */
export function onAccent(accent: Rgb, background: Rgb, text: Rgb): Rgb {
	return contrast(accent, background) >= contrast(accent, text) ? background : text;
}
