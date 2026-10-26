// Open actions for a search hit: same leaf, new tab or split, jumping to the matched line.

import { type App, MarkdownView, Notice, TFile } from 'obsidian';
import type { Hit } from './protocol';

export type OpenWhere = 'same' | 'tab' | 'split';

/** Opens `hit` and places the cursor on its matched heading/body line (0-based), centred.
 * Returns the opened file, or null when the path no longer resolves to a file. */
export async function open(app: App, hit: Hit, where: OpenWhere): Promise<TFile | null> {
	const file = app.vault.getAbstractFileByPath(hit.path);
	if (!(file instanceof TFile)) {
		new Notice(`Fast Find 10K: note not found (${hit.path})`);
		return null;
	}
	const line = hit.line ?? hit.heading?.line ?? null;
	const leaf = app.workspace.getLeaf(where === 'same' ? false : where === 'tab' ? 'tab' : 'split');
	await leaf.openFile(file, line === null ? { active: true } : { active: true, eState: { line } });
	const view = leaf.view;
	if (line !== null && view instanceof MarkdownView) {
		const editor = view.editor;
		const pos = { line: Math.min(line, editor.lastLine()), ch: 0 };
		editor.setCursor(pos);
		editor.scrollIntoView({ from: pos, to: pos }, true);
	}
	return file;
}
