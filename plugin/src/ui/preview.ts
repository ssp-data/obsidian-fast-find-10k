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

const IMAGE_EXT = /\.(png|jpe?g|gif|webp|svg|avif|bmp)$/i;

/** The image target when the whole line is one image embed (`![[pic.png|300]]`, `![alt](pic.png)`,
 * `![alt](https://…/pic.png)`); null otherwise. The preview shows such lines as a picture. */
export function imageEmbed(line: string): string | null {
	const t = line.trim();
	let m = /^!\[\[([^\]|]+)(?:\|[^\]]*)?\]\]$/.exec(t);
	if (m) return IMAGE_EXT.test(m[1].trim()) ? m[1].trim() : null;
	m = /^!\[[^\]]*\]\(<?([^)>\s]+)>?(?:\s+"[^"]*")?\)$/.exec(t);
	if (m) {
		const target = decodeURIComponent(m[1]).replace(/[?#].*$/, '');
		return IMAGE_EXT.test(target) ? m[1] : null;
	}
	return null;
}

/** Resolves an image target of a note to a URL the webview can load (null: unknown file). */
export type ImageResolver = (target: string, fromPath: string) => string | null;

/** Renders a markdown snippet (a fenced diagram) into `el`; provided by the modal through Obsidian's renderer. */
export type MarkdownRenderFn = (markdown: string, el: HTMLElement) => Promise<void>;

/** Languages of fenced blocks the preview renders as diagrams instead of code. */
const RICH_LANGS = ['mermaid'];
/** Rendering waits this long after the last selection change, so moving through results stays instant. */
export const RICH_DELAY_MS = 250;

/** Fenced blocks of `lines` whose language the preview renders (`start`..`end` inclusive, fence lines included). */
export function richBlocks(lines: readonly string[]): { start: number; end: number; lang: string }[] {
	const out: { start: number; end: number; lang: string }[] = [];
	let open: { start: number; lang: string; marker: string } | null = null;
	for (let i = 0; i < lines.length; i++) {
		const t = lines[i].trim();
		const m = /^(`{3,}|~{3,})\s*([A-Za-z0-9_-]*)/.exec(t);
		if (!m) continue;
		if (!open) {
			if (RICH_LANGS.includes(m[2].toLowerCase())) open = { start: i, lang: m[2].toLowerCase(), marker: m[1] };
			else open = { start: i, lang: '', marker: m[1] };
		} else if (t.startsWith(open.marker[0]) && t.length >= open.marker.length && /^[`~]+$/.test(t)) {
			if (open.lang) out.push({ start: open.start, end: i, lang: open.lang });
			open = null;
		}
	}
	return out;
}

/** Lines shown in "matches only" mode: every line with a match plus `context` lines around it. */
export function visibleLines(matchLines: readonly number[], total: number, context = 1): boolean[] {
	const show: boolean[] = new Array(total).fill(false);
	for (const m of matchLines) {
		for (let i = Math.max(0, m - context); i <= Math.min(total - 1, m + context); i++) show[i] = true;
	}
	return show;
}

/** The drawing export an Excalidraw file may have next to it (`name.excalidraw.png` / `.svg`), to show above its text. */
export function excalidrawExports(path: string): string[] {
	const m = /^(.*\.excalidraw)(\.md)?$/i.exec(path);
	return m ? [`${m[1]}.png`, `${m[1]}.svg`] : [];
}

export class Preview {
	private readonly el: HTMLElement;
	private readonly texts = new Map<string, string | null>();
	private readonly resolveImage: ImageResolver | null;
	private readonly renderMarkdown: MarkdownRenderFn | null;
	private richTimer = 0;
	private token = 0;
	/** "Matches only": the lines around each match instead of the whole note (session state). */
	matchesOnly = false;
	private marks: HTMLElement[] = [];
	private current = -1;
	private headEl: HTMLElement | null = null;
	private last: { text: string | null; hit: Hit; terms: string[] } | null = null;

	constructor(el: HTMLElement, resolveImage: ImageResolver | null = null, renderMarkdown: MarkdownRenderFn | null = null) {
		this.el = el;
		this.resolveImage = resolveImage;
		this.renderMarkdown = renderMarkdown;
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
		window.clearTimeout(this.richTimer);
		this.el.empty();
		this.marks = [];
		this.current = -1;
		this.headEl = null;
		this.last = null;
	}

	/** Number of highlighted matches in the rendered note. */
	get matchCount(): number {
		return this.marks.length;
	}

	/** Scrolls to the next (`1`) or previous (`-1`) match, cycling; false when there is none. */
	nextMatch(dir: 1 | -1): boolean {
		const n = this.marks.length;
		if (n === 0) return false;
		this.marks[this.current]?.removeClass('is-current');
		this.current = this.current < 0 ? (dir > 0 ? 0 : n - 1) : (this.current + dir + n) % n;
		const m = this.marks[this.current];
		m.addClass('is-current');
		m.scrollIntoView({ block: 'center' });
		this.renderHead();
		return true;
	}

