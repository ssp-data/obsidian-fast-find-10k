// Key binding strings (`Mod+Alt+Enter`): parsing, labels for hints, validation against clashes.
// Pure (no Obsidian runtime): the platform is passed in.

import type { Modifier } from 'obsidian';
import type { Keymap } from './settings';

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

/** Hint text: `Mod+Enter` → `⌘⏎` on macOS, `Ctrl+⏎` elsewhere; `Shift+/` → `?`. */
export function keyLabel(spec: string, isMac: boolean): string {
	const sym: Record<string, string> = isMac
		? { Mod: '⌘', Ctrl: '⌃', Meta: '⌘', Alt: '⌥', Shift: '⇧', Enter: '⏎' }
		: { Mod: 'Ctrl+', Ctrl: 'Ctrl+', Meta: 'Win+', Alt: 'Alt+', Shift: 'Shift+', Enter: '⏎' };
	const parts = spec.split('+');
	if (parts.length > 1 && parts[parts.length - 1] === '/' && parts.includes('Shift')) {
		parts.splice(parts.indexOf('Shift'), 1);
		parts[parts.length - 1] = '?';
	}
	return parts.map((p) => sym[p] ?? p).join('');
}

/** The key combination a spec stands for on this platform (`Mod` resolved, order-free). */
function canonical(spec: string, isMac: boolean): string | null {
	const k = parseKeySpec(spec);
	if (!k) return null;
	const mods = new Set(k.modifiers.map((m) => (m === 'Mod' ? (isMac ? 'Meta' : 'Ctrl') : m)));
	return [...[...mods].sort(), k.key.toLowerCase()].join('+');
}

/** Why `spec` cannot be the binding of `action` (unparseable, or the same keys as another
 * action on this platform); null when it can. */
export function validateBinding(km: Keymap, action: keyof Keymap, spec: string, isMac: boolean): string | null {
	const c = canonical(spec.trim(), isMac);
	if (!c) return `invalid key "${spec}" (use e.g. Mod+Shift+Enter; modifiers Mod, Ctrl, Meta, Shift, Alt)`;
	for (const other of Object.keys(km) as (keyof Keymap)[]) {
		if (other !== action && canonical(km[other], isMac) === c) return `already used by ${other}`;
	}
	return null;
}

/** Pairs of actions bound to the same keys on this platform. */
export function conflicts(km: Keymap, isMac: boolean): [keyof Keymap, keyof Keymap][] {
	const seen = new Map<string, keyof Keymap>();
	const out: [keyof Keymap, keyof Keymap][] = [];
	for (const a of Object.keys(km) as (keyof Keymap)[]) {
		const c = canonical(km[a], isMac);
		if (c === null) continue;
		const prev = seen.get(c);
		if (prev) out.push([prev, a]);
		else seen.set(c, a);
	}
	return out;
}
