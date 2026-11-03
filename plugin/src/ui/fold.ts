// Text folding for highlights: the same shape the engine's `normalize` gives a string
// (NFKC, lowercase, diacritics stripped) plus an offset map back into the original text, so a
// match found in the folded string can be marked on the text the user sees.

/** Combining marks the engine keeps (Japanese voiced-sound marks). */
const KEEP_MARKS = /[゙゚]/;
const MARKS = /\p{M}/gu;

/** Per-code-point fold cache: non-ASCII text repeats its characters (umlauts, CJK). */
const cache = new Map<string, string>();

function foldChar(c: string): string {
	let f = cache.get(c);
	if (f === undefined) {
		f = c.normalize('NFKC').toLowerCase().normalize('NFD').replace(MARKS, (m) => (KEEP_MARKS.test(m) ? m : '')).normalize('NFC');
		if (cache.size > 4096) cache.clear();
		cache.set(c, f);
	}
	return f;
}

export interface Folded {
	text: string;
	/** `map[i]`: original UTF-16 offset of folded offset `i`, `map[text.length]` = original length.
	 * `null` when the fold changed nothing but case (ASCII): offsets are identical. */
	map: Uint32Array | null;
}

/** Folds `s` like the engine does and keeps an offset map; ASCII takes a fast path. */
export function fold(s: string): Folded {
	let ascii = true;
	for (let i = 0; i < s.length; i++) {
		if (s.charCodeAt(i) > 0x7f) {
			ascii = false;
			break;
		}
	}
	if (ascii) return { text: s.toLowerCase(), map: null };
	let text = '';
	const map: number[] = [];
	let i = 0;
	for (const c of s) {
		const f = c.length === 1 && c.charCodeAt(0) < 0x80 ? c.toLowerCase() : foldChar(c);
		for (let k = 0; k < f.length; k++) map.push(i);
		text += f;
		i += c.length;
	}
	map.push(s.length);
	return { text, map: Uint32Array.from(map) };
}

/** A folded range `[s, e)` mapped onto the original text. */
export function unfoldRange(f: Folded, s: number, e: number): [number, number] {
	return f.map ? [f.map[s], f.map[e]] : [s, e];
}

/** Folded form of a query term (no offsets needed). */
export function foldTerm(term: string): string {
	return fold(term).text;
}
