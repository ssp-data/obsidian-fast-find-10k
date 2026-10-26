//! Loader for Smart Connections' `.smart-env` embeddings (bge-micro f32 rows + ajson index).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Returns `(dims, paths, data)` with `data.len() == paths.len() * dims`, or `None` when a
/// required file is missing or no vector could be read.
pub fn load(vault: &Path) -> Option<(usize, Vec<String>, Vec<f32>)> {
    let env = vault.join(".smart-env");
    let models = fs::read_to_string(env.join("embedding_models/embedding_models.ajson")).ok()?;
    let dims: usize = number_after(&models, "\"dims\":")?;
    if dims == 0 {
        return None;
    }
    let sources = env.join("smart_sources");
    let mut ajsons: Vec<_> = fs::read_dir(&sources)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "ajson"))
        .collect();
    ajsons.sort();
    // path -> (mf file, row); later lines override earlier ones.
    let mut map: HashMap<String, (String, usize)> = HashMap::new();
    for a in ajsons {
        let text = fs::read_to_string(&a).ok()?;
        for line in text.lines() {
            if let Some((path, file, row)) = parse_line(line) {
                map.insert(path, (file, row));
            }
        }
    }
    let mut entries: Vec<(String, String, usize)> = map.into_iter().map(|(p, (f, i))| (p, f, i)).collect();
    entries.sort();
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    let (mut paths, mut data) = (Vec::new(), Vec::new());
    for (path, file, row) in entries {
        if file.contains('/') || file.contains("..") {
            continue;
        }
        if !files.contains_key(&file) {
            let bytes = fs::read(sources.join(&file)).ok()?;
            files.insert(file.clone(), bytes);
        }
        let bytes = &files[&file];
        let (start, end) = (row * dims * 4, (row + 1) * dims * 4);
        if end > bytes.len() {
            continue;
        }
        data.extend(bytes[start..end].chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
        paths.push(path);
    }
    Some((dims, paths, data))
}

fn number_after(s: &str, key: &str) -> Option<usize> {
    let rest = &s[s.find(key)? + key.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// `"smart_sources:<path>": {... "embedding":{... "file":"mf_x","file_i":N ...} ...}` -> (path, file, N).
fn parse_line(line: &str) -> Option<(String, String, usize)> {
    let rest = line.trim_start().strip_prefix("\"smart_sources:")?;
    let mut path = String::new();
    let mut chars = rest.char_indices();
    let mut end = None;
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, 'n')) => path.push('\n'),
                Some((_, c2)) => path.push(c2),
                None => return None,
            },
            '"' => {
                end = Some(i + 1);
                break;
            }
            c => path.push(c),
        }
    }
    let body = &rest[end?..];
    if !body.trim_start().starts_with(':') {
        return None;
    }
    if !path.ends_with(".md") {
        return None;
    }
    let emb = &body[body.find("\"embedding\"")?..];
    let row = number_after(emb, "\"file_i\":")?;
    let f = &emb[emb.find("\"file\":\"")? + 8..];
    let file = f[..f.find('"')?].to_string();
    Some((path, file, row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_two_rows() {
        let dir = std::env::temp_dir().join(format!("ff10k-smartenv-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let env = dir.join(".smart-env");
        fs::create_dir_all(env.join("embedding_models")).unwrap();
        fs::create_dir_all(env.join("smart_sources")).unwrap();
        fs::write(
            env.join("embedding_models/embedding_models.ajson"),
            "\"embedding_models:x\": {\"model_key\":\"m\",\"dims\":3,\"max_tokens\":512},\n",
        )
        .unwrap();
        fs::write(
            env.join("smart_sources/smart_sources.ajson"),
            "\"smart_sources:a.md\": {\"embedding\":{\"history\":[],\"default\":{\"mf_x\":{\"file\":\"mf_x\",\"file_i\":1,\"at\":5}}},\"path\":\"a.md\"},\n\
             \"smart_sources:b \\\"q\\\".md\": {\"embedding\":{\"default\":{\"mf_x\":{\"file\":\"mf_x\",\"file_i\":0}}}},\n\
             \"smart_sources:img.png\": {\"embedding\":{\"default\":{\"mf_x\":{\"file\":\"mf_x\",\"file_i\":0}}}},\n\
             \"smart_sources:c.md\": {\"class_name\":\"SmartSource\"},\n",
        )
        .unwrap();
        let floats = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_le_bytes()).collect();
        fs::write(env.join("smart_sources/mf_x"), bytes).unwrap();

        let (dims, paths, data) = load(&dir).unwrap();
        assert_eq!(dims, 3);
        assert_eq!(paths, vec!["a.md".to_string(), "b \"q\".md".to_string()]);
        assert_eq!(data, vec![4.0, 5.0, 6.0, 1.0, 2.0, 3.0]);
        assert!(load(&dir.join("missing")).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
