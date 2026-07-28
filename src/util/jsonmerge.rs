//! A deliberately slot-agnostic JSON merge primitive: load an object (or start
//! from `{}`), let the caller mutate the keys IT owns, then atomic-write it back.
//! It must never grow knowledge of any specific slot's keys, types, or verify
//! policy — that knowledge stays in the slot modules.

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::path::Path;

use crate::util::atomicfile;

/// Load the JSON object at `path` (or `{}` if absent), hand the caller a mutable
/// reference to its top-level map, then atomic-write it back with `mode`.
pub fn merge_object<F>(path: &Path, mode: u32, mutate: F) -> Result<()>
where
    F: FnOnce(&mut Map<String, Value>) -> Result<()>,
{
    let mut root: Value = if path.exists() {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?
    } else {
        Value::Object(Map::new())
    };
    let map = match root.as_object_mut() {
        Some(m) => m,
        None => bail!("{} is not a JSON object", path.display()),
    };
    mutate(map)?;
    let bytes = serde_json::to_vec_pretty(&root)?;
    atomicfile::write_atomic(path, &bytes, mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preserves_other_keys_and_order_when_replacing_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        std::fs::write(&p, br#"{"alpha":1,"target":{"old":true},"zeta":2}"#).unwrap();

        merge_object(&p, 0o600, |m| {
            m.insert("target".to_string(), json!({"new": true}));
            Ok(())
        })
        .unwrap();

        let parsed: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(parsed["target"], json!({"new": true}));
        assert_eq!(parsed["alpha"], json!(1));
        let keys: Vec<&str> = parsed
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["alpha", "target", "zeta"]);
    }

    #[test]
    fn creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        merge_object(&p, 0o600, |m| {
            m.insert("k".to_string(), json!(1));
            Ok(())
        })
        .unwrap();
        let parsed: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(parsed["k"], json!(1));
    }

    #[test]
    fn rejects_non_object_root() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        std::fs::write(&p, b"[1,2,3]").unwrap();
        let err = merge_object(&p, 0o600, |_m| Ok(())).unwrap_err();
        assert!(err.to_string().contains("not a JSON object"));
    }
}
