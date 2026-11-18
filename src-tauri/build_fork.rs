//! Build steps of this fork, kept out of build.rs so syncing with upstream
//! touches as few of its lines as possible.

use std::fs;
use std::path::Path;

/// Merges the locale's `fork.json` (strings of the fork's features) into its
/// upstream `translation.json`, as the frontend does in `src/i18n/fork.ts`.
pub fn with_fork_strings(
    mut translation: serde_json::Value,
    locale_dir: &Path,
) -> serde_json::Value {
    let fork_path = locale_dir.join("fork.json");
    println!("cargo:rerun-if-changed={}", fork_path.display());
    let Ok(content) = fs::read_to_string(&fork_path) else {
        return translation;
    };
    let fork: serde_json::Value = serde_json::from_str(&content).unwrap();
    deep_merge(&mut translation, fork);
    translation
}

fn deep_merge(base: &mut serde_json::Value, extra: serde_json::Value) {
    match (base, extra) {
        (serde_json::Value::Object(base), serde_json::Value::Object(extra)) => {
            for (key, value) in extra {
                match base.get_mut(&key) {
                    Some(slot) => deep_merge(slot, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}
