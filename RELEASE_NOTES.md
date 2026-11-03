## What has changed?

A small release about accented and special letters. Searching `cafe` already found "Vienna Café", but the title lost its highlight and the preview said "no matches"; both now mark the accented text. Letters that have no plain-ASCII decomposition, such as `ß`, `ø` and `æ`, are now folded in the search itself, so a US keyboard finds "Straße" with `strasse` and "Søren" with `soren`.

**Highlights**

- **Special letters fold in search.** `ß`, `ø`, `æ`, `œ`, `ł`, `đ`, `ı`, `ð` and `þ` are matched as `ss`, `o`, `ae`, `oe`, `l`, `d`, `i`, `d` and `th` in titles, headings and content, for the typed query and the note alike. `soren kierkegaard`, `søren kierkegaard`, `strasse` and `straße` all land on the same note in the same tier. German `ä` still folds to `a`, not `ae`: `spati` finds "Späti", `spaeti` only through the typo allowance.
- **Highlights follow the engine's folding.** The title row and the preview now fold the displayed text the same way the engine does (case, diacritics, the letter table) and map the match back onto the original span. `muller` marks "Müller", `cafe` marks "Café", and the `3 / 12 matches` count and Alt+N / Alt+P jumps include those lines. Fuzzy (typo) and word-list hits are still not marked in the preview.

**Fixes**

- Searching an accented word with its plain spelling (`cafe` for "Café") found the note but showed no highlight on the title and "no matches" in the preview, because the preview compared lower-cased raw text while the engine compared folded text. Both now mark the match.
- Titles containing a character whose lower-case form changes its length (some Unicode letters, emoji sequences) dropped every highlight on that row. Highlights are now mapped by offset and only the unmatched range is skipped.

Numbers for a 100k-note vault (searchable after 0.6 s, 20–400 ms per query) are in the README's Performance section.
