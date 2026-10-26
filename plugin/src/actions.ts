// Actions on a search hit: open (same leaf, new tab or split, jumping to the matched line),
// create a note from the query, insert a link at the cursor.

import { type App, MarkdownView, Notice, normalizePath, TFile } from 'obsidian';
import type { Hit } from './protocol';

export type OpenWhere = 'same' | 'tab' | 'split';

/** Opens `hit` and places the cursor on its matched heading/body line (0-based), centred.
 * Returns the opened file, or null when the path no longer resolves to a file. */
export async function open(app: App, hit: Pick<Hit, 'path' | 'line' | 'heading'>, where: OpenWhere): Promise<TFile | null> {
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

const MAX_NAME = 200;

/** File name from a query or link target: drops `\ / : * ? " < > |`, collapses whitespace,
 * trims (also trailing dots), caps at 200 chars. `''` when nothing is left. */
export function sanitizeName(raw: string): string {
	return raw
		.replace(/[\\/:*?"<>|]/g, '')
		.replace(/\s+/g, ' ')
		.trim()
		.replace(/\.+$/, '')
		.slice(0, MAX_NAME)
		.trim();
}

/** Creates `name.md` where Obsidian's "Default location for new notes" says, then opens it.
 * Opens the existing note instead when that path is taken. Null on failure (with a Notice). */
export async function createNote(app: App, name: string, where: OpenWhere): Promise<TFile | null> {
	const clean = sanitizeName(name);
	if (!clean) {
		new Notice('Fast Find 10K: type a name to create a note');
		return null;
	}
	const folder = app.fileManager.getNewFileParent(app.workspace.getActiveFile()?.path ?? '');
	const dir = folder.path === '/' || folder.path === '' ? '' : `${folder.path}/`;
	const path = normalizePath(`${dir}${clean}.md`);
	if (app.vault.getAbstractFileByPath(path) instanceof TFile) {
		new Notice(`Fast Find 10K: ${path} already exists, opening`);
	} else {
		try {
			await app.vault.create(path, '');
		} catch (e) {
			new Notice(`Fast Find 10K: could not create ${path} (${String(e)})`);
			return null;
		}
	}
	return open(app, { path, line: null, heading: null }, where);
}

/** Inserts a link to `hit` at the active editor's cursor (replacing the selection): the user's
 * link format via `generateMarkdownLink`, `#Heading` for heading hits, `[[target]]` for ghosts.
 * False (with a Notice) when there is no Markdown editor. */
export function insertLink(app: App, hit: Pick<Hit, 'path' | 'title' | 'isGhost' | 'badge' | 'heading'>): boolean {
	const view = app.workspace.getActiveViewOfType(MarkdownView);
	if (!view?.file) {
		new Notice('Fast Find 10K: open a note in an editor to insert a link');
		return false;
	}
	let link: string;
	if (hit.isGhost) {
		link = `[[${hit.title}]]`;
	} else {
		const file = app.vault.getAbstractFileByPath(hit.path);
		if (!(file instanceof TFile)) {
			new Notice(`Fast Find 10K: note not found (${hit.path})`);
			return false;
		}
		const isHeading = hit.badge === 'H1' || hit.badge === 'H2' || hit.badge === 'H3';
		const subpath = isHeading && hit.heading ? `#${hit.heading.text}` : undefined;
		link = app.fileManager.generateMarkdownLink(file, view.file.path, subpath);
	}
	view.editor.replaceSelection(link);
	return true;
}
