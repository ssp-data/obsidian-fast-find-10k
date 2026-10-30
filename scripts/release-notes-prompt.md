You are drafting the GitHub release notes for Fast Find 10K {{VERSION}} (an Obsidian search plugin: Rust engine in WebAssembly, Telescope-style finder for large vaults). Write them to the file RELEASE_NOTES.md in the current directory, overwriting it. Do not commit, do not touch any other file, do not run the test suite.

Sources, in this order of authority:
1. `git log --no-merges --format='%s%n%b' {{PREV}}..HEAD` — every commit since the previous release {{PREV}}. Subjects use conventional prefixes (feat, fix, docs, chore, ci, test); bodies explain the why. Condense, do not copy.
2. README.md, only to phrase a feature the way the documentation does (key names, setting names, syntax).
3. The current RELEASE_NOTES.md, only as a template for shape and tone — its content belongs to the previous release and must be replaced.

Structure (exactly this, Markdown):

## What has changed?

<Two or three sentences a user can read in ten seconds: the one theme of this release and what it means when they search. No commit hashes, no function names.>

**Highlights**

- 3 to 6 bullets. Each starts with a bold lead phrase, then one or two sentences. User-visible behaviour first; name the key, setting or query syntax in backticks when there is one. Skip internals unless they change what the user sees or configures.

**Fixes**

- Bullets for bugs a user could have hit, phrased as what went wrong and what is true now. Group small related ones into one bullet. Omit test-only, docs-only, CI and refactor work.

<Optional last line: one sentence on how to install or a pointer to a README section, only if it earns its place.>

Rules: plain English, no marketing adjectives, no exclamation marks, no emoji, no headings other than the one `##` above and the two bold labels. Do not mention this prompt, Claude, or AI. If the range since {{PREV}} contains nothing user-visible, say so in the intro and leave the bullet sections short rather than inventing content. Finish by printing the file's first ten lines so the caller can see it was written.
