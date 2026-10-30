## What has changed?

A small release about the preview: "Search (resume last)" no longer forces the preview open, and on mobile the preview now starts open by default. Searching itself is unchanged; the README now explains why notes that match only some of your words show up under **partial**.

**Highlights**

- **Preview on by default on mobile.** `Preview open by default on mobile` under Settings → Behaviour now defaults to on, so the full-screen search shows the selected note's text right away; the `preview` pill still hides it. Settings saved by an earlier version keep their value, so turn the toggle on if you want the new default.
- **Partial matches and OR, documented.** The README's query syntax and ranking sections now explain that plain words are AND-ed, that notes matching only some of them appear further down under **partial** (Settings → Ranking → `Partial-match tier`), and that there is no OR.

**Fixes**

- "Search (resume last)" always opened the preview, ignoring the `Preview open by default` settings (desktop and mobile); it now follows them like a plain search.

Fast Find 10K is available in Obsidian's Community plugins list; see the README's install section for BRAT pre-releases or building from source.
