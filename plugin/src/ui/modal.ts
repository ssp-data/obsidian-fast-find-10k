// The search modal: query input, tiered results, data-driven keys, open/create actions,
// help overlay.

import { type App, Component, MarkdownRenderer, Modal, Notice, Platform, Scope, TFile, type WorkspaceLeaf } from 'obsidian';
import { createGhostNote, createNote, ghostNotePath, open, type OpenWhere, sanitizeName } from '../actions';
import type FastFindPlugin from '../main';
import { debugLog } from '../debug';
import type { Hit, Mode, SearchResponse, SemanticHit } from '../protocol';
import type { EngineClient } from '../engine-client';
import { keyLabel as keyLabelFor, parseKeySpec, splitSpecs } from '../keys';
import type { FastFindSettings, Keymap } from '../settings';
import type { SmartConnectionsBridge } from '../smart-connections-bridge';
import { Preview, queryTerms } from './preview';
import { renderHelp } from './help';
import { renderRow, renderTierLabel } from './results';
import { type PanelItem, SimilarPanel } from './similar';

const LIMIT = 50;
const INITIAL_ROWS = 30;
/** Existing notes the create guard shows at most, and how many files-only hits it asks for
 * (ghosts are tier 1 too and get filtered out). */
const GUARD_ROWS = 3;
const GUARD_SEARCH = 10;
/** Semantic neighbours asked for: the tier-5 group, and the create guard's share. */
const SEMANTIC_ROWS = 10;
const GUARD_SEMANTIC = 3;
/** Open all shown (Alt+Shift+Enter) works up to this many notes. */
export const OPEN_ALL_MAX = 10;

const keyLabel = (spec: string) => keyLabelFor(spec, Platform.isMacOS);

/** Scope handler for a modal binding: runs `fn` and swallows the event, so a key Obsidian also
 * binds (Ctrl+R reloads the app) never reaches its global hotkey while the modal is open. */
export function swallowKey(fn: () => void): (evt: Pick<KeyboardEvent, 'preventDefault' | 'stopPropagation'>) => false {
	return (evt) => {
		evt.preventDefault();
		evt.stopPropagation();
		fn();
		return false;
	};
}

/** The notes "open all" would open: distinct non-ghost paths of every shown hit, in order;
 * not eligible (and no paths) with none or more than `OPEN_ALL_MAX`. */
export function openAllTargets(hits: Pick<Hit, 'path' | 'isGhost'>[]): { eligible: boolean; count: number; paths: string[] } {
	const paths = [...new Set(hits.filter((h) => !h.isGhost).map((h) => h.path))];
	const eligible = paths.length > 0 && paths.length <= OPEN_ALL_MAX;
	return { eligible, count: paths.length, paths: eligible ? paths : [] };
}

/** A touch that closes the search: a quick, mostly horizontal swipe to the left
 * (at least 80 px, twice as wide as tall, under 600 ms), like Android's back gesture. */
export function isCloseSwipe(dx: number, dy: number, ms: number): boolean {
	return dx <= -80 && Math.abs(dx) >= 2 * Math.abs(dy) && ms < 600;
}

/** Initial query and preview state: a non-empty `resume` prefills the last query; otherwise
 * (null or '') the editor selection is the query. The preview follows the platform's setting
 * either way (a resumed search used to force it open, which ignored the mobile setting). */
export function startState(resume: string | null, previewSetting: boolean, selection: string): { query: string; previewOpen: boolean } {
	return { query: resume || selection, previewOpen: previewSetting };
}

export interface OpenAllDeps<F extends { path: string }> {
	resolve(path: string): F | null;
	noteOpen(path: string): void;
	openInTab(file: F): Promise<void>;
	restore(): void;
	close(): void;
	notice(msg: string): void;
}

/** The open-all runner: opens each resolvable path in a new tab (in order), then restores focus
 * and closes, also after a failure (with a Notice). A call while a run is in flight is ignored. */
export function createOpenAll<F extends { path: string }>(deps: OpenAllDeps<F>): (paths: string[]) => Promise<void> {
	let opening = false;
	return async (paths) => {
		if (opening) return;
		opening = true;
		try {
			for (const path of paths) {
				const file = deps.resolve(path);
				if (!file) continue;
				deps.noteOpen(path);
				await deps.openInTab(file);
			}
		} catch (e) {
			deps.notice(`Fast Find 10K: open all failed (${String(e)})`);
		} finally {
			opening = false;
			deps.restore();
			deps.close();
		}
	};
}

