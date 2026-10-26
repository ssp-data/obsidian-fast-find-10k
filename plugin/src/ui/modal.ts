// The search modal: query input, tiered results, data-driven keys, open actions.

import { type App, Modal, type Modifier, Notice, Platform } from 'obsidian';
import { open, type OpenWhere } from '../actions';
import type FastFindPlugin from '../main';
import type { Hit, Mode, SearchResponse } from '../protocol';
import type { Keymap } from '../settings';
import { renderRow, renderTierLabel } from './results';

const LIMIT = 50;
const INITIAL_ROWS = 30;
const LATER = 'Fast Find 10K: coming in a later task';
const MODIFIERS: readonly Modifier[] = ['Mod', 'Ctrl', 'Meta', 'Shift', 'Alt'];

/** `Mod+Alt+Enter` → `{ modifiers: ['Mod','Alt'], key: 'Enter' }`; single letters lower-cased.
 * Returns null for an empty spec or an unknown modifier. */
export function parseKeySpec(spec: string): { modifiers: Modifier[]; key: string } | null {
	const parts = spec.split('+');
	const key = parts.pop() ?? '';
	if (!key) return null;
	if (!parts.every((p) => (MODIFIERS as string[]).includes(p))) return null;
	return { modifiers: parts as Modifier[], key: key.length === 1 ? key.toLowerCase() : key };
}

/** Key hint text for the footer: `Mod+Enter` → `⌘⏎` on macOS, `Ctrl+⏎` elsewhere. */
function keyLabel(spec: string): string {
	const sym: Record<string, string> = Platform.isMacOS
		? { Mod: '⌘', Ctrl: '⌃', Meta: '⌘', Alt: '⌥', Shift: '⇧', Enter: '⏎' }
		: { Mod: 'Ctrl+', Ctrl: 'Ctrl+', Meta: 'Win+', Alt: 'Alt+', Shift: 'Shift+', Enter: '⏎' };
	return spec
		.split('+')
		.map((p) => sym[p] ?? p)
		.join('');
}

interface State {
	query: string;
	mode: Mode;
	hits: Hit[];
	selected: number;
	totalDocs: number;
	elapsedUs: number;
	previewOpen: boolean;
	debug: boolean;
}

export class FastFindModal extends Modal {
	private readonly plugin: FastFindPlugin;
	private readonly state: State;
	private inputEl!: HTMLInputElement;
	private statusEl!: HTMLElement;
	private pillAll!: HTMLElement;
	private pillFiles!: HTMLElement;
	private resultsEl!: HTMLElement;
	private rowEls: HTMLElement[] = [];
	private lastTier = -1;
	private renderedQuery: string | null = null;
	private seq = 0;
	private closed = false;
	private deadNoticeShown = false;
	private unsubscribeStatus: (() => void) | null = null;

	constructor(app: App, plugin: FastFindPlugin, mode: Mode) {
		super(app);
		this.plugin = plugin;
		this.state = {
			query: '',
			mode,
			hits: [],
			selected: 0,
			totalDocs: 0,
			elapsedUs: 0,
			previewOpen: plugin.settings.previewOpen,
			debug: plugin.settings.debugByDefault,
		};
		this.registerKeys(plugin.settings.keymap);
	}

