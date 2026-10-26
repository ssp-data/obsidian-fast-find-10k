//! Markdown field extraction: a hand-written line scanner turning a raw note into a `Doc`.

use crate::doc::{Doc, Heading, LinkRef};
use crate::tokenize::tokenize;

pub fn extract(path: &str, text: &str, mtime: f64) -> Doc {
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    };
    let title = file.strip_suffix(".md").unwrap_or(file).to_string();
    let path_segments = dir
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    // Lines of the original text with their byte offsets (`\r` stripped from content).
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut off = 0;
    for l in text.split('\n') {
        lines.push((off, l.strip_suffix('\r').unwrap_or(l)));
        off += l.len() + 1;
    }

    let mut aliases = Vec::new();
    let mut tags = Vec::new();
    let mut first_body_line = 0;
    if let Some(end) = frontmatter_end(&lines) {
        parse_frontmatter(&lines[1..end], &mut aliases, &mut tags);
        first_body_line = end + 1;
    }

    let mut sc = Scanner::default();
    let mut body_lines: Vec<String> = Vec::new();
    let mut headings: Vec<Heading> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for (idx, &(start, line)) in lines.iter().enumerate().skip(first_body_line) {
        let trimmed = line.trim_start();
        if let Some((fc, flen)) = fence_marker(trimmed) {
            match fence {
                None => {
                    fence = Some((fc, flen));
                    continue;
                }
                Some((c, n))
                    if c == fc && flen >= n && trimmed.trim_end().chars().all(|x| x == c) =>
                {
                    fence = None;
                    continue;
                }
                _ => {}
            }
        }
        if fence.is_some() {
            body_lines.push(line.to_string());
            continue;
        }
        let content = match atx_heading(line) {
            Some((level, htext)) => {
                if level <= 3 {
                    headings.push(Heading {
                        level,
                        text: htext.to_string(),
                        line: idx as u32,
                        byte_start: start as u32,
                        byte_end: text.len() as u32,
                    });
                }
                htext
            }
            None => line,
        };
        body_lines.push(sc.inline(content));
    }

    // Section end: start of the next heading of level <= own.
    for i in 0..headings.len() {
        if let Some(next) = headings[i + 1..]
            .iter()
            .find(|h| h.level <= headings[i].level)
        {
            headings[i].byte_end = next.byte_start;
        }
    }

    for t in sc.tags {
        add_tag(&mut tags, &t);
    }

    let mut body_line_starts = Vec::with_capacity(body_lines.len());
    let mut acc = 0u32;
    for l in &body_lines {
        body_line_starts.push(acc);
        acc += l.len() as u32 + 1;
    }
    let body = body_lines.join("\n");
    let word_count = tokenize(&body).len() as u32;

    Doc {
        path: path.to_string(),
        title,
        aliases,
        headings,
        tags,
        links: sc.links,
        path_segments,
        body,
        body_line_starts,
        mtime,
        size: text.len() as u32,
        word_count,
    }
}

/// Index of the closing `---`/`...` line, if the text opens with a `---` line.
fn frontmatter_end(lines: &[(usize, &str)]) -> Option<usize> {
    if lines.first()?.1.trim_end() != "---" {
        return None;
    }
    lines
        .iter()
        .skip(1)
        .position(|(_, l)| matches!(l.trim_end(), "---" | "..."))
        .map(|p| p + 1)
}