/** A semantic neighbour as a tier-5 row (opens at the top of the note; badge shows the score). */
function semanticHit(h: SemanticHit): Hit {
	return {
		path: h.path,
		title: h.title,
		titleNorm: '',
		tier: 5,
		score: h.score,
		badge: 'Body',
		titleRanges: [],
		heading: null,
		excerpt: null,
		line: null,
		isGhost: false,
		ghostRefs: 0,
		similarCount: 0,
		expanded: false,
	};
}

/** What the create guard needs to look up semantic neighbours of a note name. */
export interface GuardDeps {
	settings: Pick<FastFindSettings, 'useSmartConnections' | 'semantic' | 'semanticMinSimilarity'>;
	bridge: Pick<SmartConnectionsBridge, 'available' | 'hasVectors' | 'embed'> | null;
	engine: Pick<EngineClient, 'semantic'> | null;
}

/** Up to 3 semantic neighbours of `name` labelled with their score; empty with semantics off
 * (`never` or no Smart Connections) or on any failure. The bridge bounds the embedding to 1.5 s. */
export async function semanticGuard(name: string, { settings, bridge, engine }: GuardDeps): Promise<PanelItem[]> {
	if (settings.semantic === 'never' || !settings.useSmartConnections) return [];
	if (!bridge?.available() || !bridge.hasVectors() || !engine) return [];
	const vec = await bridge.embed(name);
	if (!vec) return [];
	try {
		const hits = await engine.semantic(vec, GUARD_SEMANTIC, settings.semanticMinSimilarity);
		return hits.map((h) => ({ path: h.path, title: h.title, note: h.score.toFixed(2) }));
	} catch {
		return [];
	}
}

/** Create-guard flow: lexical candidates show at once and semantic ones are appended when they
 * arrive; without lexical ones it waits for the (time-bounded) semantic lookup, then shows or
 * creates. `current()` false (the user moved on) drops every step. */
