## What has changed?

Search now reaches everything in the vault that carries text: frontmatter properties, Bases, canvases and Excalidraw drawings, each with a filter to narrow to it. The preview counts and jumps between matches, renders mermaid diagrams and drawing exports, and navigation keys moved to Alt so capital letters can be typed again.

**Highlights**

- **Properties are searchable.** Every frontmatter key and value except Obsidian's own (`aliases`, `tags`, `title`, `cssclasses`, `created`) matches in the content tier, shown with a `prop` badge and `Key: value` as context. Filter with Obsidian's syntax: `[Herkunftsland:India]` keeps notes with that value, `[price]` notes that have the key; the `Property (frontmatter)` weight slider tunes it, and 0 turns it off. (#4)

<img width="800" alt="Image" src="https://github.com/user-attachments/assets/a7870a63-21e1-4d16-a3b4-7ab314d220ca" />

- **Bases, canvases and drawings.** `.base`, `.canvas`, `.excalidraw` and `.excalidraw.md` files are indexed with the file name as title: a canvas becomes a small markdown note (group labels as headings, text cards as paragraphs, file cards as links), a drawing contributes its text elements. Results carry a `canvas`, `base` or `drawing` marker, and `type:note`, `type:base`, `type:canvas` or `type:drawing` keeps one kind. (#1)

<img width="800" alt="Image" src="https://github.com/user-attachments/assets/58e575b3-d0d0-4adf-8128-40070d7916c5" />

- **Matches in the preview.** The preview header counts the highlighted matches (`3 / 12 matches`); `Alt+N` / `Alt+P` (or `Alt+Shift+N`) jump to the next and previous one, and `Ctrl+M` (or a click on the header, the `matches` pill on mobile) shows only the lines around the matches, with line numbers. (#3)

<img width="800" alt="Image" src="https://github.com/user-attachments/assets/4774eb2a-58d4-4d4e-9d0c-12f431eaae99" />

- **Diagrams and drawing exports in the preview.** Fenced `mermaid` blocks render as diagrams a quarter second after the selection settles; an Excalidraw drawing shows its PNG/SVG export when the Excalidraw plugin wrote one next to it. Everything else stays plain text. (#2)

<img width="800" alt="Image" src="https://github.com/user-attachments/assets/2d3500e9-a420-47b3-9edd-863eb29c0ca7" />

- **Alt for navigation.** Preview scrolling is `Alt+J` / `Alt+K` and the similar-notes panel `Alt+L` / `Alt+H` (was Shift), so every capital letter stays typable, for example in a note name created with `Shift+Enter`. Saved bindings on the old keys move over automatically, and a binding can list alternatives (`Alt+Shift+N | Alt+P`).
- **Ribbon icon.** A search icon in the left ribbon (the drawer on mobile) opens the search with one click; turn it off with `Ribbon icon` under Settings → Behaviour. (#7)

<img width="800" alt="Image" src="https://github.com/user-attachments/assets/e4d76ec1-fff1-4fd3-8c1e-6fb1ae007e9a" />

**Fixes**

- On mobile the full-screen search could only be left by opening a note; a ✕ at the end of the search line and a swipe to the left now close it. (#8)

<img width="370" alt="Image" src="https://github.com/user-attachments/assets/06164e60-dd47-48bb-a6df-9d74ef6e6dde" />

- "Search (resume last)" always opened the preview; it now follows the "Preview open by default" settings like a plain search. On mobile the preview is now on by default (the `preview` pill hides it).
- On mobile the preview stayed empty until a result was tapped; it now shows the first result as on desktop (the same bug hid the preview on desktop with key hints set to never).
- Opening a result in a new tab or split (`Mod+Enter`, `Mod+Alt+Enter`) now keeps focus on the opened note instead of returning it to the note you came from.

Fast Find 10K is available in Obsidian's Community plugins list; see the README's install section for BRAT pre-releases or building from source.
