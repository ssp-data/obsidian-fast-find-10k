## What has changed?

A visual release: search and ranking are unchanged from 0.2.3. The modal now takes its colours from your theme and adjusts them until every hit, badge and heading reads clearly, in light and dark mode alike, and the selected row is easy to spot.

**Highlights**

- **Readable on every theme.** When the search opens (and whenever the theme changes), each theme colour the modal uses is measured against the modal's backgrounds and blended toward the text colour only as far as needed to reach 4.5:1 contrast. Checked on Kanagawa, Asia Neon, Fireplace, Osaka Jade, Omarchy and the default theme, both modes.
- **A selection you can see.** The selected result, and the selected row in the similar-notes panel, now carry a bar on the left plus a light tint of the theme's selection colour instead of the theme's hover shade.
- **Badges and heading context in theme colours.** The `title` badge uses the accent, the `heading` badge and the heading shown under a row use the theme's H2 colour, the `semantic` badge is outlined in the H3 colour. Other badges stay quiet.
- **Stronger preview highlights.** Matches in the preview are bold and underlined, every line with a match gets a faint band across the full width with a rule on the right edge, and the current match (Alt+N / Alt+P) is a solid accent block.
- **Quieter pills.** The active pill (all / files only / +languages / preview) is tinted and ringed with the selection colour and keeps normal text; inactive pills are outlined. The `N similar` pill and the panel's "similar notes" label drop the red for faint text and a plain border.

**Fixes**

- Pale light themes such as Osaka Jade light showed title hits and badges in colours that barely stood out from the background; they are now darkened enough to read.
- In some dark themes (Kanagawa among them) the selected row was almost indistinguishable from its neighbours, and the active pill's text could vanish against its background; both are visible now.

**Before and after** (0.2.3 above, 0.2.4 below; more themes in [docs/showcase.md](https://github.com/ssp-data/obsidian-fast-find-10k/blob/main/docs/showcase.md#theme-colours-024))

![Kanagawa dark, before and after](https://raw.githubusercontent.com/ssp-data/obsidian-fast-find-10k/main/docs/images/themes/kanagawa-dark.webp)

![Asia Neon light, before and after](https://raw.githubusercontent.com/ssp-data/obsidian-fast-find-10k/main/docs/images/themes/asia-neon-light.webp)

![Osaka Jade light, before and after: the pale accent is darkened until it reads](https://raw.githubusercontent.com/ssp-data/obsidian-fast-find-10k/main/docs/images/themes/osaka-jade-light.webp)
