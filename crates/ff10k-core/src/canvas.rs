//! Obsidian Canvas (`.canvas`, JSON Canvas 1.0) as a note: the canvas is turned into a small
//! markdown document (group labels as headings, text nodes as paragraphs, file nodes as links or
//! image embeds, link nodes as URLs, edge labels as text) and indexed like any note. The same
//! text is what the preview shows, so highlights and image embeds work without rendering.

use serde_json::Value;

const IMAGE_EXT: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "svg", "avif"];

pub fn is_canvas(path: &str) -> bool {
    path.ends_with(".canvas")
}

fn is_image(file: &str) -> bool {
    file.rsplit('.').next().is_some_and(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

/// The markdown form of a canvas; the raw text when it is not valid JSON Canvas.
pub fn canvas_to_markdown(json: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return json.to_string() };
    let mut out: Vec<String> = Vec::new();
    let str_of = |n: &Value, k: &str| n.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    for n in v.get("nodes").and_then(Value::as_array).into_iter().flatten() {
        match n.get("type").and_then(Value::as_str) {
            Some("group") => {
                if let Some(label) = str_of(n, "label") {
                    out.push(format!("## {label}"));
                }
            }
            Some("text") => {
                if let Some(text) = str_of(n, "text") {
                    out.push(text);
                }
            }
            Some("file") => {
                if let Some(file) = str_of(n, "file") {
                    let target = match str_of(n, "subpath") {
                        Some(sub) => format!("{file}{sub}"),
                        None => file.clone(),
                    };
                    out.push(if is_image(&file) { format!("![[{target}]]") } else { format!("[[{target}]]") });
                }
            }
            Some("link") => {
                if let Some(url) = str_of(n, "url") {
                    out.push(url);
                }
            }
            _ => {}
        }
    }
    for e in v.get("edges").and_then(Value::as_array).into_iter().flatten() {
        if let Some(label) = str_of(e, "label") {
            out.push(label);
        }
    }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_and_edges_become_markdown() {
        let json = r##"{"nodes":[
            {"id":"a","type":"group","label":"Brainstorming","x":0,"y":0,"width":1,"height":1},
            {"id":"b","type":"text","text":"Write the **book** #draft","x":0,"y":0,"width":1,"height":1},
            {"id":"c","type":"file","file":"Notes/Outline.md","x":0,"y":0,"width":1,"height":1},
            {"id":"d","type":"file","file":"img/cover.PNG","subpath":"","x":0,"y":0,"width":1,"height":1},
            {"id":"e","type":"file","file":"Notes/Plan.md","subpath":"#Chapter 2","x":0,"y":0,"width":1,"height":1},
            {"id":"f","type":"link","url":"https://example.com/x","x":0,"y":0,"width":1,"height":1},
            {"id":"g","type":"text","text":"   ","x":0,"y":0,"width":1,"height":1}
          ],"edges":[{"id":"e1","fromNode":"b","toNode":"c","label":"leads to"},{"id":"e2","fromNode":"c","toNode":"d"}]}"##;
        let md = canvas_to_markdown(json);
        assert_eq!(
            md,
            "## Brainstorming\n\nWrite the **book** #draft\n\n[[Notes/Outline.md]]\n\n![[img/cover.PNG]]\n\n[[Notes/Plan.md#Chapter 2]]\n\nhttps://example.com/x\n\nleads to"
        );
        assert_eq!(canvas_to_markdown("not json"), "not json");
        assert_eq!(canvas_to_markdown("{}"), "");
        assert!(is_canvas("a/b.canvas") && !is_canvas("a/b.md"));
    }
}
