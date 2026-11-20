//! Words learned for each transcription model, kept in
//! `learned_vocabulary.json` next to the settings. They are used together with
//! the user's own custom words, but only while that model is loaded: each
//! model mishears different words.
//!
//! Each word counts the dictations the model got it wrong in, so the
//! strongest ones (the most often misheard) come first and survive the limit.
//! Terms the AI saw were wrong but couldn't correct (an internal product name
//! neither model knows, say) wait as suspects for the user to name or dismiss.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use specta::Type;
use tauri::AppHandle;

const FILE_NAME: &str = "learned_vocabulary.json";
/// Spellings of a word the model wrote, kept per word.
const MAX_HEARD_AS: usize = 6;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct LearnedWord {
    pub word: String,
    /// Dictations the model got it wrong in.
    pub count: u32,
    /// How the model wrote it, most recent first.
    #[serde(default)]
    pub heard_as: Vec<String>,
    /// When it was last misheard, Unix seconds.
    #[serde(default)]
    pub last_seen: i64,
}

/// A term the AI saw was wrong but couldn't correct.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct Suspect {
    /// The term as the model wrote it.
    pub heard_as: String,
    /// Dictations it showed up in.
    pub count: u32,
    /// A short excerpt around it, from the latest dictation.
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub last_seen: i64,
    /// Dismissed by the user: kept so it isn't suggested again, never shown.
    #[serde(default)]
    pub dismissed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct ModelVocabulary {
    #[serde(default)]
    pub words: Vec<LearnedWord>,
    #[serde(default)]
    pub suspects: Vec<Suspect>,
}

impl ModelVocabulary {
    fn is_empty(&self) -> bool {
        self.words.is_empty() && self.suspects.is_empty()
    }

    /// The words, strongest first: most misheard, then most recent.
    pub fn words_by_strength(&self) -> Vec<String> {
        let mut words: Vec<&LearnedWord> = self.words.iter().collect();
        words.sort_by(|a, b| b.count.cmp(&a.count).then(b.last_seen.cmp(&a.last_seen)));
        words.into_iter().map(|w| w.word.clone()).collect()
    }

    pub fn word_index(&self, word: &str) -> Option<usize> {
        let word = word.to_lowercase();
        self.words
            .iter()
            .position(|w| w.word.to_lowercase() == word)
    }

    fn suspect_index(&self, heard_as: &str) -> Option<usize> {
        let heard_as = heard_as.to_lowercase();
        self.suspects
            .iter()
            .position(|s| s.heard_as.to_lowercase() == heard_as)
    }
}

/// A word misheard in `dictations` dictations of one run.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub word: String,
    pub heard_as: Vec<String>,
    pub dictations: u32,
}

/// A term the AI couldn't correct, seen in `dictations` dictations.
#[derive(Clone, Debug, PartialEq)]
pub struct SuspectHit {
    pub heard_as: String,
    pub context: String,
    pub dictations: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct LearnedVocabulary {
    /// Learned words and suspects per model id.
    #[serde(default, deserialize_with = "models_any_version")]
    pub models: BTreeMap<String, ModelVocabulary>,
    /// Model that re-transcribes the history; `None` picks the most accurate
    /// downloaded one.
    #[serde(default)]
    pub reference_model: Option<String>,
}

/// Reads the first version of the file too, a plain list of words per model.
fn models_any_version<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, ModelVocabulary>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Plain(Vec<String>),
        Full(ModelVocabulary),
    }
    let stored = BTreeMap::<String, Stored>::deserialize(deserializer)?;
    Ok(stored
        .into_iter()
        .map(|(model, stored)| {
            let vocabulary = match stored {
                Stored::Plain(words) => ModelVocabulary {
                    words: words
                        .into_iter()
                        .map(|word| LearnedWord {
                            word,
                            count: 1,
                            heard_as: Vec::new(),
                            last_seen: 0,
                        })
                        .collect(),
                    suspects: Vec::new(),
                },
                Stored::Full(vocabulary) => vocabulary,
            };
            (model, vocabulary)
        })
        .collect())
}

static CACHE: Mutex<Option<LearnedVocabulary>> = Mutex::new(None);

