// Result rows of the search modal: tier group labels and one row per hit.

import type { Hit } from '../protocol';

export const TIER_LABELS: Record<number, string> = {
	0: 'recent',
	1: 'title · alias',
	2: 'headings',
	3: 'content',
	4: 'partial',
	5: 'semantic',
};

/** Maps `titleRanges` (UTF-16 offsets into `titleNorm`) onto the display title: each matched
 * substring is located case-insensitively after the previous match; ranges whose text does not
 * occur (diacritics, emoji, width differences) are dropped. Never throws. */
export function mapTitleRanges(display: string, norm: string, ranges: [number, number][]): [number, number][] {
	const lower = display.toLowerCase();
	if (lower.length !== display.length) return []; // case folding changed offsets; skip highlights
	const out: [number, number][] = [];
	let from = 0;
	for (const [s, e] of [...ranges].sort((a, b) => a[0] - b[0])) {
		if (!(s >= 0 && e > s && e <= norm.length)) continue;
		const sub = norm.slice(s, e).toLowerCase();
		const at = lower.indexOf(sub, from);
		if (at < 0) continue;
		out.push([at, at + sub.length]);
		from = at + sub.length;
	}
	return out;
}

/** Appends `text` to `el`, wrapping each in-bounds range in `span.ff10k-hl`. */
export function appendHighlighted(el: HTMLElement, text: string, ranges: [number, number][]): void {
	let pos = 0;
	for (const [s, e] of [...ranges].sort((a, b) => a[0] - b[0])) {
		if (s < pos || e <= s || e > text.length) continue;
		if (s > pos) el.appendText(text.slice(pos, s));
		el.createSpan({ cls: 'ff10k-hl', text: text.slice(s, e) });
		pos = e;
	}
	if (pos < text.length) el.appendText(text.slice(pos));
}

export function renderTierLabel(parent: HTMLElement, tier: number): HTMLElement {
	return parent.createDiv({ cls: 'ff10k-tier', text: TIER_LABELS[tier] ?? `tier ${tier}` });
}

function folderOf(path: string): string {
	const i = path.lastIndexOf('/');
	return i < 0 ? '' : path.slice(0, i);
}

/** Heading text for the row: link syntax reduced to its text (`[[a|b]]` → b, `[t](url)` → t, an
 * empty link text → the URL's host), inline code marks dropped; whitespace collapsed. */
export function displayHeading(text: string): string {
	return text
		.replace(/!?\[\[([^\]|]*)(?:\|([^\]]*))?\]\]/g, (_, target: string, alias?: string) => alias || target)
		.replace(/!?\[([^\]]*)\]\(([^)]*)\)/g, (_, label: string, url: string) => {
			if (label.trim()) return label;
			const m = /^[a-z]+:\/\/([^/?#]+)/i.exec(url.trim());
			return ` ${m ? m[1] : url.trim()} `; // spaced: the link may touch the next word
		})
		.replace(/`/g, '')
		.replace(/\s+/g, ' ')
		.trim();
}

/** A short label for files that are not markdown notes (`canvas`, `base`, `drawing`), '' for notes. */
export function fileKind(path: string): string {
	const p = path.toLowerCase();
	if (p.endsWith('.canvas')) return 'canvas';
	if (p.endsWith('.base')) return 'base';
	if (p.endsWith('.excalidraw') || p.endsWith('.excalidraw.md')) return 'drawing';
	return '';
}

/** The folder for the row: deep paths keep their first and last two segments (`a/…/y/z`), so
 * the specific end stays readable when the row is narrow; the full path is in the row tooltip. */
export function shortFolder(folder: string, keepEnd = 2): string {
	const parts = folder.split('/');
	if (parts.length <= keepEnd + 2) return folder;
	return [parts[0], '…', ...parts.slice(-keepEnd)].join('/');
}

/** Fills `el` (an empty row div) with the hit's badge, title, context and pills. */
export function renderRow(el: HTMLElement, hit: Hit, selected: boolean): void {
	el.addClass('ff10k-row');
	el.toggleClass('is-selected', selected);
	el.toggleClass('ff10k-ghost', hit.isGhost);
	el.toggleClass('is-expanded', hit.expanded);
	el.setAttr('title', hit.expanded ? `${hit.path}\nmatched through an abbreviation or language word` : hit.path);
	const main = el.createDiv({ cls: 'ff10k-row-main' });

	// semantic rows (tier 5) show their cosine as Smart Connections does
	const badge = hit.isGhost ? 'link' : hit.tier === 5 ? hit.score.toFixed(2) : hit.badge === 'Property' ? 'prop' : hit.badge.toLowerCase();
	main.createSpan({ cls: `ff10k-badge ff10k-badge-t${Math.min(hit.tier, 5)}`, text: badge });

	const title = main.createSpan({ cls: 'ff10k-title' });
	appendHighlighted(title, hit.title, mapTitleRanges(hit.title, hit.titleNorm, hit.titleRanges));
	const kind = hit.isGhost ? '' : fileKind(hit.path);
	if (kind) main.createSpan({ cls: 'ff10k-kind', text: kind });

	if (hit.isGhost) {
		main.createSpan({ cls: 'ff10k-context', text: `not created · linked from ${hit.ghostRefs} notes` });
	} else {
		if (hit.heading && (hit.badge === 'H1' || hit.badge === 'H2' || hit.badge === 'H3')) {
			main.createSpan({ cls: 'ff10k-heading', text: `${'#'.repeat(hit.heading.level)} ${displayHeading(hit.heading.text)}` });
		}
		if (hit.excerpt && (hit.badge === 'Body' || hit.badge === 'Property')) {
			const ex = main.createSpan({ cls: 'ff10k-excerpt' });
			appendHighlighted(ex, hit.excerpt.text, hit.excerpt.ranges);
		}
		const folder = folderOf(hit.path);
		if (folder) main.createSpan({ cls: 'ff10k-folder', text: shortFolder(folder) });
	}
	if (hit.expanded) main.createSpan({ cls: 'ff10k-expanded-badge', text: 'via word list' });
	if (hit.similarCount > 0) main.createSpan({ cls: 'ff10k-similar-badge', text: `${hit.similarCount} similar` });

	const why = formatWhy(hit);
	if (why) el.createDiv({ cls: 'ff10k-debug', text: why });
}

/** Score components that multiply (shown as `×0.30` when not 1); the others add. */
const MULTIPLIERS = new Set(['titleFactor', 'penalty']);

const num = (v: number) => (Math.abs(v) >= 10 ? String(Math.round(v)) : v.toFixed(1));

/** The "why ranked" line: `tier 1 · obsidian→title exact 1.00 · fields +163 · frecency +12 ·
 * total 183`. Zero additive components and multipliers of 1 are left out; '' without debug info. */
export function formatWhy(hit: Pick<Hit, 'tier' | 'score' | 'debug'>): string {
	if (!hit.debug) return '';
	const parts = [`tier ${hit.tier}`];
	for (const [t, f, k, q] of hit.debug.perTerm) parts.push(`${t}→${f.toLowerCase()} ${k.toLowerCase()} ${q.toFixed(2)}`);
	for (const [name, v] of hit.debug.components) {
		if (MULTIPLIERS.has(name)) {
			if (Math.abs(v - 1) > 1e-6) parts.push(`${name} ×${v.toFixed(2)}`);
		} else if (v !== 0) {
			parts.push(`${name} ${v > 0 ? '+' : '−'}${num(Math.abs(v))}`);
		}
	}
	parts.push(`total ${num(hit.score)}`);
	return parts.join(' · ');
}
