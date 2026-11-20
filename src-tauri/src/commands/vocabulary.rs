//! Commands of the learned custom words (fork feature, see `crate::vocabulary`).

use tauri::AppHandle;

use crate::vocabulary::learn::{self, LearnReport};
use crate::vocabulary::store::{self, LearnedVocabulary};

fn to_string(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Read the unread dictations and learn the words their models got wrong.
#[tauri::command]
#[specta::specta]
pub async fn learn_custom_words(app: AppHandle) -> Result<LearnReport, String> {
    learn::learn(app).await.map_err(to_string)
}

#[tauri::command]
#[specta::specta]
pub fn cancel_learn_custom_words() {
    learn::cancel();
}

#[tauri::command]
#[specta::specta]
pub fn get_learned_vocabulary(app: AppHandle) -> LearnedVocabulary {
    store::load(&app)
}

#[tauri::command]
#[specta::specta]
pub fn remove_learned_word(
    app: AppHandle,
    model_id: String,
    word: String,
) -> Result<LearnedVocabulary, String> {
    store::update(&app, |vocabulary| {
        if let Some(model) = vocabulary.models.get_mut(&model_id) {
            model.words.retain(|w| w.word != word);
        }
    })
    .map_err(to_string)
}

/// Fixes a learned word's spelling, keeping its count.
#[tauri::command]
#[specta::specta]
pub fn rename_learned_word(
    app: AppHandle,
    model_id: String,
    word: String,
    new_word: String,
) -> Result<LearnedVocabulary, String> {
    store::update(&app, |vocabulary| {
        if let Some(model) = vocabulary.models.get_mut(&model_id) {
            store::rename_word(model, &word, &new_word);
        }
    })
    .map_err(to_string)
}

/// Names a term the AI couldn't correct: it becomes a learned word.
#[tauri::command]
#[specta::specta]
pub fn resolve_vocabulary_suspect(
    app: AppHandle,
    model_id: String,
    heard_as: String,
    word: String,
) -> Result<LearnedVocabulary, String> {
    store::update(&app, |vocabulary| {
        if let Some(model) = vocabulary.models.get_mut(&model_id) {
            store::resolve_suspect(
                model,
                &heard_as,
                &word,
                store::now(),
                learn::MAX_WORDS_PER_MODEL,
            );
        }
    })
    .map_err(to_string)
}

/// Hides a suspect for good: it isn't suggested again.
#[tauri::command]
#[specta::specta]
pub fn dismiss_vocabulary_suspect(
    app: AppHandle,
    model_id: String,
    heard_as: String,
) -> Result<LearnedVocabulary, String> {
    store::update(&app, |vocabulary| {
        if let Some(model) = vocabulary.models.get_mut(&model_id) {
            store::dismiss_suspect(model, &heard_as);
        }
    })
    .map_err(to_string)
}

#[tauri::command]
#[specta::specta]
pub fn clear_learned_words(app: AppHandle, model_id: String) -> Result<LearnedVocabulary, String> {
    store::update(&app, |vocabulary| {
        vocabulary.models.remove(&model_id);
    })
    .map_err(to_string)
}

/// `None` goes back to the most accurate downloaded model.
#[tauri::command]
#[specta::specta]
pub fn set_vocabulary_reference_model(
    app: AppHandle,
    model_id: Option<String>,
) -> Result<LearnedVocabulary, String> {
    store::update(&app, |vocabulary| vocabulary.reference_model = model_id).map_err(to_string)
}