	/** Flips "matches only" and re-renders the current note. */
	toggleMatchesOnly(): void {
		this.matchesOnly = !this.matchesOnly;
		if (this.last) this.render(this.last.text, this.last.hit, this.last.terms);
	}

	private renderHead(): void {
		if (!this.headEl) return;
		const n = this.marks.length;
		const pos = this.current >= 0 ? `${this.current + 1} / ${n}` : `${n}`;
		this.headEl.setText(n === 0 ? 'no matches in this note' : `${pos} match${n === 1 ? '' : 'es'}${this.matchesOnly ? ' · matches only' : ''}`);
		this.headEl.toggleClass('is-active', this.matchesOnly);
	}

	private remember(path: string, text: string | null): void {
		this.texts.delete(path);
		this.texts.set(path, text);
		if (this.texts.size > CACHE_SIZE) this.texts.delete(this.texts.keys().next().value as string);
	}

	private render(text: string | null, hit: Hit, terms: string[]): void {
		this.el.empty();
		this.marks = [];
		this.current = -1;
		this.headEl = null;
		this.last = { text, hit, terms };
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
		const frag = createFragment();
		let target = null as HTMLElement | null;
		// match count, current match and the "matches only" mode; a click flips the mode
		const head = frag.createDiv({ cls: 'ff10k-preview-head', attr: { title: 'Click: matches only / whole note' } });
		head.addEventListener('click', () => this.toggleMatchesOnly());
		this.headEl = head;
		const matchLines: number[] = [];
		segs.forEach((ss, i) => {
			if (ss.some((x) => x.mark)) matchLines.push(i);
		});
		const show = this.matchesOnly && matchLines.length > 0 ? visibleLines(matchLines, lines.length) : null;
		let gap = false;
		// an Excalidraw drawing's export, when the plugin wrote one next to the file
		for (const exp of this.resolveImage ? excalidrawExports(hit.path) : []) {
			const src = this.resolveImage!(exp, hit.path);
			if (!src) continue;
			const img = frag.createDiv({ cls: 'ff10k-line' }).createEl('img', { cls: 'ff10k-preview-img', attr: { src, loading: 'lazy', alt: exp } });
			img.addEventListener('error', () => img.parentElement?.remove());
			break;
		}
		// fenced diagrams (mermaid): one block each, rendered after RICH_DELAY_MS; code text until then
		const rich = this.renderMarkdown ? richBlocks(all.slice(0, lines.length)) : [];
		const pending: { el: HTMLElement; markdown: string }[] = [];
		let skipTo = -1;
		for (let i = 0; i < lines.length; i++) {
			if (i <= skipTo) continue;
			if (show && !show[i]) {
				gap = true;
				continue;
			}
			if (gap) {
				frag.createDiv({ cls: 'ff10k-line ff10k-gap', text: '⋯' });
				gap = false;
			}
			const block = rich.find((b) => b.start === i);
			if (block) {
				const div = frag.createDiv({ cls: 'ff10k-line ff10k-rich' });
				div.setText(all.slice(block.start + 1, block.end).join('\n'));
				pending.push({ el: div, markdown: all.slice(block.start, block.end + 1).join('\n') });
				skipTo = block.end;
				continue;
			}
			const line = lines[i];
			const div = frag.createDiv({ cls: line.kind === 'text' ? 'ff10k-line' : `ff10k-line ff10k-${line.kind}` });
			if (i >= lazyFrom) div.addClass('ff10k-lazy');
			if (line.text === '') div.appendText('\u200b'); // keep empty lines one line tall
			const image = line.kind === 'text' && this.resolveImage ? imageEmbed(line.text) : null;
			const src = image ? this.resolveImage!(image, hit.path) : null;
			if (src) {
				// one lazy <img> per embed line; a file that fails to load falls back to the text
				const img = div.createEl('img', { cls: 'ff10k-preview-img', attr: { src, loading: 'lazy', alt: image ?? '' } });
				img.addEventListener('error', () => {
					img.remove();
					div.setText(line.text);
				});
				if (i === anchor && anchor !== markLine) target = div;
				continue;
			}
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
		this.marks = Array.from(this.el.querySelectorAll('mark'));
		this.current = target ? this.marks.indexOf(target) : -1;
		this.marks[this.current]?.addClass('is-current');
		this.renderHead();
		this.el.scrollTop = 0;
		target?.scrollIntoView({ block: 'start' });
		window.clearTimeout(this.richTimer);
		if (pending.length && this.renderMarkdown) {
			const token = this.token;
			const render = this.renderMarkdown;
			this.richTimer = window.setTimeout(() => {
				if (token !== this.token) return;
				for (const p of pending) {
					p.el.empty();
					void render(p.markdown, p.el).catch(() => p.el.setText(p.markdown));
				}
			}, RICH_DELAY_MS);
		}
	}
}
