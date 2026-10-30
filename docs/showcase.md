# Showcase

What the features look like, one screenshot each. The README keeps the text; this page keeps the pictures.
Images are the ones posted on the GitHub issues that asked for the feature, so they stay out of the repository.

## Matches in the preview

The preview header counts the highlighted matches in the note. `Alt+N` / `Alt+P` jump between them, `Ctrl+M` shows only the lines around each match, with line numbers. Asked in [#3](https://github.com/ssp-data/obsidian-fast-find-10k/issues/3) and on Reddit.

<img alt="7 matches counted in the preview header" src="https://github.com/user-attachments/assets/4774eb2a-58d4-4d4e-9d0c-12f431eaae99" />

<img alt="Alt+N steps through the matches: 3 / 7" src="https://github.com/user-attachments/assets/0c2ed4f9-70af-4523-b921-6801e5a199ef" />

<img alt="Ctrl+M: matches only, with line numbers and gaps" src="https://github.com/user-attachments/assets/c67001cc-905e-47a8-8281-7aa75d746484" />

## Properties

Every frontmatter key and value is searchable. `[key]` lists the notes that have the property, `[key:value]` the ones with that value. Asked in [#4](https://github.com/ssp-data/obsidian-fast-find-10k/issues/4) and on Reddit.

<img alt="[herkunftsland] lists every note with the property" src="https://github.com/user-attachments/assets/a7870a63-21e1-4d16-a3b4-7ab314d220ca" />

<img alt="[herkunftsland:india] keeps the notes whose value matches" src="https://github.com/user-attachments/assets/d6fb5b40-23b5-4fb7-a841-29f5a2bf8acb" />

## Canvases and Bases

`.canvas` and `.base` files are indexed with the file name as title; a canvas is previewed as its cards and labels. `type:canvas` / `type:base` keep one kind, the small marker on the row shows which. Asked in [#1](https://github.com/ssp-data/obsidian-fast-find-10k/issues/1).

<img alt="type:canvas lists the canvases, previewed as text" src="https://github.com/user-attachments/assets/58e575b3-d0d0-4adf-8128-40070d7916c5" />

## Excalidraw drawings

A drawing contributes its text elements; `type:drawing` keeps only drawings. Asked in [#2](https://github.com/ssp-data/obsidian-fast-find-10k/issues/2).

<img alt="type:drawings with the drawing marker" src="https://github.com/user-attachments/assets/2d3500e9-a420-47b3-9edd-863eb29c0ca7" />

## Ribbon icon

One click in the left ribbon (the drawer on mobile) opens the search; Settings → Behaviour turns it off. Asked in [#7](https://github.com/ssp-data/obsidian-fast-find-10k/issues/7) and on Reddit.

<img alt="The ribbon icon in the left bar" src="https://github.com/user-attachments/assets/e4d76ec1-fff1-4fd3-8c1e-6fb1ae007e9a" />

<img alt="The Ribbon icon setting" src="https://github.com/user-attachments/assets/9767ac90-8217-4a2a-84a4-f7f2b3c56c42" />

## Mobile

One column, pills instead of key hints, a ✕ to close (swipe left works too), two taps to open when the preview is on. The close button came from [#8](https://github.com/ssp-data/obsidian-fast-find-10k/issues/8).

<img alt="Mobile layout with the close button" src="https://github.com/user-attachments/assets/06164e60-dd47-48bb-a6df-9d74ef6e6dde" width="360" />

![Mobile preview](images/ff10k-mobile-preview.png)

## Theme colours (0.2.4)

The modal takes its colours from the theme and, when it opens, blends each one toward the theme's text colour only as far as it needs to read at 4.5:1. The selected row gets a bar and a tint, the `N similar` pills go quiet, matches in the preview are bold and underlined with a band across their line. Each image shows 0.2.3 above and 0.2.4 below, on the test vault with `pipeline` typed.

![Kanagawa dark, before and after](images/themes/kanagawa-dark.webp)

![Asia Neon dark, before and after](images/themes/asia-neon-dark.webp)

![Asia Neon light, before and after](images/themes/asia-neon-light.webp)

![Fireplace dark, before and after](images/themes/fireplace-dark.webp)

![Fireplace light, before and after](images/themes/fireplace-light.webp)

![Osaka Jade light, before and after: the pale accent is darkened until it reads](images/themes/osaka-jade-light.webp)

## Why ranked

`Ctrl+R` shows the tier, the matched fields and every score part of each row.

![why ranked](images/why-ranked.webp)
