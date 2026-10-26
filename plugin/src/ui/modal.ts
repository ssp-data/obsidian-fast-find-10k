// The search modal: query input, tiered results, data-driven keys, open/create/link actions.

import { type App, Modal, type Modifier, Notice, Platform, Scope } from 'obsidian';
import { createNote, insertLink, open, type OpenWhere, sanitizeName } from '../actions';
import type FastFindPlugin from '../main';
import type { Hit, Mode, SearchResponse } from '../protocol';
import type { Keymap } from '../settings';
import { Preview, queryTerms } from './preview';
import { renderRow, renderTierLabel } from './results';
import { type PanelItem, SimilarPanel } from './similar';

const LIMIT = 50;
const INITIAL_ROWS = 30;
/** Existing notes the create guard shows at most, and how many files-only hits it asks for
 * (ghosts are tier 1 too and get filtered out). */
const GUARD_ROWS = 3;
const GUARD_SEARCH = 10;
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
	private preview!: Preview;
	private similar!: SimilarPanel;
	/** Pending paint of preview + similar panel (one per frame, superseded by newer selections). */
	private paintFrame = 0;
	/** Pushed while keyboard focus is in the similar panel. */
	private similarScope: Scope | null = null;
	/** Create guard ("Already have?") in progress for this note name. */
	private guard: { name: string } | null = null;
	/** Bumped per create request; a slower guard lookup that lost the race is dropped. */
	private createSeq = 0;

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
		this.preview = new Preview(right.createDiv({ cls: 'ff10k-preview' }));
		this.similar = new SimilarPanel(right.createDiv({ cls: 'ff10k-similar' }));
		this.similar.onOpen((path) => void this.openPath(path));
		this.resultsEl.addEventListener('scroll', () => {
			const r = this.resultsEl;
			if (r.scrollTop + r.clientHeight >= r.scrollHeight - 80) this.renderUpTo(this.state.hits.length);
		});

		this.renderFooter(contentEl.createDiv({ cls: 'ff10k-footer' }));

		this.inputEl.value = this.app.workspace.activeEditor?.editor?.getSelection().trim() ?? '';
		this.inputEl.addEventListener('input', () => {
			this.leaveSimilar(); // typing ends the create guard
			this.search();
		});
		this.inputEl.addEventListener('focus', () => this.leaveSimilar());
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
		cancelAnimationFrame(this.paintFrame);
		this.leaveSimilar();
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
			create: () => void this.startCreate('same'),
			createTab: () => void this.startCreate('tab'),
			insertLink: () => this.insertLinkSelected(),
			semantic: later,
			focusSimilar: () => this.enterSimilar(km),
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
		this.schedulePaint();
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
			[[km.createTab], 'create in tab'],
			[[km.insertLink], 'insert [[link]]'],
			[[km.filesOnly], 'files only'],
			[[km.moveDown, km.moveUp], 'move'],
			[[km.togglePreview], 'toggle preview'],
			[[km.focusSimilar], 'similar notes'],
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
		this.schedulePaint();
	}

	/** Coalesces preview + similar work to one run per frame; each ignores its own stale replies. */
	private schedulePaint(): void {
		this.leaveSimilar();
		cancelAnimationFrame(this.paintFrame);
		this.paintFrame = requestAnimationFrame(() => {
			this.paintFrame = 0;
			this.paint();
		});
	}

	private paint(): void {
		const engine = this.plugin.engine;
		const hit = this.state.hits[this.state.selected];
		if (this.closed || !this.state.previewOpen) return;
		if (!engine || !hit) {
			this.preview.clear();
			this.similar.clear();
			return;
		}
		// hasVectors: false until the Smart Connections bridge exists (Task 20)
		void this.preview.show(engine, hit, queryTerms(this.state.query));
		void this.similar.show(engine, hit, this.plugin.settings.minSimilarity, false);
	}

	/** Tab: keyboard focus into the similar panel; its own scope handles move/open/leave.
	 * In the create guard the same scope also confirms creation. */
	private enterSimilar(km: Keymap, guard = false): void {
		if (this.similarScope || (!guard && !this.state.previewOpen) || !this.similar.focus()) return;
		const scope = new Scope(this.scope);
		const bind = (spec: string, fn: () => void) => {
			const k = parseKeySpec(spec);
			if (k)
				scope.register(k.modifiers, k.key, () => {
					fn();
					return false;
				});
		};
		const leave = () => this.leaveSimilar();
		if (guard) {
			bind(km.create, () => void this.confirmCreate('same'));
			bind(km.createTab, () => void this.confirmCreate('tab'));
		} else {
			bind(km.focusSimilar, leave);
		}
		bind('Escape', leave);
		bind(km.moveDown, () => this.similar.move(1));
		bind(km.moveUp, () => this.similar.move(-1));
		bind('ArrowDown', () => this.similar.move(1));
		bind('ArrowUp', () => this.similar.move(-1));
		bind(km.open, () => this.similar.openSelected());
		this.similarScope = scope;
		this.app.keymap.pushScope(scope);
	}

	/** Leaves the similar panel (if focused) and gives keyboard focus back to the input.
	 * Also ends the create guard, restoring the panel. */
	private leaveSimilar(): void {
		if (!this.similarScope) return;
		this.app.keymap.popScope(this.similarScope);
		this.similarScope = null;
		this.similar.blur();
		this.inputEl.focus();
		if (this.guard) {
			this.guard = null;
			this.modalEl.toggleClass('ff10k-preview-hidden', !this.state.previewOpen);
			this.schedulePaint();
		}
	}

	/** Records the open for frecency (synchronously, so the `file-open` that follows is deduplicated). */
	private noteOpen(path: string): void {
		this.plugin.frecency?.noteModalOpen(path, this.inputEl.value).catch((e: unknown) => console.error('[ff10k] frecency', e));
	}

	private async openPath(path: string): Promise<void> {
		this.noteOpen(path);
		const file = await open(this.app, { path, line: null, heading: null }, 'same');
		if (file) this.close();
	}

	private async openSelected(where: OpenWhere): Promise<void> {
		const hit = this.state.hits[this.state.selected];
		if (!hit) return;
		if (hit.isGhost) return this.startCreate(where);
		this.noteOpen(hit.path);
		const file = await open(this.app, hit, where);
		if (file) this.close();
	}

	/** Shift+Enter: name = the selected ghost's target, else the query. With the guard on and
	 * existing notes close to the name, shows them first ("Already have?"); else creates now. */
	private async startCreate(where: OpenWhere): Promise<void> {
		if (this.guard) return this.confirmCreate(where);
		const hit = this.state.hits[this.state.selected];
		const query = this.inputEl.value;
		const name = sanitizeName(hit?.isGhost ? hit.title : query);
		if (!name) {
			new Notice('Fast Find 10K: type a name to create a note');
			return;
		}
		this.leaveSimilar();
		const seq = ++this.createSeq;
		const candidates = this.plugin.settings.createGuard ? await this.guardCandidates(name) : [];
		if (candidates === null || this.closed || seq !== this.createSeq || this.inputEl.value !== query) return;
		if (candidates.length === 0) return this.create(name, where);
		cancelAnimationFrame(this.paintFrame);
		this.guard = { name };
		this.modalEl.removeClass('ff10k-preview-hidden');
		this.similar.showGuard(candidates);
		this.enterSimilar(this.plugin.settings.keymap, true);
	}

	/** Existing notes whose title/alias matches `name` (tier 1, no ghosts). Null when a newer
	 * search superseded the lookup (the user kept typing): then nothing is created. */
	private async guardCandidates(name: string): Promise<PanelItem[] | null> {
		const engine = this.plugin.engine;
		if (!engine) return [];
		try {
			const res = await engine.search(name, 'filesOnly', GUARD_SEARCH, false);
			return res.hits
				.filter((h) => h.tier === 1 && !h.isGhost)
				.slice(0, GUARD_ROWS)
				.map((h) => ({ path: h.path, title: h.title, note: h.badge.toLowerCase() }));
		} catch (e) {
			return e === 'stale' ? null : [];
		}
	}

	private async confirmCreate(where: OpenWhere): Promise<void> {
		if (this.guard) await this.create(this.guard.name, where);
	}

	private async create(name: string, where: OpenWhere): Promise<void> {
		const file = await createNote(this.app, name, where);
		if (file) this.close();
	}

	/** Alt+Enter: link to the selected result at the editor cursor, then close. */
	private insertLinkSelected(): void {
		const hit = this.state.hits[this.state.selected];
		if (hit && insertLink(this.app, hit)) this.close();
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
		if (this.state.previewOpen) this.schedulePaint();
		else this.leaveSimilar();
	}

	private toggleDebug(): void {
		this.state.debug = !this.state.debug;
		this.search();
	}
}