fn parse_frontmatter(lines: &[(usize, &str)], aliases: &mut Vec<String>, tags: &mut Vec<String>) {
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].1;
        i += 1;
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let is_alias = matches!(key.trim(), "aliases" | "alias");
        let is_tag = matches!(key.trim(), "tags" | "tag");
        if !is_alias && !is_tag {
            continue;
        }
        let value = value.trim();
        let mut items: Vec<String> = Vec::new();
        if value.is_empty() {
            while i < lines.len() {
                let t = lines[i].1.trim();
                if let Some(item) = t.strip_prefix('-') {
                    items.push(unquote(item.trim()).to_string());
                } else if !t.is_empty() {
                    break;
                }
                i += 1;
            }
        } else if let Some(inner) = value.strip_prefix('[') {
            let inner = inner.strip_suffix(']').unwrap_or(inner);
            items.extend(inner.split(',').map(|s| unquote(s.trim()).to_string()));
        } else if is_tag {
            items.extend(
                unquote(value)
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .map(String::from),
            );
        } else {
            items.push(unquote(value).to_string());
        }
        for it in items {
            if is_tag {
                add_tag(tags, &it);
            } else if !it.is_empty() && !aliases.contains(&it) {
                aliases.push(it);
            }
        }
    }
}

fn unquote(s: &str) -> &str {
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Add a tag (leading `#` stripped): the full path, each prefix path (longest first), each segment.
fn add_tag(tags: &mut Vec<String>, raw: &str) {
    let t = raw.trim().trim_start_matches('#').trim_matches('/');
    if t.is_empty() {
        return;
    }
    let mut push = |s: &str| {
        if !s.is_empty() && !tags.iter().any(|x| x == s) {
            tags.push(s.to_string());
        }
    };
    push(t);
    let cuts: Vec<usize> = t.match_indices('/').map(|(i, _)| i).collect();
    for &c in cuts.iter().rev() {
        push(&t[..c]);
    }
    if !cuts.is_empty() {
        for seg in t.split('/') {
            push(seg);
        }
    }
}

/// A code fence opener/closer: 3+ backticks or tildes.
fn fence_marker(trimmed: &str) -> Option<(char, usize)> {
    let c = trimmed.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = trimmed.chars().take_while(|&x| x == c).count();
    // A backtick fence's info string cannot contain a backtick (CommonMark).
    if c == '`' && trimmed[n..].contains('`') {
        return None;
    }
    (n >= 3).then_some((c, n))
}

/// ATX heading: level and text with markers and trailing `#`s trimmed.
fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let n = line.bytes().take_while(|&b| b == b'#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &line[n..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim_end();
    Some((n as u8, text))
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// Inline-level scanner: rewrites a line to body text, collecting links and inline tags.
#[derive(Default)]
struct Scanner {
    links: Vec<LinkRef>,
    tags: Vec<String>,
}

impl Scanner {
    fn inline(&mut self, line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut close_wiki = Finder::new("]]");
        let mut md = MdLinkFinder::default();
        let mut ticks_memo: Vec<Finder> = Vec::new();
        let mut i = 0;
        while i < line.len() {
            let rest = &line[i..];
            let prev = line[..i].chars().next_back();
            let at_boundary = prev.is_none_or(char::is_whitespace);

            if rest.starts_with('`') {
                let n = rest.bytes().take_while(|&b| b == b'`').count();
                let ticks = &rest[..n];
                while ticks_memo.len() < n {
                    ticks_memo.push(Finder::new(""));
                }
                let memo = &mut ticks_memo[n - 1];
                memo.pat = ticks;
                if let Some(close) = memo.find(line, i + n) {
                    out.push_str(&line[i + n..close]);
                    i = close + n;
                } else {
                    out.push_str(ticks);
                    i += n;
                }
                continue;
            }
            if rest.starts_with("![[") {
                if let Some(close) = close_wiki.find(line, i + 3) {
                    let inner = &line[i + 3..close];
                    if is_note_target(inner) {
                        self.wikilink(inner, &mut out);
                    }
                    i = close + 2;
                    continue;
                }
            }
            if rest.starts_with("[[") {
                if let Some(close) = close_wiki.find(line, i + 2) {
                    self.wikilink(&line[i + 2..close], &mut out);
                    i = close + 2;
                    continue;
                }
            }
            if rest.starts_with("![") {
                if let Some((_, end)) = md.find(line, i + 2) {
                    i = end;
                    continue;
                }
            }
            if rest.starts_with('[') {
                if let Some((mid, end)) = md.find(line, i + 1) {
                    out.push_str(&line[i + 1..mid]);
                    i = end;
                    continue;
                }
            }
            if rest.starts_with("data:")
                && prev.is_none_or(|c| !c.is_alphanumeric())
                && is_data_uri(&rest[5..])
            {
                let n = rest
                    .find(|c: char| c.is_whitespace() || c == ')')
                    .unwrap_or(rest.len());
                i += n;
                continue;
            }
            if at_boundary && rest.starts_with('#') {
                let tag_len = rest[1..]
                    .find(|c: char| !is_tag_char(c))
                    .unwrap_or(rest.len() - 1);
                let tag = &rest[1..1 + tag_len];
                if !tag.is_empty() && !tag.chars().all(|c| c.is_ascii_digit()) {
                    self.tags.push(tag.to_string());
                }
            }
            let c = rest.chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
        out
    }

    /// Record `Target#Heading|alias` and write its readable text.
    fn wikilink(&mut self, inner: &str, out: &mut String) {
        let (dest, alias) = match inner.split_once('|') {
            Some((d, a)) => (d, Some(a.trim())),
            None => (inner, None),
        };
        let (target, heading) = match dest.split_once('#') {
            Some((t, h)) => (t.trim(), Some(h.trim())),
            None => (dest.trim(), None),
        };
        let heading = heading.filter(|h| !h.is_empty() && !h.starts_with('^'));
        let alias = alias.filter(|a| !a.is_empty());
        let parts: Vec<&str> = [Some(target), heading, alias]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
        out.push_str(&parts.join(" "));
        if !target.is_empty() {
            self.links.push(LinkRef {
                target: target.to_string(),
                alias: alias.map(String::from),
                heading: heading.map(String::from),
            });
        }
    }
}

/// Embeds count as links only for notes: no extension, or `.md`.
fn is_note_target(inner: &str) -> bool {
    let target = inner.split(['|', '#']).next().unwrap_or("").trim();
    let file = target.rsplit('/').next().unwrap_or(target);
    match file.rfind('.') {
        Some(dot) => file[dot + 1..].eq_ignore_ascii_case("md"),
        None => true,
    }
}

/// Memoized "next occurrence of `pat` at or after `q`" within one line.
/// Keeps repeated searches linear: a cached hit stays valid while it is >= q,
/// and a cached miss stays valid for every later q.
struct Finder<'p> {
    pat: &'p str,
    from: usize,
    hit: Option<usize>,
    set: bool,
}

impl<'p> Finder<'p> {
    fn new(pat: &'p str) -> Self {
        Finder {
            pat,
            from: 0,
            hit: None,
            set: false,
        }
    }

    fn find(&mut self, hay: &str, q: usize) -> Option<usize> {
        if self.set && self.from <= q && self.hit.is_none_or(|p| p >= q) {
            return self.hit;
        }
        self.from = q;
        self.hit = hay[q..].find(self.pat).map(|p| p + q);
        self.set = true;
        self.hit
    }
}

/// Finds markdown links `text](url)` after an opening `[`, with memoized searches.
struct MdLinkFinder {
    mid: Finder<'static>,
    open: Finder<'static>,
    close: Finder<'static>,
}

impl Default for MdLinkFinder {
    fn default() -> Self {
        MdLinkFinder {
            mid: Finder::new("]("),
            open: Finder::new("["),
            close: Finder::new(")"),
        }
    }
}

impl MdLinkFinder {
    /// For link text starting at `q`: absolute `(text_end, link_end)`.
    fn find(&mut self, line: &str, q: usize) -> Option<(usize, usize)> {
        let mid = self.mid.find(line, q)?;
        if self.open.find(line, q).is_some_and(|p| p < mid) {
            return None;
        }
        let close = self.close.find(line, mid + 2)?;
        Some((mid, close + 1))
    }
}

/// After `data:`: a mime type `type/subtype` or a bare `;base64`.
fn is_data_uri(after: &str) -> bool {
    if after.starts_with(";base64") {
        return true;
    }
    let is_mime_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-');
    let Some((ty, rest)) = after.split_once('/') else {
        return false;
    };
    let sub_len = rest.find(|c: char| !is_mime_char(c)).unwrap_or(rest.len());
    !ty.is_empty() && ty.chars().all(is_mime_char) && sub_len > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(target: &str, alias: Option<&str>, heading: Option<&str>) -> LinkRef {
        LinkRef {
            target: target.into(),
            alias: alias.map(Into::into),
            heading: heading.map(Into::into),
        }
    }

    const FM: &str = "---\naliases:\n  - AI DE\n  - LLM pipelines\ntags: [ai, de/ml]\n---\n# x";

    #[test]
    fn title_and_path_segments() {
        let d = extract(
            "💡 Resources/🧮 Zettelkasten/AI in Data Engineering.md",
            "",
            0.0,
        );
        assert_eq!(d.title, "AI in Data Engineering");
        assert_eq!(d.path_segments, vec!["💡 Resources", "🧮 Zettelkasten"]);
    }

    #[test]
    fn frontmatter_aliases_list_and_scalar() {
        let d = extract("n.md", FM, 0.0);
        assert_eq!(d.aliases, vec!["AI DE", "LLM pipelines"]);
        assert_eq!(d.tags, vec!["ai", "de/ml", "de", "ml"]);
        let d = extract("n.md", "---\naliases: Solo\n---\nbody", 0.0);
        assert_eq!(d.aliases, vec!["Solo"]);
    }

    #[test]
    fn frontmatter_removed_from_body() {
        let d = extract("n.md", FM, 0.0);
        assert!(!d.body.contains("aliases"), "body: {:?}", d.body);
    }

    #[test]
    fn unterminated_frontmatter_keeps_body() {
        let d = extract("n.md", "---\ntitle: x\nno end\n\nreal body here", 0.0);
        assert!(d.body.contains("real body here"));
        assert!(d.aliases.is_empty());
    }

    #[test]
    fn headings_levels_and_sections() {
        let text = "# One\ntext\n## Two\nmore\n#### Four\n";
        let d = extract("n.md", text, 0.0);
        let hs: Vec<(u8, &str, u32)> = d
            .headings
            .iter()
            .map(|h| (h.level, h.text.as_str(), h.line))
            .collect();
        assert_eq!(hs, vec![(1, "One", 0), (2, "Two", 2)]);
        assert!(d.body.contains("Four"));
        let two = &d.headings[1];
        assert_eq!(
            &text[two.byte_start as usize..two.byte_end as usize],
            "## Two\nmore\n#### Four\n"
        );
    }

    #[test]
    fn inline_tags_and_nested() {
        let d = extract("n.md", "see #data/eng and #ai.", 0.0);
        for t in ["data/eng", "data", "eng", "ai"] {
            assert!(d.tags.iter().any(|x| x == t), "missing {t}: {:?}", d.tags);
        }
        assert!(!d.tags.iter().any(|x| x == "ai."));
    }

    #[test]
    fn wikilinks_variants() {
        let d = extract(
            "n.md",
            "[[Data Mesh]] [[Data Mesh|mesh]] [[Note#Head]] ![[img.png]]",
            0.0,
        );
        assert_eq!(
            d.links,
            vec![
                link("Data Mesh", None, None),
                link("Data Mesh", Some("mesh"), None),
                link("Note", None, Some("Head")),
            ]
        );
        assert!(d.body.contains("Data Mesh mesh Note"), "body: {:?}", d.body);
        assert!(!d.body.contains("[["));
        assert!(!d.body.contains("img"));
    }

    #[test]
    fn markdown_links_and_data_uris() {
        let d = extract(
            "n.md",
            "[text](http://x) ![](data:image/png;base64,AAAA)",
            0.0,
        );
        assert!(d.body.contains("text"));
        assert!(!d.body.contains("base64"));
    }

    #[test]
    fn code_fences_kept() {
        let d = extract(
            "n.md",
            "before\n```rust\nlet x = foo_bar(); # not a tag [[nolink]]\n```\nafter",
            0.0,
        );
        assert!(d.body.contains("let x = foo_bar();"));
        assert!(!d.body.contains("```"));
        assert!(d.tags.is_empty());
        assert!(d.links.is_empty());
    }

    #[test]
    fn word_count_and_lines() {
        let d = extract("n.md", "alpha beta\ngamma\ndelta epsilon", 0.0);
        assert_eq!(d.body_line_starts.len(), d.body.split('\n').count());
        assert_eq!(d.body_line_starts, vec![0, 11, 17]);
        assert_eq!(d.word_count, 5);
        assert_eq!(d.size, 30);
    }

    #[test]
    fn link_variants_block_and_note_embed() {
        let d = extract(
            "n.md",
            "[[T#^blk]] [[T#H|a]] ![[Other]] ![[Doc.md]] ![[f.pdf]]",
            0.0,
        );
        assert_eq!(
            d.links,
            vec![
                link("T", None, None),
                link("T", Some("a"), Some("H")),
                link("Other", None, None),
                link("Doc.md", None, None),
            ]
        );
    }

    #[test]
    fn not_tags() {
        let d = extract(
            "n.md",
            "# Head\nissue #1 and `#code` and a#b\n### H3 ###",
            0.0,
        );
        assert!(d.tags.is_empty(), "{:?}", d.tags);
        assert_eq!(d.headings[1].text, "H3");
    }

    #[test]
    fn frontmatter_tag_variants() {
        let d = extract(
            "n.md",
            "---\ntags:\n  - \"#x\"\n  - y/z\ntag: w\nother: [q]\n---\nb #x",
            0.0,
        );
        assert_eq!(d.tags, vec!["x", "y/z", "y", "z", "w"]);
    }

    #[test]
    fn heading_inside_fence_is_not_heading() {
        let d = extract("n.md", "```\n# not\n```\n# yes", 0.0);
        assert_eq!(d.headings.len(), 1);
        assert_eq!(d.headings[0].text, "yes");
        assert_eq!(d.headings[0].line, 3);
    }

    #[test]
    fn long_bracket_lines_are_linear() {
        // The scanner itself gets the tight budget; the end-to-end bound is looser because
        // the (linear) tokenizer alone needs ~100 ms per 200k chars in unoptimized builds.
        // The former quadratic scan took ~39 s on the first input.
        let inputs = [
            "[".repeat(200_000),
            "[x] ".repeat(20_000),
            "[[".repeat(100_000),
            "](".repeat(100_000),
            "` `` ".repeat(40_000),
        ];
        for line in inputs {
            let t = std::time::Instant::now();
            Scanner::default().inline(&line);
            let ms = t.elapsed().as_millis();
            assert!(ms < 100, "scanner took {ms} ms on {:?}...", &line[..5]);
            let t = std::time::Instant::now();
            extract("n.md", &line, 0.0);
            let ms = t.elapsed().as_millis();
            assert!(ms < 1000, "extract took {ms} ms on {:?}...", &line[..5]);
        }
    }

    #[test]
    fn inline_triple_backticks_do_not_open_fence() {
        let d = extract("n.md", "```inline```\n# Later\n#tag [[L]]", 0.0);
        assert_eq!(d.headings.len(), 1);
        assert_eq!(d.tags, vec!["tag"]);
        assert_eq!(d.links, vec![link("L", None, None)]);
    }

    #[test]
    fn prose_data_colon_kept() {
        let d = extract("n.md", "Big data: the new oil", 0.0);
        assert!(d.body.contains("data"), "body: {:?}", d.body);
        let d = extract(
            "n.md",
            "x data:image/png;base64,AAAA y data:;base64,BBBB z",
            0.0,
        );
        assert!(
            !d.body.contains("AAAA") && !d.body.contains("BBBB"),
            "body: {:?}",
            d.body
        );
        assert!(d.body.contains('z'));
    }
}
