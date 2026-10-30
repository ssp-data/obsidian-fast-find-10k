## What has changed?

Search now reaches everything in the vault that carries text: frontmatter properties, Bases, canvases and Excalidraw drawings, with filters to narrow to each. The preview renders mermaid diagrams and drawing exports, and the mobile layout gets a proper way to close.

**Highlights**

- **Properties are searchable.** Every frontmatter key and value (except Obsidian's own `aliases`, `tags`, `title`, `cssclasses`, `created`) matches in the content tier, shown with a `prop` badge and `Key: value` as context. Filter with Obsidian's syntax: `[Herkunftsland:India]`, `[price]`.
- **Bases, canvases and drawings.** `.base`, `.canvas` and `.excalidraw` files are indexed with the file name as title: a canvas becomes a small markdown note (group labels as headings, cards as text, file cards as links), a drawing contributes its text elements. Results carry a small `canvas` / `base` / `drawing` marker; `type:base`, `type:canvas`, `type:drawing`, `type:note` keep one kind.
- **Preview.** Fenced `mermaid` blocks render as diagrams a quarter second after the selection settles; an Excalidraw PNG/SVG export shows above the drawing's text when the plugin wrote one.
- **Mobile.** A ✕ at the end of the search line and a swipe to the left close the search.

**Fixes**

- Opening a result in a new tab or split (`Ctrl+Enter`, `Ctrl+Alt+Enter`) now keeps focus on the opened note instead of jumping back to the one you came from.
