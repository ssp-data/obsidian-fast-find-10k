## What has changed?

A small release about the preview. Optional words such as `as` or `i` no longer light up all over a note, and a short query word is marked only where it stands as a word of its own, so the highlights in the preview point at the hits that matter.

**Highlights**

- **Optional words are not highlighted in the preview.** Words on the "Optional words (never required)" list (`a an the of to in on i` by default) still add to the score where they match, but the preview leaves them unmarked, so `should i change` marks "should" and "change" and skips every `i`. A query made only of optional words keeps its highlights. The setting's description now says so, and that an empty box means no optional words at all.
- **Short terms mark whole words only.** A query word of up to three letters is highlighted only as a word of its own, never inside a longer one, mirroring the engine's rule that such words match exactly. Longer words still mark substrings, so `code` still marks "codebase".

**Fixes**

- Searching for `as` painted the letters inside "basic", "cases" and every similar word across the preview, and the match counter in the header counted them too. Both now count and mark only the standalone word.

How optional words take part in ranking and in the preview is described in the README under "How ranking works" and the "Optional words" example.
