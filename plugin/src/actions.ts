// Actions on a search hit: open (same leaf, new tab or split, jumping to the matched line),
// create a note from the query or an unresolved link.

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
	// The modal closes right after this and Obsidian then restores focus to the element that
	// had it before the search opened: with a new tab or split next to it, that is the old
	// note. Re-activate the opened leaf once that restore has happened.
	window.setTimeout(() => app.workspace.setActiveLeaf(leaf, { focus: true }), 0);
	return file;
}

const MAX_NAME = 200;

/** Leading/trailing dots and whitespace: a leading dot would make a hidden (never indexed) file. */
const stripEnds = (s: string) => s.replace(/^[\s.]+|[\s.]+$/g, '');

/** File name from a query or link target: drops `\ / : * ? " < > |` and the link-breaking
 * `# ^ [ ]`, collapses whitespace, trims (also leading and trailing dots), caps at 200 chars.
 * `''` when nothing is left. */
export function sanitizeName(raw: string): string {
	const s = raw.replace(/[\\/:*?"<>|#^[\]]/g, '').replace(/\s+/g, ' ');
	return stripEnds(stripEnds(s).slice(0, MAX_NAME));
}

/** Vault path (with `.md`) for an unresolved link target, which Obsidian resolves from the vault
 * root: each folder segment sanitized, a trailing `.md` dropped, a leading `/` ignored.
 * Null for an empty, `.` or `..` segment (or one that sanitizes to nothing). */
export function ghostNotePath(target: string): string | null {
	const raw = target.trim().replace(/^\/+/, '').replace(/\.md$/i, '').split('/');
	if (raw.some((seg) => ['', '.', '..'].includes(seg.trim()))) return null;
	const segs = raw.map(sanitizeName);
	return segs.some((seg) => !seg) ? null : `${segs.join('/')}.md`;
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
	return createAt(app, normalizePath(`${dir}${clean}.md`), where);
}

/** Creates the note an unresolved link points to, so that link resolves: a target with a folder
 * lands at that path from the vault root (missing folders created), one without goes where new
 * notes go. Null on failure (with a Notice). */
export async function createGhostNote(app: App, target: string, where: OpenWhere): Promise<TFile | null> {
	const path = ghostNotePath(target);
	if (!path) {
		new Notice(`Fast Find 10K: cannot create a note for the link "${target}"`);
		return null;
	}
	const slash = path.lastIndexOf('/');
	if (slash < 0) return createNote(app, path.slice(0, -'.md'.length), where);
	const parts = path.slice(0, slash).split('/');
	try {
		for (let i = 1; i <= parts.length; i++) {
			const dir = parts.slice(0, i).join('/');
			if (!app.vault.getFolderByPath(dir)) await app.vault.createFolder(dir);
		}
	} catch (e) {
		new Notice(`Fast Find 10K: could not create the folder for ${path} (${String(e)})`);
		return null;
	}
	return createAt(app, normalizePath(path), where);
}

/** Creates an empty note at `path` (or finds the existing one) and opens it. */
async function createAt(app: App, path: string, where: OpenWhere): Promise<TFile | null> {
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
