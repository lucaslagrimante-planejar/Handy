//! Words learned for each transcription model, kept in
//! `learned_vocabulary.json` next to the settings. They are used together with
//! the user's own custom words, but only while that model is loaded: each
//! model mishears different words.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;

const FILE_NAME: &str = "learned_vocabulary.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct LearnedVocabulary {
    /// Words per model id, oldest first.
    #[serde(default)]
    pub models: BTreeMap<String, Vec<String>>,
    /// Model that re-transcribes the history; `None` picks the most accurate
    /// downloaded one.
    #[serde(default)]
    pub reference_model: Option<String>,
}

static CACHE: Mutex<Option<LearnedVocabulary>> = Mutex::new(None);

fn file_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::portable::app_data_dir(app)?.join(FILE_NAME))
}

/// The learned vocabulary; empty when the file is missing or unreadable.
pub fn load(app: &AppHandle) -> LearnedVocabulary {
    let mut cache = CACHE.lock().unwrap();
    if let Some(vocabulary) = cache.as_ref() {
        return vocabulary.clone();
    }
    let vocabulary = file_path(app)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(
            |text| match serde_json::from_str::<LearnedVocabulary>(&text) {
                Ok(vocabulary) => Some(vocabulary),
                Err(e) => {
                    log::warn!("Ignoring unreadable {FILE_NAME}: {e}");
                    None
                }
            },
        )
        .unwrap_or_default();
    *cache = Some(vocabulary.clone());
    vocabulary
}

/// Change the learned vocabulary and save it, returning the new state.
pub fn update(
    app: &AppHandle,
    change: impl FnOnce(&mut LearnedVocabulary),
) -> Result<LearnedVocabulary> {
    let mut vocabulary = load(app);
    change(&mut vocabulary);
    vocabulary.models.retain(|_, words| !words.is_empty());

    let path = file_path(app)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&vocabulary)?)?;
    std::fs::rename(&tmp, &path)?;

    *CACHE.lock().unwrap() = Some(vocabulary.clone());
    Ok(vocabulary)
}

thread_local! {
    /// Set while this thread runs a transcription that isn't a dictation.
    static WITHOUT_LEARNED_WORDS: Cell<bool> = const { Cell::new(false) };
}

/// Run `transcription` with only the user's custom words. Learned words are
/// for dictation; a meeting must never use them, even when it falls back to
/// the dictation engine. A batch transcription resolves its words on the
/// calling thread, so the flag reaches it.
pub fn without_learned_words<T>(transcription: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            WITHOUT_LEARNED_WORDS.with(|flag| flag.set(self.0));
        }
    }
    let _reset = Reset(WITHOUT_LEARNED_WORDS.with(|flag| flag.replace(true)));
    transcription()
}

fn learned_words_allowed() -> bool {
    !WITHOUT_LEARNED_WORDS.with(Cell::get)
}

/// The custom words a transcription on `model_id` uses: the user's, then the
/// ones learned for that model (dictation only, see [`without_learned_words`]).
pub fn custom_words_for(app: &AppHandle, model_id: Option<&str>, manual: &[String]) -> Vec<String> {
    let Some(model_id) = model_id.filter(|_| learned_words_allowed()) else {
        return manual.to_vec();
    };
    match load(app).models.get(model_id) {
        Some(learned) => merge_words(manual, learned),
        None => manual.to_vec(),
    }
}

/// `first` followed by the words of `second` it doesn't already have,
/// ignoring case.
pub fn merge_words(first: &[String], second: &[String]) -> Vec<String> {
    let mut merged = first.to_vec();
    for word in second {
        if !contains_ignoring_case(&merged, word) {
            merged.push(word.clone());
        }
    }
    merged
}

pub fn contains_ignoring_case(words: &[String], word: &str) -> bool {
    let word = word.to_lowercase();
    words.iter().any(|w| w.to_lowercase() == word)
}

/// Add `new` to a model's `learned` words, keeping at most `max`: the oldest
/// go first. Words already there (ignoring case) are not added again.
pub fn merge_learned(learned: &mut Vec<String>, new: &[String], max: usize) {
    for word in new {
        if !contains_ignoring_case(learned, word) {
            learned.push(word.clone());
        }
    }
    if learned.len() > max {
        learned.drain(..learned.len() - max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn learned_words_follow_the_manual_ones_without_repeats() {
        let merged = merge_words(&words(&["Siplan", "jqGrid"]), &words(&["siplan", "Handy"]));
        assert_eq!(merged, words(&["Siplan", "jqGrid", "Handy"]));
    }

    #[test]
    fn merging_learned_words_drops_the_oldest_past_the_limit() {
        let mut learned = words(&["a1", "b2", "c3"]);
        merge_learned(&mut learned, &words(&["B2", "d4", "e5"]), 4);
        assert_eq!(learned, words(&["b2", "c3", "d4", "e5"]));
    }

    #[test]
    fn learned_words_are_off_only_inside_without_learned_words() {
        assert!(learned_words_allowed());
        without_learned_words(|| {
            assert!(!learned_words_allowed());
            // Nested calls keep it off and restore it on the way out.
            without_learned_words(|| assert!(!learned_words_allowed()));
            assert!(!learned_words_allowed());
        });
        assert!(learned_words_allowed());

        let panicked = std::panic::catch_unwind(|| without_learned_words(|| panic!("boom")));
        assert!(panicked.is_err());
        assert!(learned_words_allowed());

        // Another thread (a live dictation's worker) is unaffected.
        without_learned_words(|| {
            assert!(std::thread::spawn(learned_words_allowed).join().unwrap());
        });
    }

    #[test]
    fn an_old_file_without_a_reference_model_still_reads() {
        let vocabulary: LearnedVocabulary =
            serde_json::from_str(r#"{"models":{"m":["Siplan"]}}"#).unwrap();
        assert_eq!(vocabulary.models["m"], words(&["Siplan"]));
        assert_eq!(vocabulary.reference_model, None);
    }
}
