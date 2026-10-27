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

Fast Find 10K, every note (9,385 notes), before and after the four fixes below (same vault, same
queries; "after" was run after the fixes, so the five fixture notes are not in it):

| Variant | Queries | @1 before → after | @5 before → after | @10 before → after | p50 | p95 |
|---|---|---|---|---|---|---|
| full | 9,385 | 97.81 % → **98.91 %** | 99.53 % → **99.97 %** | 99.61 % → 99.98 % | 2.8 ms | 12.9 ms |
| two | 4,072 | 57.66 % → 57.57 % | 79.94 % → 79.89 % | 86.12 % → 86.23 % | 2.7 ms | 6.5 ms |
| typo | 6,323 | 91.03 % → **97.90 %** | 94.48 % → **99.84 %** | 94.97 % → 99.98 % | 3.3 ms | 14.8 ms |

Same 939-note sample (every 10th note), both tools, before the fixes; the last column is Fast
Find 10K after the fixes (sample re-drawn the same way; the vault changed slightly in between,
so 400 instead of 421 titles had three or more words):

| Variant | Fast Find 10K @1 / @5 / @10 | Omnisearch @1 / @5 / @10 | FF10K p50 / p95 | Omnisearch p50 / p95 | FF10K after the fixes @1 / @5 / @10 |
|---|---|---|---|---|---|
| full (939) | 97.34 / 99.68 / 99.79 % | 97.76 / 100 / 100 % | 2.8 / 11.4 ms | 5.8 / 58.7 ms | 99.25 / 100 / 100 % |
| two (421) | 56.53 / 80.76 / 87.89 % | 71.50 / 87.41 / 90.26 % | 2.7 / 6.8 ms | 6.5 / 44 ms | 60.75 / 82.00 / 88.00 % (400) |
| typo (634) | 91.64 / 94.64 / 95.27 % | 78.23 / 83.75 / 84.70 % | 3.1 / 12.9 ms | 4.8 / 63.7 ms | 98.57 / 100 / 100 % (628) |

No abbreviations, language expansion or dictionary note were active in any run.

Latencies are not directly comparable: Fast Find 10K ran natively (release build, one core),
Omnisearch inside Obsidian's renderer. Inside Obsidian a Fast Find 10K query takes about 5 ms for
one word and 15 ms for three (see [perf.md](perf.md)).

## What the misses were (Fast Find 10K, before the fixes)

Full title, 37 misses at @10 out of 9,385 (every one inspected):

- **`#` in the title (20).** `RW Data Engineering Weekly #115`, `RW Joe's Nerdy Rants #6`,
  `#657 — Professor John Vervaeke …`: `#115` was parsed as a tag filter, no note has that tag, the
  result was empty.
- **A word starting with `-` (4).** `KW13 -Marktleistung`, `Regular Expression in SSIS with C# &
  -NET`, `xattr -rd com.apple.quarantine`: `-NET` was parsed as an exclusion and removed the note
  itself.
- **Dotted names (7).** `jasper.ai`, `Otter.ai`, `Perplexity.ai`, `Gable.ai`, `Bodo.ai`,
  `vanna.ai`: the tokenizer joined compounds only on `-`, so `jasper.ai` became the two words
  `jasper` and `ai`; the engine took the first as the whole term and, where it did not match,
  fell back to the remaining part alone, so `ai` matched about a thousand notes at quality 0.95
  while the real note was charged for `ai` as an unmatched title word.
- **Exact one-word title tied with compounds (1).** `data` → the empty note `data.md` ranked 20th
  behind `DataFusion`, `HelloDATA`, `DataOps`: all scored the same and backlinks broke the tie.
- **Bible chapters (4).** `1 Chr 1` ranked below `1 Chr 11`: a hit marks only the first
  occurrence of a word, so the second `1` counted as unmatched.
- **Not findable by text (1).** `💡` (emoji only, no token).

Typo, 318 misses out of 6,323: almost all 6-letter words with a letter deleted (`Gitub`, `Ollma`,
`Klana`, `Entty`, `Menor`). The typo budget came from the typed length (6–9 typed letters allow
one typo, 10+ two), so a 5-letter query got none.

Two words: not a recall problem, and expected to stay below Omnisearch at @1. `RW The`,
`RW How`, `RW Data` each match dozens of notes equally (`RW` is a Readwise prefix on 2,000
notes): every query word matched, the same number of title words unmatched, so the order among
them is a tie-break (backlinks, then modification date). Omnisearch's BM25 prefers the shorter
document, which agrees with the test's "the note itself" expectation more often. The note is in
the top 10 in 88 % of these ambiguous queries; one more typed word resolves them. Ranking equals
by document length would copy that behaviour, but the owner's vault has many short stub notes,
so it was not adopted.

## Fixes applied (owner-approved 2026-10-27, commits `7d6ed82`, `b15ad29`)

1. `#word` is a tag filter only when some note carries that tag, otherwise the word; `-word`
   with other words is retried as a word when excluding leaves no tier 1–3 hit (`-word` alone
   stays an exclusion).
2. The tokenizer joins compounds on `.` as on `-` and `_` (`jasper.ai` → `jasper.ai`, `jasper`,
   `ai`), so the part fallback needs every part and the title counts as covered.
3. Five typed letters get one typo against Title and Alias strings only (`Gitub` → "GitHub").
4. +15 in tier 1 when the title's words equal the query's words in any order; a repeated title
   word counts as covered.

Each fix has a unit test, a golden case with a fixture note (`tests/golden/core.toml`), and the
typo test now includes deletions in 6-letter words (234 queries, 0 misses). Remaining full-title
misses after the fixes: 2 (`-NET`, where excluding leaves other partial hits, and the emoji
title); remaining typo misses: 1 (`Fraen`, three notes titled "Fragen" in other folders rank
first).

## What the misses are (Omnisearch)

Typo: 15.3 % of the sampled one-typo queries return nothing at all (`RW DucDB + Delta Lake`,
`RW Shold We Get Rid of ETLs`, `Bascs`, `Conig`), including words of 8+ letters; its fuzziness
`1` tolerates fewer edits than frizbee with the guard rules. `obsidian troubelshoot` (two
letters swapped) returns zero results.

## Open question found on the way (not applied)

`invoice` ranks "Brand Voice of AIrbyte" and "My Writing Voice" (fuzzy: `n voice` matches 6 of
7 letters) above "Sales Invoice (ungleich Revenue)" (exact, but three unmatched title words,
`titleFactor ×0.55`). Within a tier the match kind does not order results, so a fuzzy hit in a
short title can beat an exact hit in a long one. Candidate rule: within a tier, a note whose
every unit matched only fuzzily sorts after notes with an exact or prefix hit.

## Tests that exist

- 23 golden queries over the 56-note fixture vault (`tests/golden/core.toml`): ordering,
  must-not-appear decoys, tiers, similar counts. Run on every commit.
- 204 one-typo title queries over the fixtures (`tests/typo_recall.rs`), 0 misses allowed.
- 144 unit tests in the core (tokenizer, markdown extractor, index, matcher guard rules,
  ranking, optional words, abbreviations, similar, vectors).
- `ff10k recall`, this document: every note of a real vault, the number to watch.