	onOpen(): void {
		this.modalEl.addClass('ff10k-modal');
		this.modalEl.toggleClass('ff10k-preview-hidden', !this.state.previewOpen);
		const { contentEl } = this;
		contentEl.empty();
		contentEl.addClass('ff10k-content');

		const bar = contentEl.createDiv({ cls: 'ff10k-bar' });
		this.inputEl = bar.createEl('input', { cls: 'ff10k-input', type: 'text', attr: { placeholder: 'Find a note…', spellcheck: 'false' } });
		this.pillAll = bar.createSpan({ cls: 'ff10k-pill', text: 'all' });
		this.pillFiles = bar.createSpan({ cls: 'ff10k-pill', text: `files only ${keyLabel(this.plugin.settings.keymap.filesOnly)}` });
		this.statusEl = bar.createSpan({ cls: 'ff10k-status' });
		this.pillAll.addEventListener('click', () => this.setMode('all'));
		this.pillFiles.addEventListener('click', () => this.setMode('filesOnly'));

		const body = contentEl.createDiv({ cls: 'ff10k-body' });
		this.resultsEl = body.createDiv({ cls: 'ff10k-results' });
		const right = body.createDiv({ cls: 'ff10k-right' });
		right.createDiv({ cls: 'ff10k-preview' });
		right.createDiv({ cls: 'ff10k-similar' });
		this.resultsEl.addEventListener('scroll', () => {
			const r = this.resultsEl;
			if (r.scrollTop + r.clientHeight >= r.scrollHeight - 80) this.renderUpTo(this.state.hits.length);
		});

		this.renderFooter(contentEl.createDiv({ cls: 'ff10k-footer' }));

		this.inputEl.value = this.app.workspace.activeEditor?.editor?.getSelection().trim() ?? '';
		this.inputEl.addEventListener('input', () => this.search());
		this.unsubscribeStatus =
			this.plugin.indexer?.onStatus((s) => {
				this.renderStatus();
				if (s.done) this.search(); // indexing finished while open: refresh
			}) ?? null;
		this.updatePills();
		this.renderStatus();
		this.search();
		this.inputEl.focus();
		this.inputEl.select();
	}

	onClose(): void {
		this.closed = true;
		this.unsubscribeStatus?.();
		this.unsubscribeStatus = null;
		this.contentEl.empty();
	}

	private registerKeys(km: Keymap): void {
		const later = () => new Notice(LATER, 1500);
		const actions: Record<keyof Keymap, () => void> = {
			open: () => void this.openSelected('same'),
			openTab: () => void this.openSelected('tab'),
			openSplit: () => void this.openSelected('split'),
			create: later,
			insertLink: later,
			semantic: later,
			focusSimilar: later,
			filesOnly: () => this.setMode(this.state.mode === 'all' ? 'filesOnly' : 'all'),
			moveDown: () => this.select(this.state.selected + 1),
			moveUp: () => this.select(this.state.selected - 1),
			togglePreview: () => this.togglePreview(),
			toggleDebug: () => this.toggleDebug(),
		};
		const bind = (spec: string, fn: () => void) => {
			const k = parseKeySpec(spec);
			if (!k) return console.warn('Fast Find 10K: invalid key binding', spec);
			this.scope.register(k.modifiers, k.key, () => {
				fn();
				return false;
			});
		};
		for (const name of Object.keys(actions) as (keyof Keymap)[]) bind(km[name], actions[name]);
		bind('ArrowDown', actions.moveDown);
		bind('ArrowUp', actions.moveUp);
	}

	private search(): void {
		const query = this.inputEl.value;
		this.state.query = query;
		const id = ++this.seq;
		const engine = this.plugin.engine;
		if (!engine) return this.engineDead('disposed');
		engine.search(query, this.state.mode, LIMIT, this.state.debug).then(
			(res) => {
				if (this.closed || id !== this.seq || query !== this.inputEl.value) return;
				const t0 = performance.now();
				this.render(res);
				const paintMs = performance.now() - t0;
				if (this.plugin.settings.logTimings) console.log('[ff10k] search', query, res.elapsedUs, 'us', paintMs);
			},
			(err: unknown) => {
				if (err !== 'stale') this.engineDead(err);
			},
		);
	}

	private engineDead(err: unknown): void {
		if (this.deadNoticeShown) return;
		this.deadNoticeShown = true;
		new Notice(`Fast Find 10K: engine unavailable (${String(err)})`);
	}

