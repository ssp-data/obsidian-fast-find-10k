// Similar-notes panel: near-duplicates of the selected note (title overlap; cosine with vectors).
// In the create guard it lists existing notes close to the name about to be created.

import type { EngineClient } from '../engine-client';
import type { Hit, SimilarHit, SimilarReason } from '../protocol';

const LIMIT = 5;
const SIMILAR_LABEL = 'similar notes · possible duplicates';

/** One panel row: `note` is the muted reason/score text. */
export interface PanelItem {
	path: string;
	title: string;
	note: string;
}

export function reasonLabel(reason: SimilarReason): string {
	if (reason === 'sameWordsReordered') return 'same words, different order';
	if (reason === 'copySuffix') return 'copy / punctuation variant';
	if (reason === 'sameTitleDifferentFolder') return 'same title, other folder';
	if ('similarTitle' in reason) return 'similar title';
	if ('semantic' in reason) return reason.semantic.toFixed(2);
	return '';
}

export class SimilarPanel {
	private readonly labelEl: HTMLElement;
	private readonly listEl: HTMLElement;
	private readonly hintEl: HTMLElement;
	private items: PanelItem[] = [];
	private rowEls: HTMLElement[] = [];
	private selected = -1;
	private token = 0;
	private openCb: ((path: string) => void) | null = null;

	constructor(el: HTMLElement) {
		this.labelEl = el.createDiv({ cls: 'ff10k-similar-label', text: SIMILAR_LABEL });
		this.listEl = el.createDiv({ cls: 'ff10k-similar-list', attr: { tabindex: '-1' } });
		this.hintEl = el.createDiv({ cls: 'ff10k-similar-hint', text: 'Smart Connections off: title similarity only' });
		this.renderRows([]);
	}

	/** Ghost and recent (tier 0) hits show "—" without asking the engine; stale replies are dropped. */
	async show(engine: EngineClient, hit: Hit, minScore: number, hasVectors: boolean): Promise<void> {
		const token = ++this.token;
		this.labelEl.setText(SIMILAR_LABEL);
		this.hintEl.toggle(!hasVectors);
		if (hit.isGhost || hit.tier === 0) return this.renderRows([]);
		let items: SimilarHit[];
		try {
			items = await engine.similar(hit.path, LIMIT, minScore);
		} catch {
			items = [];
		}
		if (token === this.token) this.renderRows(items.map((i) => ({ path: i.path, title: i.title, note: reasonLabel(i.reason) })));
	}

	/** Create guard: "Already have?" with the given existing notes (drops pending similar replies). */
	showGuard(items: PanelItem[]): void {
		this.token++;
		this.labelEl.setText('Already have?');
		this.hintEl.toggle(false);
		this.renderRows(items);
	}

	clear(): void {
		this.token++;
		this.labelEl.setText(SIMILAR_LABEL);
		this.renderRows([]);
	}

	/** Moves keyboard focus into the panel (first row selected). False when there is nothing to select. */
	focus(): boolean {
		if (this.items.length === 0) return false;
		this.setSelected(0);
		this.listEl.focus();
		return true;
	}

	blur(): void {
		this.setSelected(-1);
	}

	move(delta: number): void {
		if (this.items.length === 0) return;
		this.setSelected(Math.max(0, Math.min(this.items.length - 1, this.selected + delta)));
	}

	/** Opens the selected row through the `onOpen` callback. */
	openSelected(): void {
		const item = this.items[this.selected];
		if (item) this.openCb?.(item.path);
	}

	onOpen(cb: (path: string) => void): void {
		this.openCb = cb;
	}

	private setSelected(i: number): void {
		this.rowEls[this.selected]?.removeClass('is-selected');
		this.selected = i;
		const row = this.rowEls[i];
		row?.addClass('is-selected');
		row?.scrollIntoView({ block: 'nearest' });
	}

	private renderRows(items: PanelItem[]): void {
		this.items = items;
		this.rowEls = [];
		this.selected = -1;
		this.listEl.empty();
		if (items.length === 0) {
			this.listEl.createDiv({ cls: 'ff10k-similar-none', text: '—' });
			return;
		}
		for (const item of items) {
			const row = this.listEl.createDiv({ cls: 'ff10k-similar-row', attr: { title: item.path } });
			row.createSpan({ cls: 'ff10k-similar-title', text: item.title });
			row.createSpan({ cls: 'ff10k-similar-path', text: item.path });
			row.createSpan({ cls: 'ff10k-similar-reason', text: item.note });
			row.addEventListener('click', () => this.openCb?.(item.path));
			this.rowEls.push(row);
		}
	}
}
