//! Document model: the fields extracted from one markdown note, and per-field weights.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Field {
    Title,
    Alias,
    H1,
    H2,
    H3,
    Tag,
    Link,
    Path,
    Body,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Weights {
    pub title: f32,
    pub alias: f32,
    pub h1: f32,
    pub h2: f32,
    pub h3: f32,
    pub tag: f32,
    pub link: f32,
    pub path: f32,
    pub body: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            title: 100.0,
            alias: 90.0,
            // H1 is usually the note's own title and gets a hair more than the file name (owner
            // request, 2026-10-27); tiers are unchanged: H1-only matches stay in the headings tier.
            h1: 105.0,
            h2: 65.0,
            h3: 55.0,
            tag: 30.0,
            link: 20.0,
            path: 15.0,
            body: 10.0,
        }
    }
}

impl Weights {
    pub fn get(&self, f: Field) -> f32 {
        match f {
            Field::Title => self.title,
            Field::Alias => self.alias,
            Field::H1 => self.h1,
            Field::H2 => self.h2,
            Field::H3 => self.h3,
            Field::Tag => self.tag,
            Field::Link => self.link,
            Field::Path => self.path,
            Field::Body => self.body,
        }
    }
}

/// An ATX heading (`#`–`###`); `byte_start..byte_end` is its section in the original text.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub line: u32,
    pub byte_start: u32,
    pub byte_end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRef {
    pub target: String,
    pub alias: Option<String>,
    pub heading: Option<String>,
}

/// All string fields are raw display strings; normalization happens at index time.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Doc {
    pub path: String,
    pub title: String,
    pub aliases: Vec<String>,
    pub headings: Vec<Heading>,
    pub tags: Vec<String>,
    pub links: Vec<LinkRef>,
    pub path_segments: Vec<String>,
    pub body: String,
    pub body_line_starts: Vec<u32>,
    /// Source-text line index of each body line (frontmatter and fence markers are not in `body`).
    pub body_line_src: Vec<u32>,
    pub mtime: f64,
    pub size: u32,
    pub word_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_weights_match_spec() {
        let w = Weights::default();
        assert_eq!(w.get(Field::Title), 100.0);
        assert_eq!(w.get(Field::Alias), 90.0);
        assert_eq!(w.get(Field::H1), 105.0);
        assert_eq!(w.get(Field::H2), 65.0);
        assert_eq!(w.get(Field::H3), 55.0);
        assert_eq!(w.get(Field::Tag), 30.0);
        assert_eq!(w.get(Field::Link), 20.0);
        assert_eq!(w.get(Field::Path), 15.0);
        assert_eq!(w.get(Field::Body), 10.0);
    }
}
