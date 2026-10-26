// Preview pane: lightweight text rendering of the selected note (headings coloured, links and
// tags tinted, query terms marked), scrolled to the match. Not Obsidian's markdown renderer.

import type { EngineClient } from '../engine-client';
import type { Hit } from '../protocol';

const MAX_LINES = 4000;
const MAX_MARKS = 200;
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
const LINK = /!?\[\[[^\]\n]+\]\]/g;
const TAG = /(?<![^\s(])#[\p{L}\p{N}_\-/]+/gu;

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

/** Non-overlapping case-insensitive matches of the terms (≥ 2 chars), leftmost first, longest
 * term on ties; consumes `budget.left`. Empty when lower-casing changes the line's length. */
function markRanges(text: string, terms: string[], budget: { left: number }): [number, number][] {
	const lower = text.toLowerCase();
	const usable = terms.filter((t) => t.length >= MIN_TERM);
	if (lower.length !== text.length || usable.length === 0) return [];
	const out: [number, number][] = [];
	let pos = 0;
	while (budget.left > 0) {
		let best = -1;
		let len = 0;
		for (const t of usable) {
			const at = lower.indexOf(t, pos);
			if (at >= 0 && (best < 0 || at < best || (at === best && t.length > len))) {
				best = at;
				len = t.length;
			}
		}
		if (best < 0) break;
		out.push([best, best + len]);
		budget.left--;
		pos = best + len;
	}
	return out;
}

function styleRanges(text: string): [number, number, 'link' | 'tag'][] {
	const out: [number, number, 'link' | 'tag'][] = [];
	for (const m of text.matchAll(LINK)) out.push([m.index, m.index + m[0].length, 'link']);
	for (const m of text.matchAll(TAG)) {
		const s = m.index;
		const e = s + m[0].length;
		if (/^#\d+$/.test(m[0]) || out.some(([ls, le]) => s < le && e > ls)) continue;
		out.push([s, e, 'tag']);
	}
	return out.sort((a, b) => a[0] - b[0]);
}

/** Splits a line at every link/tag and mark boundary. `plain` (code) skips link/tag tinting. */
export function segmentLine(text: string, terms: string[], budget: { left: number }, plain: boolean): Segment[] {
	const styles = plain ? [] : styleRanges(text);
	const marks = markRanges(text, terms, budget);
	const cuts = new Set<number>([0, text.length]);
	for (const [s, e] of styles) cuts.add(s).add(e);
	for (const [s, e] of marks) cuts.add(s).add(e);
	const points = [...cuts].sort((a, b) => a - b);
	const out: Segment[] = [];
	for (let i = 0; i + 1 < points.length; i++) {
		const [s, e] = [points[i], points[i + 1]];
		const style = styles.find(([a, b]) => s >= a && e <= b)?.[2] ?? null;
		const mark = marks.some(([a, b]) => s >= a && e <= b);
		const last = out[out.length - 1];
		if (last && !mark && !last.mark && last.style === style) last.text += text.slice(s, e);
		else out.push({ text: text.slice(s, e), style, mark });
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
		const budget = { left: MAX_MARKS };
		const frag = document.createDocumentFragment();
		const lineEls: HTMLElement[] = [];
		let firstMark: HTMLElement | null = null;
		for (const line of lines) {
			const div = frag.createDiv({ cls: line.kind === 'text' ? 'ff10k-line' : `ff10k-line ff10k-${line.kind}` });
			if (line.text === '') div.appendText('​'); // keep empty lines one line tall
			for (const s of segmentLine(line.text, terms, budget, line.kind === 'code')) {
				const host = s.style ? div.createSpan({ cls: `ff10k-${s.style}` }) : div;
				if (s.mark) {
					const m = host.createEl('mark', { text: s.text });
					firstMark ??= m;
				} else host.appendText(s.text);
			}
			lineEls.push(div);
		}
		if (all.length > MAX_LINES) frag.createDiv({ cls: 'ff10k-line ff10k-truncated', text: '… truncated' });
		this.el.appendChild(frag);
		this.el.scrollTop = 0;

		const line = hit.heading?.line ?? hit.line ?? null;
		const target = (line !== null ? lineEls[line] : undefined) ?? firstMark;
		target?.scrollIntoView({ block: 'start' });
	}
}
