## What has changed?

Search now reaches everything in the vault that carries text: frontmatter properties, Bases, canvases and Excalidraw drawings, with filters to narrow to each kind. The preview renders mermaid diagrams and drawing exports, a ribbon icon opens the search, and on mobile the search can now be closed directly.

**Highlights**

- **Properties are searchable.** Every frontmatter key and value except Obsidian's own (`aliases`, `tags`, `title`, `cssclasses`, `created`) matches in the content tier, shown with a `prop` badge and `Key: value` as context. Filter with Obsidian's syntax: `[Herkunftsland:India]` keeps notes with that value, `[price]` notes that have the key; the `Property (frontmatter)` weight slider tunes it, and 0 turns it off.
- **Bases, canvases and drawings.** `.base`, `.canvas`, `.excalidraw` and `.excalidraw.md` files are indexed with the file name as title: a canvas becomes a small markdown note (group labels as headings, text cards as paragraphs, file cards as links), a drawing contributes its text elements. Results carry a `canvas`, `base` or `drawing` marker after the title, and `type:note`, `type:base`, `type:canvas` or `type:drawing` keeps one kind.
- **Matches in the preview.** The preview header counts the highlighted matches in the note (`3 / 12 matches`); `Alt+N` / `Alt+Shift+N` jump to the next and previous one, `Ctrl+M` (or a click on the header) shows only the lines around the matches.
- **Preview.** Fenced `mermaid` blocks render as diagrams a quarter second after the selection settles; an Excalidraw drawing shows its PNG/SVG export when the plugin wrote one next to it. Everything else stays plain text.
- **Ribbon icon.** A search icon in the left ribbon (the drawer on mobile) opens the search with one click; turn it off under Settings → Behaviour.
- **Mobile close.** A close button at the end of the search line and a swipe to the left close the search.

**Fixes**

- Opening a result in a new tab or split (`Mod+Enter`, `Mod+Alt+Enter`) now keeps focus on the opened note instead of returning it to the note you came from.

Fast Find 10K is available in Obsidian's Community plugins list; see the README's install section for BRAT pre-releases or building from source.
