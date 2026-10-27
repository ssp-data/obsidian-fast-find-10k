// Shortcut cheat-sheet overlay of the modal: every action with its current binding, grouped.

import { keyLabel } from '../keys';
import type { Keymap } from '../settings';

export interface HelpRow {
	action: keyof Keymap | 'close';
	label: string;
	/** Key specs (`Mod+Enter`); fixed alternatives such as the arrow keys come after the binding. */
	keys: string[];
}

export interface HelpGroup {
	title: string;
	rows: HelpRow[];
}

const GROUPS: [string, [keyof Keymap | 'close', string][]][] = [
	['Navigate', [['moveDown', 'next result'], ['moveUp', 'previous result'], ['previewDown', 'scroll preview down'], ['previewUp', 'scroll preview up'], ['focusSimilar', 'focus similar notes'], ['panelRight', 'similar notes (right)'], ['panelLeft', 'back to the results (left)']]],
	['Open', [['open', 'open'], ['openTab', 'open in new tab'], ['openSplit', 'open in split'], ['openAll', 'open all shown in tabs (≤ 10)']]],
	['Create', [['create', 'create note from query'], ['createTab', 'create note in new tab']]],
	['Modes & panels', [['filesOnly', 'toggle files only'], ['semantic', 'add semantic results'], ['toggleLanguages', 'toggle language expansion (German list, dictionary note)'], ['togglePreview', 'toggle preview'], ['toggleDebug', 'toggle why ranked']]],
	['Help', [['clearQuery', 'clear the query (copy while text is selected)'], ['showHelp', 'toggle this help'], ['close', 'close']]],
];

const FIXED: Partial<Record<HelpRow['action'], string>> = { moveDown: 'ArrowDown', moveUp: 'ArrowUp', close: 'Escape' };

/** The overlay's rows for `km`; every keymap action appears exactly once. */
export function helpGroups(km: Keymap): HelpGroup[] {
	return GROUPS.map(([title, rows]) => ({
		title,
		rows: rows.map(([action, label]) => {
			const fixed = FIXED[action];
			const keys = action === 'close' ? [] : [km[action]];
			return { action, label, keys: fixed ? [...keys, fixed] : keys };
		}),
	}));
}

/** Notes under the shortcut grid: a `Shift+<letter>` binding takes that capital letter away from
 * the query (the defaults Shift+J / Shift+K scroll the preview). */
export function helpNotes(km: Keymap, isMac: boolean): string[] {
	const shifted = [...new Set(Object.values(km).filter((k) => /^Shift\+[A-Za-z]$/.test(k)))];
	if (!shifted.length) return [];
	const letters = shifted.map((k) => k.slice(-1).toUpperCase());
	const labels = shifted.map((k) => keyLabel(k, isMac)).join(', ');
	const alt = letters.map((l) => keyLabel(`Alt+${l}`, isMac)).join(' / ');
	return [`${labels} are shortcuts here, so capital ${letters.join(', ')} cannot be typed into the query; rebind them (e.g. ${alt}) in the settings to type them.`];
}

const ARROWS: Record<string, string> = { ArrowDown: '↓', ArrowUp: '↑', Escape: 'Esc' };

/** Fills `el` (emptied first) with the grouped rows. */
export function renderHelp(el: HTMLElement, km: Keymap, isMac: boolean): void {
	el.empty();
	el.createDiv({ cls: 'ff10k-help-title', text: 'Keyboard shortcuts' });
	const grid = el.createDiv({ cls: 'ff10k-help-groups' });
	for (const g of helpGroups(km)) {
		const box = grid.createDiv({ cls: 'ff10k-help-group' });
		box.createDiv({ cls: 'ff10k-help-group-title', text: g.title });
		for (const r of g.rows) {
			const row = box.createDiv({ cls: 'ff10k-help-row' });
			const keys = row.createSpan({ cls: 'ff10k-help-keys' });
			r.keys.forEach((k, i) => {
				if (i > 0) keys.appendText(' / ');
				keys.createEl('kbd', { text: ARROWS[k] ?? keyLabel(k, isMac) });
			});
			row.createSpan({ cls: 'ff10k-help-label', text: r.label });
		}
	}
	for (const note of helpNotes(km, isMac)) el.createDiv({ cls: 'ff10k-help-note', text: note });
}