export async function runGuard(
	lexical: PanelItem[],
	semantic: Promise<PanelItem[]>,
	ui: { current(): boolean; show(items: PanelItem[]): void; update(items: PanelItem[]): void; create(): unknown },
): Promise<void> {
	const merge = (sem: PanelItem[]) => {
		const seen = new Set(lexical.map((i) => i.path));
		return [...lexical, ...sem.filter((i) => !seen.has(i.path))];
	};
	if (lexical.length > 0) {
		if (!ui.current()) return;
		ui.show(lexical);
		void semantic.then((sem) => {
			const all = merge(sem);
			if (all.length > lexical.length && ui.current()) ui.update(all);
		});
		return;
	}
	const sem = await semantic;
	if (!ui.current()) return;
	if (sem.length > 0) ui.show(sem);
	else await ui.create();
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
	/** Owns what Obsidian's renderer mounts in the preview (diagrams); unloaded with the modal. */
	private readonly renderComponent = new Component();
	private statusEl!: HTMLElement;
	private pillAll!: HTMLElement;
	private pillFiles!: HTMLElement;
	private pillLang!: HTMLElement;
	private pillMatches: HTMLElement | null = null;
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
	/** Create guard ("Already have?") in progress: the note name, and the link target when the
	 * note is created for a ghost (its path comes from the target). */
	private guard: { name: string; ghost: string | null } | null = null;
	/** Bumped per create request; a slower guard lookup that lost the race is dropped. */
	private createSeq = 0;
	/** Bumped per `render`; a semantic lookup for an older render is dropped. */
	private renderSeq = 0;
	/** The semantic group is in `state.hits` for the current render. */
	private semanticShown = false;
	/** Query for which Ctrl+S asked for the semantic tier (kept across re-renders of that query). */
	private semanticForced: string | null = null;
	private emptyEl: HTMLElement | null = null;
	private previewEl!: HTMLElement;
	private helpEl!: HTMLElement;
	/** Pushed while the help overlay is shown (Esc and the help key close it). */
	private helpScope: Scope | null = null;
	/** The footer's "open all" hint; null when the footer is hidden (mobile, key hints off). */
	private openAllHint: HTMLElement | null = null;
	/** Query to prefill instead of the editor selection ("Search (resume last)"); null or '' = none. */
	private readonly resume: string | null;

	/** `resume`: prefill this query and show the preview whatever the setting says. */
	constructor(app: App, plugin: FastFindPlugin, mode: Mode, resume: string | null = null) {
		super(app);
		this.plugin = plugin;
		this.resume = resume;
		this.state = {
			query: '',
			mode,
			hits: [],
			selected: 0,
			totalDocs: 0,
			elapsedUs: 0,
			previewOpen: startState(resume, Platform.isMobile ? plugin.settings.previewOpenMobile : plugin.settings.previewOpen, '').previewOpen,
			debug: plugin.settings.debugByDefault,
		};
		this.registerKeys(plugin.settings.keymap);
	}

	onOpen(): void {
		this.modalEl.addClass('ff10k-modal');
		this.modalEl.toggleClass('ff10k-preview-hidden', !this.state.previewOpen);
		this.modalEl.toggleClass('ff10k-mobile', Platform.isMobile);
		const { contentEl } = this;
		contentEl.empty();
		contentEl.addClass('ff10k-content');

		const bar = contentEl.createDiv({ cls: 'ff10k-bar' });
		this.inputEl = bar.createEl('input', { cls: 'ff10k-input', type: 'text', attr: { placeholder: 'Find a note…', spellcheck: 'false' } });
		if (Platform.isMobile) {
			// no Esc on a phone: a close button at the end of the search line
			const close = bar.createEl('button', { cls: 'ff10k-close', text: '✕', attr: { 'aria-label': 'Close search', type: 'button' } });
			close.addEventListener('click', () => this.close());
			// swipe left closes too, also where the OS has no back gesture (iOS); not from the
			// input, where a horizontal drag moves the text cursor
			let start: { x: number; y: number; t: number } | null = null;
			this.modalEl.addEventListener('touchstart', (e) => {
				const t = e.touches[0];
				start = e.touches.length === 1 && t && e.target !== this.inputEl ? { x: t.clientX, y: t.clientY, t: e.timeStamp } : null;
			}, { passive: true });
			this.modalEl.addEventListener('touchend', (e) => {
				const t = e.changedTouches[0];
				if (start && t && isCloseSwipe(t.clientX - start.x, t.clientY - start.y, e.timeStamp - start.t)) this.close();
				start = null;
			}, { passive: true });
		}
		// pills: tap targets; their key labels are omitted on mobile (no keyboard shortcuts there)
		const key = (spec: string) => (Platform.isMobile ? '' : ` ${keyLabel(spec)}`);
		const pills = Platform.isMobile ? bar.createDiv({ cls: 'ff10k-pills' }) : bar;
		this.pillAll = pills.createSpan({ cls: 'ff10k-pill', text: 'all' });
		this.pillFiles = pills.createSpan({ cls: 'ff10k-pill', text: `files only${key(this.plugin.settings.keymap.filesOnly)}` });
		this.pillLang = pills.createSpan({
			cls: 'ff10k-pill',
			text: `+languages${key(this.plugin.settings.keymap.toggleLanguages)}`,
			attr: { title: 'Also search the German word list and the dictionary note' },
		});
		if (Platform.isMobile) {
			const pillPreview = pills.createSpan({ cls: 'ff10k-pill', text: 'preview' });
			pillPreview.toggleClass('is-active', this.state.previewOpen);
			pillPreview.addEventListener('click', () => {
				this.togglePreview();
				pillPreview.toggleClass('is-active', this.state.previewOpen);
			});
			// "matches": only the lines around each match in the preview (Ctrl+M on desktop);
			// wired to the preview once it exists (below)
			const pillMatches = pills.createSpan({ cls: 'ff10k-pill', text: 'matches', attr: { title: 'Preview: matches only / whole note' } });
			pillMatches.addEventListener('click', () => {
				this.toggleMatchesOnly(true);
				pillPreview.toggleClass('is-active', this.state.previewOpen);
			});
			this.pillMatches = pillMatches;
		}
		this.statusEl = pills.createSpan({ cls: 'ff10k-status' });
		this.pillAll.addEventListener('click', () => this.setMode('all'));
		this.pillFiles.addEventListener('click', () => this.setMode('filesOnly'));
		this.pillLang.addEventListener('click', () => this.toggleLanguages());

		const body = contentEl.createDiv({ cls: 'ff10k-body' });
		this.resultsEl = body.createDiv({ cls: 'ff10k-results' });
		const right = body.createDiv({ cls: 'ff10k-right' });
		this.previewEl = right.createDiv({ cls: 'ff10k-preview' });
		this.renderComponent.load();
		this.preview = new Preview(
			this.previewEl,
			(target, from) => {
				if (/^https?:\/\//i.test(target)) return target;
				const file = this.app.metadataCache.getFirstLinkpathDest(decodeURIComponent(target), from);
				return file ? this.app.vault.getResourcePath(file) : null;
			},
			(markdown, el) => MarkdownRenderer.render(this.app, markdown, el, this.state.hits[this.state.selected]?.path ?? '', this.renderComponent),
		);
		if (this.pillMatches) {
			const pill = this.pillMatches;
			pill.toggleClass('is-active', this.preview.matchesOnly);
			this.preview.onMatchesOnlyChange = (on) => pill.toggleClass('is-active', on);
		}
		this.similar = new SimilarPanel(right.createDiv({ cls: 'ff10k-similar' }));
		this.modalEl.toggleClass('ff10k-no-similar', !this.similarOn());
		this.similar.onOpen((path) => void this.openPath(path));
		this.resultsEl.addEventListener('scroll', () => {
			const r = this.resultsEl;
			if (r.scrollTop + r.clientHeight >= r.scrollHeight - 80) this.renderUpTo(this.state.hits.length);
		});

		const hints = this.plugin.settings.keyHints;
		if (hints === 'always' || (hints === 'desktop' && !Platform.isMobile)) this.renderFooter(contentEl.createDiv({ cls: 'ff10k-footer' }));
		this.helpEl = contentEl.createDiv({ cls: 'ff10k-help' });
		this.helpEl.hide();

		const selection = this.app.workspace.activeEditor?.editor?.getSelection().trim() ?? '';
		this.inputEl.value = startState(this.resume, this.state.previewOpen, selection).query;
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
		this.renderComponent.unload();
		this.closed = true;
		this.plugin.rememberQuery(this.inputEl.value);
		window.cancelAnimationFrame(this.paintFrame);
		this.hideHelp();
		this.leaveSimilar();
		this.unsubscribeStatus?.();
		this.unsubscribeStatus = null;
		this.contentEl.empty();
	}

	private registerKeys(km: Keymap): void {
		const actions: Record<keyof Keymap, () => void> = {
			open: () => void this.openSelected('same'),
			openTab: () => void this.openSelected('tab'),
			openSplit: () => void this.openSelected('split'),
			create: () => void this.startCreate('same'),
			createTab: () => void this.startCreate('tab'),
			semantic: () => this.forceSemantic(),
			focusSimilar: () => this.enterSimilar(km),
			panelRight: () => this.enterSimilar(km),
			panelLeft: () => this.leaveSimilar(),
			filesOnly: () => this.setMode(this.state.mode === 'all' ? 'filesOnly' : 'all'),
			moveDown: () => this.select(this.state.selected + 1),
			moveUp: () => this.select(this.state.selected - 1),
			togglePreview: () => this.togglePreview(),
			toggleDebug: () => this.toggleDebug(),
			previewDown: () => this.scrollPreview(1),
			previewUp: () => this.scrollPreview(-1),
			nextMatch: () => this.jumpMatch(1),
			prevMatch: () => this.jumpMatch(-1),
			toggleMatchesOnly: () => this.toggleMatchesOnly(),
			showHelp: () => this.toggleHelp(),
			openAll: () => void this.openAll(),
			toggleLanguages: () => this.toggleLanguages(),
			clearQuery: () => this.clearQuery(),
		};
		const bind = (spec: string, fn: () => void) => {
			for (const part of splitSpecs(spec)) {
				const k = parseKeySpec(part);
				if (!k) {
					console.warn('Fast Find 10K: invalid key binding', part);
					continue;
				}
				this.scope.register(k.modifiers, k.key, swallowKey(fn));
			}
		};
		for (const name of Object.keys(actions) as (keyof Keymap)[]) {
			if (name === 'clearQuery') continue;
			bind(km[name], actions[name]);
		}
		// clearQuery passes through (copy) while text is selected in the field
		for (const part of splitSpecs(km.clearQuery)) {
			const clear = parseKeySpec(part);
			if (!clear) continue;
			this.scope.register(clear.modifiers, clear.key, (evt) => {
				const i = this.inputEl;
				if (document.activeElement === i && i.selectionStart !== i.selectionEnd) return true;
				evt.preventDefault();
				actions.clearQuery();
				return false;
			});
		}
		bind('ArrowDown', actions.moveDown);
		bind('ArrowUp', actions.moveUp);
	}

	/** Empties the query and re-runs (the recent list); the previous query stays saved for "Search (resume last)". */
	private clearQuery(): void {
		if (this.inputEl.value === '') return;
		this.plugin.rememberQuery(this.inputEl.value);
		this.inputEl.value = '';
		this.leaveSimilar();
		this.inputEl.focus();
		this.search();
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
				debugLog('search', query, res.elapsedUs, 'us', paintMs);
				// after the lexical paint, never before it
				void this.addSemantic(this.semanticForced === query);
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
		this.renderSeq++;
		this.semanticShown = false;
		if (this.semanticForced !== this.state.query) this.semanticForced = null;
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
		this.emptyEl = res.hits.length === 0 ? this.resultsEl.createDiv({ cls: 'ff10k-empty', text: this.state.query ? 'No matches' : 'No recent notes' }) : null;
		this.renderStatus();
		this.updateOpenAllHint();
		this.schedulePaint();
	}

	/** Ctrl+S: the semantic tier for the current query (unless the setting says never). */
	private forceSemantic(): void {
		if (!this.state.query.trim() || this.plugin.settings.semantic === 'never') return;
		const bridge = this.plugin.bridge;
		if (!this.plugin.settings.useSmartConnections || !bridge?.available() || !bridge.hasVectors()) {
			new Notice('Fast Find 10K: Smart Connections embeddings not available', 2000);
			return;
		}
		this.semanticForced = this.state.query;
		void this.addSemantic(true);
	}

	/** Appends the `semantic` group (tier 5) for the rendered query: automatically when the
	 * lexical hits are few, or when `forced` (Ctrl+S). Dropped when the query or render changed. */
	private async addSemantic(forced: boolean): Promise<void> {
		const { settings, bridge, engine } = this.plugin;
		const query = this.state.query;
		// "few" counts full lexical matches (tiers 1-3), not the partial tier shown because of them (spec 6.3)
		const full = this.state.hits.filter((h) => h.tier >= 1 && h.tier <= 3).length;
		// `always`: a query embedding costs a few ms, so the tier is appended for every query
		const auto = settings.semantic === 'always' || (settings.semantic === 'auto' && full < settings.fewThreshold);
		if (this.semanticShown || !query.trim() || settings.semantic === 'never' || !(forced || auto)) return;
		if (!settings.useSmartConnections || !bridge?.available() || !bridge.hasVectors() || !engine) return;
		const gen = this.renderSeq;
		const current = () => !this.closed && gen === this.renderSeq && query === this.inputEl.value && !this.semanticShown;
		const vec = await bridge.embed(query);
		if (!vec || !current()) return;
		let found: SemanticHit[];
		try {
			found = await engine.semantic(vec, SEMANTIC_ROWS, settings.semanticMinSimilarity);
		} catch {
			return;
		}
		if (!current()) return;
		const lexical = new Set(this.state.hits.map((h) => h.path));
		const rows = found.filter((f) => !lexical.has(f.path)).map(semanticHit);
		this.semanticShown = true;
		if (rows.length === 0) return;
		const first = this.state.hits.length;
		this.state.hits.push(...rows);
		this.emptyEl?.remove();
		this.emptyEl = null;
		// at most LIMIT lexical rows: render the rest so the group is reachable without scrolling first
		this.renderUpTo(this.state.hits.length);
		if (forced) this.rowEls[first]?.scrollIntoView({ block: 'nearest' });
		this.renderStatus();
		this.updateOpenAllHint();
		if (first === 0) this.schedulePaint(); // nothing was selected before
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
			renderRow(row, hit, i === this.state.selected, this.similarOn());
			row.addEventListener('click', () => {
				// Mobile with the preview on: the first tap selects (and previews) the row, a tap on
				// the selected row opens it. Everywhere else a click opens at once.
				if (Platform.isMobile && this.state.previewOpen && this.state.selected !== i) {
					this.select(i);
					return;
				}
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
			[[km.filesOnly], 'files only'],
			[[km.semantic], 'semantic'],
			[[km.moveDown, km.moveUp], 'move'],
			[[km.nextMatch, km.prevMatch], 'next / prev match'],
			[[km.toggleMatchesOnly], 'matches only'],
			[[km.togglePreview], 'toggle preview'],
			[[km.focusSimilar, km.panelRight], 'similar notes'],
			[[km.toggleDebug], 'why ranked'],
			[[km.toggleLanguages], 'languages'],
			[[km.openAll], 'open all'],
			[[km.clearQuery], 'clear'],
			[[km.showHelp], 'help'],
			[['Esc'], 'close'],
		];
		for (const [keys, label] of hints) {
			const span = el.createSpan({ cls: 'ff10k-hint' });
			span.createEl('kbd', { text: keys.map(keyLabel).join(' / ') });
			span.appendText(` ${label}`);
			if (label === 'open all') this.openAllHint = span;
		}
		this.openAllHint?.hide();
	}

	/** The "open all" hint shows only while the shown notes are few enough. */
	private updateOpenAllHint(): void {
		this.openAllHint?.toggle(openAllTargets(this.state.hits).eligible);
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
		window.cancelAnimationFrame(this.paintFrame);
		this.paintFrame = window.requestAnimationFrame(() => {
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
		void this.preview.show(engine, hit, queryTerms(this.state.query));
		if (this.similarOn()) void this.similar.show(engine, hit, this.plugin.settings.minSimilarity, this.plugin.bridge?.hasVectors() ?? false);
		else this.similar.clear();
	}

	/** The similar-notes panel and pill, per the setting and platform. The create guard shows the
	 * panel regardless, since its candidates are the whole point of the guard. */
	private similarOn(): boolean {
		const s = this.plugin.settings.similarPanel;
		return s === 'always' || (s === 'desktop' && !Platform.isMobile);
	}

	/** Tab: keyboard focus into the similar panel; its own scope handles move/open/leave.
	 * In the create guard the same scope also confirms creation. */
	private enterSimilar(km: Keymap, guard = false): void {
		if (this.similarScope || (!guard && (!this.state.previewOpen || !this.similarOn())) || !this.similar.focus()) return;
		const scope = new Scope(this.scope);
		const bind = (spec: string, fn: () => void) => {
			for (const part of splitSpecs(spec)) {
				const k = parseKeySpec(part);
				if (k)
					scope.register(k.modifiers, k.key, () => {
						fn();
						return false;
					});
			}
		};
		const leave = () => this.leaveSimilar();
		if (guard) {
			bind(km.create, () => void this.confirmCreate('same'));
			bind(km.createTab, () => void this.confirmCreate('tab'));
		} else {
			bind(km.focusSimilar, leave);
			bind(km.panelLeft, leave);
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
			this.modalEl.toggleClass('ff10k-no-similar', !this.similarOn());
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

	/** Shift+Enter: the selected ghost's link target, else the query. With the guard on and
	 * existing notes close to the name, shows them first ("Already have?"); else creates now. */
	private async startCreate(where: OpenWhere): Promise<void> {
		if (this.guard) return this.confirmCreate(where);
		const hit = this.state.hits[this.state.selected];
		const query = this.inputEl.value;
		const ghost = hit?.isGhost ? hit.title : null;
		const name = ghost === null ? sanitizeName(query) : (ghostNotePath(ghost)?.split('/').pop()?.slice(0, -'.md'.length) ?? '');
		if (!name) {
			new Notice(ghost === null ? 'Fast Find 10K: type a name to create a note' : `Fast Find 10K: cannot create a note for the link "${ghost}"`);
			return;
		}
		this.leaveSimilar();
		const seq = ++this.createSeq;
		if (!this.plugin.settings.createGuard) return this.create(name, ghost, where);
		const semantic = semanticGuard(name, this.plugin); // in parallel with the lexical lookup
		const lexical = await this.lexicalGuard(name);
		// still the same create request, the modal open, the query unchanged, no guard from elsewhere
		const current = () => !this.closed && seq === this.createSeq && this.inputEl.value === query;
		if (lexical === null || !current()) return;
		await runGuard(lexical, semantic, {
			current: () => current() && (this.guard === null || this.guard.name === name),
			show: (items) => {
				window.cancelAnimationFrame(this.paintFrame);
				this.guard = { name, ghost };
				this.modalEl.removeClass('ff10k-preview-hidden');
				this.modalEl.removeClass('ff10k-no-similar');
				this.similar.showGuard(items);
				this.enterSimilar(this.plugin.settings.keymap, true);
			},
			// only while that guard is still up (Esc or typing ends it)
			update: (items) => {
				if (this.guard?.name === name) this.similar.showGuard(items, true);
			},
			create: () => this.create(name, ghost, where),
		});
	}

	/** Existing notes whose title/alias matches `name` (tier 1, no ghosts). Null when a newer
	 * search superseded the lookup (the user kept typing): then nothing is created. */
	private async lexicalGuard(name: string): Promise<PanelItem[] | null> {
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
		if (this.guard) await this.create(this.guard.name, this.guard.ghost, where);
	}

	private async create(name: string, ghost: string | null, where: OpenWhere): Promise<void> {
		const file = ghost === null ? await createNote(this.app, name, where) : await createGhostNote(this.app, ghost, where);
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
		this.pillLang.toggleClass('is-active', this.plugin.languagesActive);
	}

	/** Ctrl+L: language expansion on/off for this session (the config change precedes the search in the worker queue). */
	private toggleLanguages(): void {
		this.plugin.toggleLanguages();
		this.updatePills();
		this.search();
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

	/** Alt+J / Alt+K: the preview by half its visible height; nothing while it is hidden. */
	private scrollPreview(dir: 1 | -1): void {
		if (!this.state.previewOpen) return;
		this.previewEl.scrollBy({ top: (dir * this.previewEl.clientHeight) / 2 });
	}

	/** Alt+N / Alt+Shift+N: the next / previous highlighted match in the preview. */
	private jumpMatch(dir: 1 | -1): void {
		if (!this.state.previewOpen) return;
		this.preview.nextMatch(dir);
	}

	/** Ctrl+M (or the mobile "matches" pill): only the lines around each match, or the whole
	 * note. The pill opens a hidden preview first so the tap shows something. */
	private toggleMatchesOnly(fromPill = false): void {
		if (!this.state.previewOpen) {
			if (!fromPill) return;
			this.togglePreview();
			if (this.preview.matchesOnly) return;
		}
		this.preview.toggleMatchesOnly();
	}

	/** Opens every shown note (≤ OPEN_ALL_MAX, ghosts excluded) in background tabs, then closes. */
	private readonly runOpenAll = createOpenAll({
		resolve: (path) => {
			const f = this.app.vault.getAbstractFileByPath(path);
			return f instanceof TFile ? f : null;
		},
		noteOpen: (path) => this.noteOpen(path),
		openInTab: async (file) => {
			if (!this.openAllPrev) {
				const ws = this.app.workspace;
				this.openAllPrev = { leaf: ws.getMostRecentLeaf(), path: ws.getActiveFile()?.path ?? null };
			}
			await this.app.workspace.getLeaf('tab').openFile(file, { active: false });
		},
		// getLeaf('tab') activates each new tab: give focus back to where the user was, without
		// that note's `file-open` counting as an access
		restore: () => {
			const prev = this.openAllPrev;
			this.openAllPrev = null;
			if (!prev?.leaf) return;
			if (prev.path) this.plugin.frecency?.suppressNext(prev.path);
			this.app.workspace.setActiveLeaf(prev.leaf, { focus: true });
		},
		close: () => this.close(),
		notice: (msg) => new Notice(msg),
	});
	/** The leaf (and its note) active before an open-all run. */
	private openAllPrev: { leaf: WorkspaceLeaf | null; path: string | null } | null = null;

	private async openAll(): Promise<void> {
		const t = openAllTargets(this.state.hits);
		if (!t.eligible) {
			if (t.count > 0) new Notice(`Fast Find 10K: Open all works for ${OPEN_ALL_MAX} or fewer results (${t.count} shown)`);
			return;
		}
		await this.runOpenAll(t.paths);
	}

	private toggleHelp(): void {
		if (this.helpScope) return this.hideHelp();
		renderHelp(this.helpEl, this.plugin.settings.keymap, Platform.isMacOS);
		this.helpEl.show();
		const scope = new Scope(this.scope);
		const hide = () => {
			this.hideHelp();
			return false;
		};
		scope.register([], 'Escape', hide);
		for (const part of splitSpecs(this.plugin.settings.keymap.showHelp)) {
			const k = parseKeySpec(part);
			if (k) scope.register(k.modifiers, k.key, hide);
		}
		this.helpScope = scope;
		this.app.keymap.pushScope(scope);
	}

	private hideHelp(): void {
		if (!this.helpScope) return;
		this.app.keymap.popScope(this.helpScope);
		this.helpScope = null;
		this.helpEl.hide();
	}
}
