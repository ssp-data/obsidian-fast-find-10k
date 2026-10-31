## What has changed?

A maintenance release with no user-visible change: search, ranking, keys and settings are the same as in 0.2.4, and the modal looks the same on every theme. The plugin's stylesheet and colour code were reworked to pass the Obsidian community review, which is what stands between the plugin and a listing in Community plugins.

- **Ready for the Obsidian plugin review.** The reviewer's automated findings are cleared: colours are written through Obsidian's style helpers instead of inline styles, preview lines with a match are marked with a class rather than a `:has()` selector, and the mobile pill row no longer sets a scrollbar property. None of this changes what you see; it only changes how the stylesheet and colour code are expressed.

**Fixes**

- No user-facing bugs were fixed in this release. The theme-colour contrast pass from 0.2.4 is unchanged.

Install from the release assets or build with `make build` as described in the README.
