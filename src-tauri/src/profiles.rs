//! Saved sessions, stored as JSON in the app config directory.
//!
//! The content is owned by the UI (opaque JSON here) so the profile schema can
//! grow without backend changes. Passwords are never written to this file.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const FILE: &str = "sessions.json";

pub fn path(config_dir: &Path) -> PathBuf {
    config_dir.join(FILE)
}

pub fn load(config_dir: &Path) -> Result<Value, String> {
    let p = path(config_dir);
    match fs::read_to_string(&p) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} is corrupt: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({ "version": 1, "sessions": [] })),
        Err(e) => Err(format!("Cannot read {}: {e}", p.display())),
    }
}

pub fn save(config_dir: &Path, data: &Value) -> Result<(), String> {
    fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    let p = path(config_dir);
    let tmp = p.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    // Write-then-rename so a crash never leaves a half-written file.
    fs::write(&tmp, text).map_err(|e| format!("Cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &p).map_err(|e| format!("Cannot replace {}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_default() {
        let dir = std::env::temp_dir().join(format!("termworker-profiles-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(load(&dir).unwrap()["sessions"], json!([]));
        let data = json!({ "version": 1, "sessions": [{ "id": "a", "name": "Router" }] });
        save(&dir, &data).unwrap();
        assert_eq!(load(&dir).unwrap(), data);
        let _ = fs::remove_dir_all(&dir);
    }
}