	private render(res: SearchResponse): void {
		// same query re-run (debug/indexing refresh): keep the selected note selected
		const keep = this.renderedQuery === this.state.query ? this.state.hits[this.state.selected]?.path : undefined;
		this.renderedQuery = this.state.query;
		this.state.hits = res.hits;
		this.state.totalDocs = res.totalDocs;
		this.state.elapsedUs = res.elapsedUs;
		this.state.selected = Math.max(0, keep === undefined ? 0 : res.hits.findIndex((h) => h.path === keep));
		this.resultsEl.empty();
		this.resultsEl.scrollTop = 0;
		this.rowEls = [];
		this.lastTier = -1;
		this.renderUpTo(Math.min(Math.max(INITIAL_ROWS, this.state.selected + 1), res.hits.length));
		this.rowEls[this.state.selected]?.scrollIntoView({ block: 'nearest' });
		if (res.hits.length === 0) this.resultsEl.createDiv({ cls: 'ff10k-empty', text: this.state.query ? 'No matches' : 'No recent notes' });
		this.renderStatus();
	}

	/** Appends rows (and tier labels) until `n` hits are rendered. */
	private renderUpTo(n: number): void {
		const { hits } = this.state;
		for (let i = this.rowEls.length; i < Math.min(n, hits.length); i++) {
			const hit = hits[i];
			if (hit.tier !== this.lastTier) {
				renderTierLabel(this.resultsEl, hit.tier);
				this.lastTier = hit.tier;
			}
			const row = this.resultsEl.createDiv();
			renderRow(row, hit, i === this.state.selected);
			row.addEventListener('click', () => {
				this.select(i);
				void this.openSelected('same');
			});
			this.rowEls.push(row);
		}
	}

	private renderStatus(): void {
		const s = this.plugin.indexer?.status;
		if (s && !s.done) {
			this.statusEl.setText(`indexing ${s.indexed.toLocaleString()} / ${s.total.toLocaleString()}`);
			return;
		}
		const ms = Math.round(this.state.elapsedUs / 1000);
		this.statusEl.setText(`${this.state.hits.length} / ${this.state.totalDocs.toLocaleString()} · ${ms === 0 ? '<1' : ms} ms`);
	}

	private renderFooter(el: HTMLElement): void {
		const km = this.plugin.settings.keymap;
		const hints: [string[], string][] = [
			[[km.open], 'open'],
			[[km.openTab], 'new tab'],
			[[km.openSplit], 'split'],
			[[km.create], 'create note'],
			[[km.insertLink], 'insert [[link]]'],
			[[km.filesOnly], 'files only'],
			[[km.moveDown, km.moveUp], 'move'],
			[[km.togglePreview], 'toggle preview'],
			[[km.toggleDebug], 'why ranked'],
			[['Esc'], 'close'],
		];
		for (const [keys, label] of hints) {
			const span = el.createSpan({ cls: 'ff10k-hint' });
			span.createEl('kbd', { text: keys.map(keyLabel).join(' / ') });
			span.appendText(` ${label}`);
		}
	}

	private select(i: number): void {
		const n = this.state.hits.length;
		if (n === 0) return;
		const next = Math.max(0, Math.min(n - 1, i));
		if (next >= this.rowEls.length) this.renderUpTo(n);
		this.rowEls[this.state.selected]?.removeClass('is-selected');
		this.state.selected = next;
		const row = this.rowEls[next];
		row.addClass('is-selected');
		row.scrollIntoView({ block: 'nearest' });
	}

	private async openSelected(where: OpenWhere): Promise<void> {
		const hit = this.state.hits[this.state.selected];
		if (!hit) return;
		if (hit.isGhost) {
			new Notice(LATER, 1500);
			return;
		}
		const file = await open(this.app, hit, where);
		if (file) this.close();
	}

	private setMode(mode: Mode): void {
		if (mode === this.state.mode) return;
		this.state.mode = mode;
		this.updatePills();
		this.search();
	}

	private updatePills(): void {
		this.pillAll.toggleClass('is-active', this.state.mode === 'all');
		this.pillFiles.toggleClass('is-active', this.state.mode === 'filesOnly');
	}

	private togglePreview(): void {
		this.state.previewOpen = !this.state.previewOpen;
		this.modalEl.toggleClass('ff10k-preview-hidden', !this.state.previewOpen);
	}

	private toggleDebug(): void {
		this.state.debug = !this.state.debug;
		this.search();
	}
}
