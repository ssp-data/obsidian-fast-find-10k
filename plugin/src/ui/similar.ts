// Similar-notes panel: near-duplicates of the selected note (title overlap; cosine with vectors).

import type { EngineClient } from '../engine-client';
import type { Hit, SimilarHit, SimilarReason } from '../protocol';

const LIMIT = 5;

export function reasonLabel(reason: SimilarReason): string {
	if (reason === 'sameWordsReordered') return 'same words, different order';
	if (reason === 'copySuffix') return 'copy / punctuation variant';
	if (reason === 'sameTitleDifferentFolder') return 'same title, other folder';
	if ('similarTitle' in reason) return 'similar title';
	if ('semantic' in reason) return reason.semantic.toFixed(2);
	return '';
}

export class SimilarPanel {
	private readonly listEl: HTMLElement;
	private readonly hintEl: HTMLElement;
	private items: SimilarHit[] = [];
	private rowEls: HTMLElement[] = [];
	private selected = -1;
	private token = 0;
	private openCb: ((path: string) => void) | null = null;

	constructor(el: HTMLElement) {
		el.createDiv({ cls: 'ff10k-similar-label', text: 'similar notes · possible duplicates' });
		this.listEl = el.createDiv({ cls: 'ff10k-similar-list', attr: { tabindex: '-1' } });
		this.hintEl = el.createDiv({ cls: 'ff10k-similar-hint', text: 'Smart Connections off: title similarity only' });
		this.renderRows([]);
	}

	/** Ghost and recent (tier 0) hits show "—" without asking the engine; stale replies are dropped. */
	async show(engine: EngineClient, hit: Hit, minScore: number, hasVectors: boolean): Promise<void> {
		const token = ++this.token;
		this.hintEl.toggle(!hasVectors);
		if (hit.isGhost || hit.tier === 0) return this.renderRows([]);
		let items: SimilarHit[];
		try {
			items = await engine.similar(hit.path, LIMIT, minScore);
		} catch {
			items = [];
		}
		if (token === this.token) this.renderRows(items);
	}

	clear(): void {
		this.token++;
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

	private renderRows(items: SimilarHit[]): void {
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
			row.createSpan({ cls: 'ff10k-similar-reason', text: reasonLabel(item.reason) });
			row.addEventListener('click', () => this.openCb?.(item.path));
			this.rowEls.push(row);
		}
	}
}
