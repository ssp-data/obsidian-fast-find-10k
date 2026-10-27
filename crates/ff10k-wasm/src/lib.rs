//! wasm-bindgen wrapper over `ff10k_core::Engine`. Structured data crosses the boundary as JSON strings.
//!
//! Error convention: methods returning `Result` throw a plain string; `search` returns
//! `{"error": ...}` JSON because it must always return a String.

use ff10k_core::engine::Engine;
use ff10k_core::query::Query;
use ff10k_core::rank::{Config, Ghost, SearchOptions};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// The combo key of a raw query: `Query::parse(raw).normalized` (terms only, no filters).
#[wasm_bindgen]
pub fn normalize_query(raw: &str) -> String {
    Query::parse(raw).normalized
}

#[derive(Deserialize)]
struct Doc {
    path: String,
    mtime: f64,
    text: String,
}

fn err_json(e: impl std::fmt::Display) -> String {
    serde_json::json!({ "error": e.to_string() }).to_string()
}

fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(err_json)
}

fn js_err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[wasm_bindgen]
pub struct WasmEngine {
    inner: Engine,
}

#[wasm_bindgen]
impl WasmEngine {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmEngine {
        console_error_panic_hook::set_once();
        WasmEngine { inner: Engine::new() }
    }

    pub fn set_config(&mut self, json: &str) -> Result<(), JsValue> {
        let c: Config = serde_json::from_str(json).map_err(js_err)?;
        self.inner.set_config(c);
        Ok(())
    }

    pub fn upsert(&mut self, path: &str, mtime: f64, text: &str) {
        self.inner.upsert(path, mtime, text);
    }

    /// `[{path, mtime, text}]`; returns the number of docs applied; throws on invalid JSON.
    pub fn upsert_batch(&mut self, json: &str) -> Result<u32, JsValue> {
        let docs: Vec<Doc> = serde_json::from_str(json).map_err(js_err)?;
        for d in &docs {
            self.inner.upsert(&d.path, d.mtime, &d.text);
        }
        Ok(docs.len() as u32)
    }

    /// Drops every indexed note (rebuild); frecency, combos, ghosts and vectors stay.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Builds the lazy query caches (call when idle after mutations).
    pub fn warm_up(&self) {
        self.inner.warm_up();
    }

    pub fn remove(&mut self, path: &str) -> bool {
        self.inner.remove(path)
    }

    pub fn rename(&mut self, old: &str, new: &str) -> bool {
        self.inner.rename(old, new)
    }

    pub fn doc_count(&self) -> u32 {
        self.inner.doc_count() as u32
    }

    pub fn doc_text(&self, path: &str) -> Option<String> {
        self.inner.doc_text(path).map(str::to_owned)
    }

    /// Returns SearchResponse JSON, or `{"error": "..."}` for invalid options.
    pub fn search(&self, query: &str, opts_json: &str) -> String {
        match serde_json::from_str::<SearchOptions>(opts_json) {
            Ok(o) => to_json(&self.inner.search(query, &o)),
            Err(e) => err_json(e),
        }
    }

    pub fn set_access(&mut self, path: &str, ts_json: &str) -> Result<(), JsValue> {
        let ts: Vec<f64> = serde_json::from_str(ts_json).map_err(js_err)?;
        self.inner.set_access(path, ts);
        Ok(())
    }

    pub fn record_access(&mut self, path: &str, now: f64) {
        self.inner.record_access(path, now);
    }

    pub fn record_query_open(&mut self, q: &str, path: &str) {
        self.inner.record_query_open(q, path);
    }

    pub fn clear_frecency(&mut self) {
        self.inner.clear_frecency();
    }

    pub fn set_ghosts(&mut self, json: &str) -> Result<(), JsValue> {
        let g: Vec<Ghost> = serde_json::from_str(json).map_err(js_err)?;
        self.inner.set_ghosts(g);
        Ok(())
    }

    pub fn similar(&self, path: &str, limit: u32, min_score: f32) -> String {
        to_json(&self.inner.similar(path, limit as usize, min_score))
    }

    pub fn semantic(&self, query_vec: &[f32], limit: u32, min_score: f32) -> String {
        to_json(&self.inner.semantic(query_vec, limit as usize, min_score))
    }

    /// `data` arrives as an owned `Vec` (wasm-bindgen copies the `Float32Array` into wasm memory
    /// once), so the matrix is not copied a second time (13 MB for 8k x 384).
    pub fn set_vectors(&mut self, dims: u32, paths_json: &str, data: Vec<f32>) -> Result<(), JsValue> {
        let paths: Vec<String> = serde_json::from_str(paths_json).map_err(js_err)?;
        self.inner
            .set_vectors(dims as usize, paths, data)
            .map_err(|e| js_err(format!("{e:?}")))
    }

    pub fn clear_vectors(&mut self) {
        self.inner.clear_vectors();
    }

    pub fn has_vectors(&self) -> bool {
        self.inner.has_vectors()
    }
}

impl Default for WasmEngine {
    fn default() -> Self {
        Self::new()
    }
}
