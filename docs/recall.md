# Recall: does the finder miss notes?

Measured 2026-10-27 on the test vault (9,385 indexed notes, a copy of the owner's vault) with
`ff10k recall`, and the same queries through Omnisearch 1.31.0 (`api.search`, fuzziness `1`,
weights basename 10 / H1 10 / H2 9 / H3 7 as set in that vault) inside Obsidian.

## Method

For every note, three queries are derived from its file name:

| Variant | Query | Notes used |
|---|---|---|
| full | the whole title, as typed | all |
| two | the first two words | titles of 3+ words |
| typo | the longest word of 6+ letters with its middle letter deleted (`GitHub` → `Gitub`) | titles with such a word |

A query counts as a hit at k when the note itself (by path) is among the first k results. A
same-title duplicate in another folder ranking first is counted separately. Fast Find 10K runs in
mode "all" with the default settings and no frecency; Omnisearch runs with the vault's settings.
`--sample-out` writes the queries so both tools see the identical set.

```sh
ff10k recall <vault> --all-misses                      # every note
ff10k recall <vault> --every 10 --sample-out s.json    # the 939-note sample used below
```

## Results

Fast Find 10K, every note (9,385 notes, 19,780 queries):

| Variant | Queries | @1 | @5 | @10 | duplicate first | p50 | p95 |
|---|---|---|---|---|---|---|---|
| full | 9,385 | 97.81 % | 99.53 % | 99.61 % | 58 | 2.5 ms | 11.9 ms |
| two | 4,072 | 57.66 % | 79.94 % | 86.12 % | 6 | 2.4 ms | 6.2 ms |
| typo | 6,323 | 91.03 % | 94.48 % | 94.97 % | 32 | 3.0 ms | 13.8 ms |

Same 939-note sample (every 10th note), both tools:

| Variant | Fast Find 10K @1 / @5 / @10 | Omnisearch @1 / @5 / @10 | FF10K p50 / p95 | Omnisearch p50 / p95 |
|---|---|---|---|---|
| full (939) | 97.34 / 99.68 / 99.79 % | 97.76 / 100 / 100 % | 2.8 / 11.4 ms | 5.8 / 58.7 ms |
| two (421) | 56.53 / 80.76 / 87.89 % | 71.50 / 87.41 / 90.26 % | 2.7 / 6.8 ms | 6.5 / 44 ms |
| typo (634) | 91.64 / 94.64 / 95.27 % | 78.23 / 83.75 / 84.70 % | 3.1 / 12.9 ms | 4.8 / 63.7 ms |

Latencies are not directly comparable: Fast Find 10K ran natively (release build, one core),
Omnisearch inside Obsidian's renderer. Inside Obsidian a Fast Find 10K query takes about 5 ms for
one word and 15 ms for three (see [perf.md](perf.md)).

## What the misses are (Fast Find 10K)

Full title, 37 misses at @10 out of 9,385 (every one inspected):

- **`#` in the title (20).** `RW Data Engineering Weekly #115`, `RW Joe's Nerdy Rants #6`,
  `#657 — Professor John Vervaeke …`: `#115` is parsed as a tag filter, no note has that tag, the
  result is empty. Omnisearch finds these.
- **A word starting with `-` (4).** `KW13 -Marktleistung`, `Regular Expression in SSIS with C# &
  -NET`, `xattr -rd com.apple.quarantine`: `-NET` is parsed as an exclusion and removes the note
  itself.
- **Dotted names (7).** `jasper.ai`, `Otter.ai`, `Perplexity.ai`, `Gable.ai`, `Bodo.ai`,
  `vanna.ai`: the tokenizer joins compounds only on `-`, so `jasper.ai` becomes the two words
  `jasper` and `ai`. The engine takes the first as the whole term and, where it does not match,
  falls back to the remaining part alone, so `ai` matches about a thousand notes at quality 0.95.
  The real note matches `jasper` exactly but is charged for `ai` as an unmatched title word
  (`titleFactor ×0.85`) and lands around rank 100.
- **Exact one-word title tied with compounds (1).** `data` → the empty note `data.md` ranks 20th
  behind `DataFusion`, `HelloDATA`, `DataOps`: all score the same 100 (camel-case titles count as
  one fully covered word) and backlinks break the tie. There is no bonus for a title that equals
  the whole query.
- **Not findable by text (5).** `💡` (emoji only, no token), `Gonto Meetings -Office Hour Meeting`
  (also `-`), and three titles with both problems.

Typo, 318 misses out of 6,323: almost all are 6-letter words with a letter deleted (`Gitub`,
`Ollma`, `Klana`, `Entty`, `Menor`). The typo budget is computed from the typed length (spec
§6.2: 6–9 typed letters allow one typo, 10+ two), and a 5-letter query gets none, so a deletion
in a 6-letter word is never recovered while a substitution (`Githab`) is. The 204-query typo
test in `tests/typo_recall.rs` did not catch this because its fixtures have few 6-letter words.

Two words: not a recall problem. `RW The`, `RW How`, `RW Data` each match dozens of notes
equally (`RW` is a Readwise prefix on 2,000 notes); which of the equals is first comes down to
backlinks. Omnisearch's BM25 does better here at @1 (71.5 % vs 56.5 %) because it rewards
shorter documents; Fast Find 10K ranks equals by backlinks, then modification time.

## What the misses are (Omnisearch)

Typo: 15.3 % of the sampled one-typo queries return nothing at all (`RW DucDB + Delta Lake`,
`RW Shold We Get Rid of ETLs`, `Bascs`, `Conig`), including words of 8+ letters; its fuzziness
`1` tolerates fewer edits than frizbee with the guard rules. `obsidian troubelshoot` (two
letters swapped) returns zero results.

## Tests that exist

- 23 golden queries over the 56-note fixture vault (`tests/golden/core.toml`): ordering,
  must-not-appear decoys, tiers, similar counts. Run on every commit.
- 204 one-typo title queries over the fixtures (`tests/typo_recall.rs`), 0 misses allowed.
- 144 unit tests in the core (tokenizer, markdown extractor, index, matcher guard rules,
  ranking, optional words, abbreviations, similar, vectors).
- `ff10k recall`, this document: every note of a real vault, the number to watch.

## Open proposals (not applied; the owner asked for findings first)

1. Treat `#word` and `-word` as filters only when the tag or the excluded word exists in the
   index; otherwise search them as plain words. Fixes 24 of the 37 full-title misses.
2. Join compounds on `.` and `_` the way `-` is joined, and credit every matched part for title
   coverage. Fixes the `x.ai` family.
3. Typo budget from `max(typed length, token length)` for prefix candidates, or allow one typo
   at 5 typed letters when the query is a single word and the fuzzy hit is in the title. Fixes
   most of the 318 typo misses; needs a precision check on the golden decoys.
4. A small bonus when the whole title equals the query (`data` → `data.md`).