fn file_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::portable::app_data_dir(app)?.join(FILE_NAME))
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
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
    vocabulary.models.retain(|_, model| !model.is_empty());

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
/// ones learned for that model, strongest first (dictation only, see
/// [`without_learned_words`]).
pub fn custom_words_for(app: &AppHandle, model_id: Option<&str>, manual: &[String]) -> Vec<String> {
    let Some(model_id) = model_id.filter(|_| learned_words_allowed()) else {
        return manual.to_vec();
    };
    match load(app).models.get(model_id) {
        Some(learned) => merge_words(manual, &learned.words_by_strength()),
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

/// Puts `new` spellings first in `heard_as`, without repeats, keeping a few.
fn merge_heard_as(heard_as: &mut Vec<String>, new: &[String], word: &str) {
    for spelling in new.iter().rev() {
        let spelling = spelling.trim();
        if spelling.is_empty() || spelling.to_lowercase() == word.to_lowercase() {
            continue;
        }
        let lower = spelling.to_lowercase();
        heard_as.retain(|h| h.to_lowercase() != lower);
        heard_as.insert(0, spelling.to_string());
    }
    heard_as.truncate(MAX_HEARD_AS);
}

/// Counts `hits` in a model's words, adding the new ones. A suspect written
/// like one of a hit's spellings is now solved, so it folds into the word.
/// Past `max` words the weakest go: least misheard, then oldest.
pub fn record_hits(model: &mut ModelVocabulary, hits: &[Hit], now: i64, max: usize) {
    for hit in hits {
        let mut count = hit.dictations;
        for spelling in &hit.heard_as {
            if let Some(index) = model.suspect_index(spelling) {
                let suspect = model.suspects.remove(index);
                if !suspect.dismissed {
                    count += suspect.count;
                }
            }
        }
        match model.word_index(&hit.word) {
            Some(index) => {
                let word = &mut model.words[index];
                word.count += count;
                word.last_seen = now;
                let name = word.word.clone();
                merge_heard_as(&mut word.heard_as, &hit.heard_as, &name);
            }
            None => {
                let mut heard_as = Vec::new();
                merge_heard_as(&mut heard_as, &hit.heard_as, &hit.word);
                model.words.push(LearnedWord {
                    word: hit.word.clone(),
                    count,
                    heard_as,
                    last_seen: now,
                });
            }
        }
    }
    keep_strongest(&mut model.words, max, |w| (w.count, w.last_seen));
}

/// Counts `suspects` in a model's suspects, adding the new ones. Dismissed
/// ones stay dismissed. Past `max` the weakest go.
pub fn record_suspects(model: &mut ModelVocabulary, suspects: &[SuspectHit], now: i64, max: usize) {
    for hit in suspects {
        match model.suspect_index(&hit.heard_as) {
            Some(index) => {
                let suspect = &mut model.suspects[index];
                if suspect.dismissed {
                    continue;
                }
                suspect.count += hit.dictations;
                suspect.last_seen = now;
                if !hit.context.is_empty() {
                    suspect.context = hit.context.clone();
                }
            }
            None => model.suspects.push(Suspect {
                heard_as: hit.heard_as.clone(),
                count: hit.dictations,
                context: hit.context.clone(),
                last_seen: now,
                dismissed: false,
            }),
        }
    }
    keep_strongest(&mut model.suspects, max, |s| (s.count, s.last_seen));
}

/// Drops the weakest entries (lowest `strength`) past `max`.
fn keep_strongest<T>(items: &mut Vec<T>, max: usize, strength: impl Fn(&T) -> (u32, i64)) {
    while items.len() > max {
        let weakest = items
            .iter()
            .enumerate()
            .min_by_key(|(_, item)| strength(item))
            .map(|(index, _)| index)
            .unwrap();
        items.remove(weakest);
    }
}

/// Renames a learned word, keeping its count. When `new` is already learned,
/// the two merge.
pub fn rename_word(model: &mut ModelVocabulary, old: &str, new: &str) {
    let new = new.trim();
    let Some(index) = model.word_index(old) else {
        return;
    };
    if new.is_empty() || model.words[index].word == new {
        return;
    }
    match model.word_index(new) {
        Some(other) if other != index => {
            let old = model.words.remove(index);
            let other = if other > index { other - 1 } else { other };
            let target = &mut model.words[other];
            target.count += old.count;
            target.last_seen = target.last_seen.max(old.last_seen);
            let mut spellings = old.heard_as;
            spellings.push(old.word);
            let name = target.word.clone();
            merge_heard_as(&mut target.heard_as, &spellings, &name);
        }
        _ => {
            let word = &mut model.words[index];
            let previous = std::mem::replace(&mut word.word, new.to_string());
            // A change of case only isn't a spelling the model wrote.
            if previous.to_lowercase() != new.to_lowercase() {
                merge_heard_as(&mut word.heard_as, &[previous], new);
            }
        }
    }
}

/// The user named a suspect: it becomes a learned word with its count.
pub fn resolve_suspect(
    model: &mut ModelVocabulary,
    heard_as: &str,
    word: &str,
    now: i64,
    max: usize,
) {
    let word = word.trim();
    if word.is_empty() {
        return;
    }
    let Some(index) = model.suspect_index(heard_as) else {
        return;
    };
    let suspect = model.suspects.remove(index);
    let hit = Hit {
        word: word.to_string(),
        heard_as: vec![suspect.heard_as],
        dictations: suspect.count,
    };
    record_hits(model, &[hit], now, max);
}

pub fn dismiss_suspect(model: &mut ModelVocabulary, heard_as: &str) {
    if let Some(index) = model.suspect_index(heard_as) {
        model.suspects[index].dismissed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    fn hit(word: &str, heard_as: &[&str], dictations: u32) -> Hit {
        Hit {
            word: word.into(),
            heard_as: words(heard_as),
            dictations,
        }
    }

    fn suspect(heard_as: &str, dictations: u32) -> SuspectHit {
        SuspectHit {
            heard_as: heard_as.into(),
            context: format!("... {heard_as} ..."),
            dictations,
        }
    }

    #[test]
    fn learned_words_follow_the_manual_ones_without_repeats() {
        let merged = merge_words(&words(&["Siplan", "jqGrid"]), &words(&["siplan", "Handy"]));
        assert_eq!(merged, words(&["Siplan", "jqGrid", "Handy"]));
    }

    #[test]
    fn hits_count_up_and_the_strongest_come_first() {
        let mut model = ModelVocabulary::default();
        record_hits(&mut model, &[hit("jqGrid", &["jay que grid"], 1)], 10, 40);
        record_hits(&mut model, &[hit("Siplan", &["si plan"], 1)], 20, 40);
        record_hits(&mut model, &[hit("JQGRID", &["jota que grid"], 2)], 30, 40);

        assert_eq!(model.words_by_strength(), words(&["jqGrid", "Siplan"]));
        let jq = &model.words[model.word_index("jqgrid").unwrap()];
        assert_eq!(jq.count, 3);
        assert_eq!(jq.last_seen, 30);
        assert_eq!(jq.heard_as, words(&["jota que grid", "jay que grid"]));
    }

    #[test]
    fn past_the_limit_the_weakest_word_goes_not_the_oldest() {
        let mut model = ModelVocabulary::default();
        record_hits(&mut model, &[hit("forte", &[], 5)], 1, 2);
        record_hits(&mut model, &[hit("fraca", &[], 1)], 2, 2);
        record_hits(&mut model, &[hit("nova", &[], 1)], 3, 2);
        assert_eq!(model.words_by_strength(), words(&["forte", "nova"]));
    }

    #[test]
    fn a_suspect_solved_by_a_hit_folds_into_the_word() {
        let mut model = ModelVocabulary::default();
        record_suspects(&mut model, &[suspect("plano x", 3)], 1, 40);
        record_hits(&mut model, &[hit("PlanoX", &["Plano X"], 1)], 2, 40);
        assert!(model.suspects.is_empty());
        assert_eq!(model.words[0].count, 4);
    }

    #[test]
    fn suspects_count_up_and_dismissed_ones_stay_quiet() {
        let mut model = ModelVocabulary::default();
        record_suspects(&mut model, &[suspect("sisplan", 1)], 1, 40);
        record_suspects(&mut model, &[suspect("Sisplan", 2)], 2, 40);
        assert_eq!(model.suspects[0].count, 3);

        dismiss_suspect(&mut model, "SISPLAN");
        record_suspects(&mut model, &[suspect("sisplan", 1)], 3, 40);
        assert_eq!(model.suspects[0].count, 3);
        assert!(model.suspects[0].dismissed);
    }

    #[test]
    fn naming_a_suspect_turns_it_into_a_word_with_its_count() {
        let mut model = ModelVocabulary::default();
        record_suspects(&mut model, &[suspect("sisplan", 3)], 1, 40);
        resolve_suspect(&mut model, "sisplan", "Siplan", 5, 40);
        assert!(model.suspects.is_empty());
        assert_eq!(model.words[0].word, "Siplan");
        assert_eq!(model.words[0].count, 3);
        assert_eq!(model.words[0].heard_as, words(&["sisplan"]));
    }

    #[test]
    fn renaming_keeps_the_count_and_merges_into_an_existing_word() {
        let mut model = ModelVocabulary::default();
        record_hits(&mut model, &[hit("Jquery", &["jei query"], 2)], 1, 40);
        record_hits(&mut model, &[hit("jqGrid", &[], 1)], 2, 40);

        rename_word(&mut model, "Jquery", "jQuery");
        assert_eq!(model.words[0].word, "jQuery");
        assert_eq!(model.words[0].heard_as, words(&["jei query"]));

        rename_word(&mut model, "jQuery", "jqgrid");
        assert_eq!(model.words.len(), 1);
        assert_eq!(model.words[0].word, "jqGrid");
        assert_eq!(model.words[0].count, 3);
        assert_eq!(model.words[0].heard_as, words(&["jei query", "jQuery"]));
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
    fn an_old_file_with_plain_word_lists_still_reads() {
        let vocabulary: LearnedVocabulary =
            serde_json::from_str(r#"{"models":{"m":["Siplan","jqGrid"]}}"#).unwrap();
        let model = &vocabulary.models["m"];
        assert_eq!(model.words_by_strength(), words(&["Siplan", "jqGrid"]));
        assert_eq!(model.words[0].count, 1);
        assert_eq!(vocabulary.reference_model, None);

        // And the new format round-trips.
        let text = serde_json::to_string(&vocabulary).unwrap();
        let again: LearnedVocabulary = serde_json::from_str(&text).unwrap();
        assert_eq!(again, vocabulary);
    }
}
