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
        if let Some(words) = vocabulary.models.get_mut(&model_id) {
            words.retain(|w| w != &word);
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
