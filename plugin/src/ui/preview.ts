// Preview pane: lightweight text rendering of the selected note (headings coloured, links and
// tags tinted, query terms marked), scrolled to the match. Not Obsidian's markdown renderer.

import type { EngineClient } from '../engine-client';
import type { Hit } from '../protocol';

const MAX_LINES = 4000;
const MAX_MARKS = 200;
const MAX_STYLES = 2000;
export const MAX_LINE_CHARS = 2000;
export const MAX_LINE_STYLES = 50;
const LAZY_MARGIN = 200;
const CACHE_SIZE = 20;
const MIN_TERM = 2;

export type LineKind = 'h1' | 'h2' | 'h3' | 'code' | 'text';

export interface Line {
	kind: LineKind;
	/** Display text: heading markers stripped, everything else verbatim. */
	text: string;
}

export interface Segment {
	text: string;
	style: 'link' | 'tag' | null;
	mark: boolean;
}

const HEADING = /^(#{1,3})[ \t]+(.*)$/;
const FENCE = /^ {0,3}(`{3,}|~{3,})/;
const TAG_BODY = /[\p{L}\p{N}_\-/]+/uy;

/** Query input → highlight terms: whitespace split, filters (`-x`, `#x`, `path:x`) dropped,
 * quotes stripped, lower-cased, deduplicated. */
export function queryTerms(input: string): string[] {
	const out: string[] = [];
	for (const raw of input.split(/\s+/)) {
		if (!raw || raw.startsWith('-') || raw.startsWith('#') || raw.toLowerCase().startsWith('path:')) continue;
		const t = raw.replace(/["']/g, '').toLowerCase();
		if (t && !out.includes(t)) out.push(t);
	}
	return out;
}

/** Headings `#`–`###` (outside fenced code) get their kind and lose the markers; fence lines and
 * everything inside a fence are `code`. */
export function classifyLines(lines: string[]): Line[] {
	let fence: string | null = null;
	return lines.map((line) => {
		const f = FENCE.exec(line);
		if (fence !== null) {
			if (f && f[1][0] === fence[0] && f[1].length >= fence.length && line.trim() === f[1]) fence = null;
			return { kind: 'code', text: line };
		}
		if (f) {
			fence = f[1];
			return { kind: 'code', text: line };
		}
		const h = HEADING.exec(line);
		if (h) return { kind: `h${h[1].length}` as LineKind, text: h[2] };
		return { kind: 'text', text: line };
	});
}

/** Per-note caps on the work the preview renders. `marks`/`styles` are consumed across lines. */
export interface Budget {
	marks: number;
	styles: number;
}

/** Non-overlapping case-insensitive matches of the terms (≥ 2 chars), leftmost first, longest
 * term on ties; consumes `budget.marks`. Empty when lower-casing changes the line's length.
 * Each term's next occurrence is cached, so a term absent from the line is scanned once. */
function markRanges(text: string, terms: string[], budget: Budget): [number, number][] {
	const lower = text.toLowerCase();
	const usable = terms.filter((t) => t.length >= MIN_TERM);
	if (lower.length !== text.length || usable.length === 0) return [];
	const next = usable.map((t) => lower.indexOf(t));
	const out: [number, number][] = [];
	let pos = 0;
	while (budget.marks > 0) {
		let best = -1;
		let len = 0;
		for (let k = 0; k < usable.length; k++) {
			if (next[k] >= 0 && next[k] < pos) next[k] = lower.indexOf(usable[k], pos);
			const at = next[k];
			if (at >= 0 && (best < 0 || at < best || (at === best && usable[k].length > len))) {
				best = at;
				len = usable[k].length;
			}
		}
		if (best < 0) break;
		out.push([best, best + len]);
		budget.marks--;
		pos = best + len;
	}
	return out;
}

/** `[[link]]` / `![[embed]]` and `#tag` ranges, left to right, at most `cap`. One linear scan:
 * the next `[[` and `#` positions are cached, and an unclosed `[[` stops link search for the line. */
function styleRanges(text: string, cap: number): [number, number, 'link' | 'tag'][] {
	const out: [number, number, 'link' | 'tag'][] = [];
	let pos = 0;
	let link = text.indexOf('[[');
	let hash = text.indexOf('#');
	while (out.length < cap) {
		if (link >= 0 && link < pos) link = text.indexOf('[[', pos);
		if (hash >= 0 && hash < pos) hash = text.indexOf('#', pos);
		if (link < 0 && hash < 0) break;
		if (link >= 0 && (hash < 0 || link < hash)) {
			const close = text.indexOf(']]', link + 2);
			if (close < 0) {
				link = -1; // no closer left on this line
				continue;
			}
			if (close === link + 2) {
				pos = link + 2; // `[[]]` is not a link
				continue;
			}
			const s = link > pos && text[link - 1] === '!' ? link - 1 : link;
			out.push([s, close + 2, 'link']);
			pos = close + 2;
			continue;
		}
		pos = hash + 1;
		if (hash > 0 && !/[\s(]/.test(text[hash - 1])) continue;
		TAG_BODY.lastIndex = hash + 1;
		const m = TAG_BODY.exec(text);
		if (!m) continue;
		pos = hash + 1 + m[0].length;
		if (!/^\d+$/.test(m[0])) out.push([hash, pos, 'tag']);
	}
	return out;
}

/** Splits a line at every link/tag and mark boundary. `plain` (code) skips link/tag tinting.
 * Lines over MAX_LINE_CHARS are cut with a trailing `…`; at most MAX_LINE_STYLES styled ranges
 * per line, and both styles and marks draw from the per-note `budget`. Linear in the line. */
export function segmentLine(text: string, terms: string[], budget: Budget, plain: boolean): Segment[] {
	let cut = false;
	if (text.length > MAX_LINE_CHARS) {
		let n = MAX_LINE_CHARS;
		const c = text.charCodeAt(n - 1);
		if (c >= 0xd800 && c <= 0xdbff) n--; // do not split a surrogate pair
		text = text.slice(0, n);
		cut = true;
	}
	const styles = plain ? [] : styleRanges(text, Math.min(MAX_LINE_STYLES, budget.styles));
	budget.styles -= styles.length;
	const marks = markRanges(text, terms, budget);
	const points = [0, text.length];
	for (const [s, e] of styles) points.push(s, e);
	for (const [s, e] of marks) points.push(s, e);
	points.sort((a, b) => a - b);
	const out: Segment[] = [];
	let si = 0;
	let mi = 0;
	for (let i = 0; i + 1 < points.length; i++) {
		const s = points[i];
		const e = points[i + 1];
		if (s === e) continue;
		while (si < styles.length && styles[si][1] <= s) si++;
		while (mi < marks.length && marks[mi][1] <= s) mi++;
		const style = si < styles.length && styles[si][0] <= s ? styles[si][2] : null;
		const mark = mi < marks.length && marks[mi][0] <= s;
		const last = out[out.length - 1];
		if (last && !mark && !last.mark && last.style === style) last.text += text.slice(s, e);
		else out.push({ text: text.slice(s, e), style, mark });
	}
	if (cut) {
		const last = out[out.length - 1];
		if (last && !last.mark && last.style === null) last.text += '…';
		else out.push({ text: '…', style: null, mark: false });
	}
	return out;
}

export class Preview {
	private readonly el: HTMLElement;
	private readonly texts = new Map<string, string | null>();
	private token = 0;

	constructor(el: HTMLElement) {
		this.el = el;
	}

	/** Renders `hit`'s note; a completion for an older call (selection moved on) is dropped. */
	async show(engine: EngineClient, hit: Hit, queryTerms: string[]): Promise<void> {
		const token = ++this.token;
		let text = this.texts.get(hit.path);
		if (text === undefined) {
			try {
				text = await engine.docText(hit.path);
			} catch {
				text = null;
			}
			if (token !== this.token) return;
			this.remember(hit.path, text);
		} else {
			this.remember(hit.path, text); // refresh LRU order
		}
		this.render(text, hit, queryTerms);
	}

	clear(): void {
		this.token++;
		this.el.empty();
	}

	private remember(path: string, text: string | null): void {
		this.texts.delete(path);
		this.texts.set(path, text);
		if (this.texts.size > CACHE_SIZE) this.texts.delete(this.texts.keys().next().value as string);
	}

	private render(text: string | null, hit: Hit, terms: string[]): void {
		this.el.empty();
		if (text === null) {
			this.el.createDiv({ cls: 'ff10k-preview-empty', text: hit.isGhost ? 'not created yet' : 'no preview' });
			return;
		}
		const all = text.split('\n');
		const lines = classifyLines(all.slice(0, MAX_LINES));
		const budget: Budget = { marks: MAX_MARKS, styles: MAX_STYLES };
		const segs = lines.map((l) => segmentLine(l.text, terms, budget, l.kind === 'code'));
		const hitLine = hit.heading?.line ?? hit.line ?? null;
		const markLine = segs.findIndex((ss) => ss.some((x) => x.mark));
		const anchor = hitLine !== null && hitLine < lines.length ? hitLine : markLine;
		// Lines from the anchor's neighbourhood down skip off-screen layout; lines above it are
		// laid out for real so the scroll position is exact.
		const lazyFrom = Math.max(anchor, 0) + LAZY_MARGIN;
		const frag = document.createDocumentFragment();
		let target = null as HTMLElement | null;
		for (let i = 0; i < lines.length; i++) {
			const line = lines[i];
			const div = frag.createDiv({ cls: line.kind === 'text' ? 'ff10k-line' : `ff10k-line ff10k-${line.kind}` });
			if (i >= lazyFrom) div.addClass('ff10k-lazy');
			if (line.text === '') div.appendText('\u200b'); // keep empty lines one line tall
			for (const s of segs[i]) {
				const host = s.style ? div.createSpan({ cls: `ff10k-${s.style}` }) : div;
				if (s.mark) {
					const m = host.createEl('mark', { text: s.text });
					if (i === anchor && anchor === markLine) target ??= m;
				} else host.appendText(s.text);
			}
			if (i === anchor && anchor !== markLine) target = div;
		}
		if (all.length > MAX_LINES) frag.createDiv({ cls: 'ff10k-line ff10k-truncated', text: '… truncated' });
		this.el.appendChild(frag);
		this.el.scrollTop = 0;
		target?.scrollIntoView({ block: 'start' });
	}
}
