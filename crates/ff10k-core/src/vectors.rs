//! Dense vector matrix (Smart Connections embeddings), keyed by note path.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VectorError {
    /// `data.len()` is not `paths.len() * dims`.
    DimsMismatch { expected: usize, got_len: usize },
    /// `dims == 0`.
    BadDims,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticHit {
    pub path: String,
    pub title: String,
    pub score: f32,
}

/// Row-major matrix. Keyed by path (never DocId: ids are reused after a remove).
#[derive(Debug)]
pub struct Vectors {
    dims: usize,
    rows: Vec<f32>,
    row_of: HashMap<String, u32>,
    path_of: Vec<String>,
}

/// Dot product with 8 independent accumulators: float addition is not associative, so a plain
/// `sum()` cannot be auto-vectorized; this form can.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ca.remainder().iter().zip(cb.remainder()).map(|(x, y)| x * y).sum();
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum::<f32>() + tail
}

impl Vectors {
    /// `data` holds `paths.len()` rows of `dims` floats. Rows are stored as given (expected to
    /// be unit-normalized already). A duplicate path keeps its LAST row.
    pub fn new(dims: usize, paths: Vec<String>, data: Vec<f32>) -> Result<Self, VectorError> {
        if dims == 0 {
            return Err(VectorError::BadDims);
        }
        let expected = paths.len().checked_mul(dims).unwrap_or(usize::MAX);
        if data.len() != expected {
            return Err(VectorError::DimsMismatch { expected, got_len: data.len() });
        }
        let mut last: HashMap<&str, usize> = HashMap::with_capacity(paths.len());
        for (i, p) in paths.iter().enumerate() {
            last.insert(p, i);
        }
        let mut v = Vectors {
            dims,
            rows: Vec::with_capacity(last.len() * dims),
            row_of: HashMap::with_capacity(last.len()),
            path_of: Vec::with_capacity(last.len()),
        };
        for (i, p) in paths.iter().enumerate() {
            if last[p.as_str()] == i {
                v.row_of.insert(p.clone(), v.path_of.len() as u32);
                v.path_of.push(p.clone());
                v.rows.extend_from_slice(&data[i * dims..(i + 1) * dims]);
            }
        }
        Ok(v)
    }

    pub fn dims(&self) -> usize {
        self.dims
    }

    pub fn len(&self) -> usize {
        self.path_of.len()
    }

    pub fn is_empty(&self) -> bool {
        self.path_of.is_empty()
    }

    pub fn get(&self, path: &str) -> Option<&[f32]> {
        let r = *self.row_of.get(path)? as usize;
        Some(&self.rows[r * self.dims..(r + 1) * self.dims])
    }

    /// Normalized copy of `q`; `None` on a dimension mismatch or a zero/non-finite norm.
    fn unit(&self, q: &[f32]) -> Option<Vec<f32>> {
        if q.len() != self.dims {
            return None;
        }
        let n = dot(q, q).sqrt();
        (n > 0.0 && n.is_finite()).then(|| q.iter().map(|x| x / n).collect())
    }

    /// All `(path, cosine)` with `cosine >= min_score` whose path passes `keep`, unordered.
    /// One dot product per row; empty on a dimension mismatch or a zero query.
    pub fn scan(&self, q: &[f32], min_score: f32, mut keep: impl FnMut(&str) -> bool) -> Vec<(&str, f32)> {
        let Some(q) = self.unit(q) else { return Vec::new() };
        let mut out = Vec::new();
        for (i, row) in self.rows.chunks_exact(self.dims).enumerate() {
            let s = dot(&q, row);
            if s >= min_score && keep(&self.path_of[i]) {
                out.push((self.path_of[i].as_str(), s));
            }
        }
        out
    }

    /// Top `limit` rows by cosine, best first (ties by path), skipping `exclude`.
    pub fn nearest(&self, q: &[f32], limit: usize, exclude: Option<&str>) -> Vec<(String, f32)> {
        top(self.scan(q, f32::NEG_INFINITY, |p| Some(p) != exclude), limit)
            .into_iter()
            .map(|(p, s)| (p.to_string(), s))
            .collect()
    }
}

/// Best `limit` entries, ordered by score desc then path.
pub fn top(mut v: Vec<(&str, f32)>, limit: usize) -> Vec<(&str, f32)> {
    let cmp = |a: &(&str, f32), b: &(&str, f32)| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0));
    if v.len() > limit {
        if limit == 0 {
            return Vec::new();
        }
        v.select_nth_unstable_by(limit - 1, cmp);
        v.truncate(limit);
    }
    v.sort_by(cmp);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn rejects_dims_mismatch() {
        let r = Vectors::new(384, s(&["a", "b", "c"]), vec![0.0; 384 * 2]);
        assert_eq!(r.unwrap_err(), VectorError::DimsMismatch { expected: 384 * 3, got_len: 384 * 2 });
        assert_eq!(Vectors::new(0, s(&["a"]), vec![]).unwrap_err(), VectorError::BadDims);
    }

    #[test]
    fn nearest_is_cosine_ordered() {
        let v = Vectors::new(
            3,
            s(&["x", "y", "z"]),
            vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.6, 0.8, 0.0],
        )
        .unwrap();
        let n = v.nearest(&[0.0, 2.0, 0.0], 3, None);
        assert_eq!(n[0].0, "y");
        assert!((n[0].1 - 1.0).abs() < 1e-6);
        assert_eq!(n[1].0, "z");
        assert!((n[1].1 - 0.8).abs() < 1e-6);
        assert_eq!(n[2].0, "x");
        assert!(n[2].1.abs() < 1e-6);
        let n = v.nearest(&[0.0, 1.0, 0.0], 3, Some("y"));
        assert_eq!(n.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["z", "x"]);
        assert!(v.nearest(&[0.0, 0.0, 0.0], 3, None).is_empty());
        assert!(v.nearest(&[1.0, 0.0], 3, None).is_empty());
        assert!(v.nearest(&[1.0, 0.0, 0.0], 0, None).is_empty());
    }

    #[test]
    fn duplicate_path_keeps_last_row() {
        let v = Vectors::new(2, s(&["a", "b", "a"]), vec![1.0, 0.0, 0.0, 1.0, 0.0, -1.0]).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v.get("a"), Some(&[0.0, -1.0][..]));
        assert_eq!(v.get("zz"), None);
    }
}
