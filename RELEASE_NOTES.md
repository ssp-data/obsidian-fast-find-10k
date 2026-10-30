## What has changed?

A small layout release: search and ranking are unchanged from 0.2.1. The preview's match counter now floats over the text instead of taking a line of its own, so the first lines of a note stay visible. The difference is most noticeable on mobile.

**Highlights**

- **Match counter as a floating chip.** The `3 / 12 matches` count and the matches-only label (`Ctrl+M`) now sit in a translucent chip in the preview's top-right corner. They no longer push the note text down, and clicking the chip still switches between matches only and the whole note.

**Fixes**

- On mobile, the preview header with the match count hid the first lines of the selected note. The label now takes no space, so the note starts at its first line.
