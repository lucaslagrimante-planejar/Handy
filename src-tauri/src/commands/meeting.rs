use crate::meeting::history::{self, MeetingInfo};
use crate::meeting::summary::{self, AiProviderInfo};
use crate::meeting::{self, MeetingManager, StopAction};
use serde::Serialize;
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[derive(Serialize, Type)]
pub struct MeetingStatus {
    pub active: bool,
    /// Seconds since the meeting began, including adopted dictation audio.
    pub elapsed_secs: u32,
}

fn manager(app: &AppHandle) -> Arc<MeetingManager> {
    app.state::<Arc<MeetingManager>>().inner().clone()
}

#[tauri::command]
#[specta::specta]
pub fn start_meeting(app: AppHandle) -> Result<(), String> {
    manager(&app)
        .start()
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
}

/// Turn the dictation in progress into a meeting, keeping its audio.
#[tauri::command]
#[specta::specta]
pub fn convert_to_meeting(app: AppHandle) -> Result<(), String> {
    manager(&app)
        .convert_dictation()
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
#[specta::specta]
pub fn stop_meeting(app: AppHandle, then: StopAction) {
    manager(&app).stop_with(then);
}

/// Expand the meeting pill into the stop options (`open`), or collapse it.
#[tauri::command]
#[specta::specta]
pub fn show_meeting_stop_menu(app: AppHandle, open: bool) {
    manager(&app).show_stop_menu(open);
}

#[tauri::command]
#[specta::specta]
pub fn dismiss_meeting_invite(app: AppHandle) {
    manager(&app).dismiss_invite();
}

#[tauri::command]
#[specta::specta]
pub fn get_meeting_status(app: AppHandle) -> MeetingStatus {
    let elapsed = manager(&app).elapsed();
    MeetingStatus {
        active: elapsed.is_some(),
        elapsed_secs: elapsed.map_or(0, |d| d.as_secs() as u32),
    }
}

/// Run the AI step (revised transcript, summary, minutes) for a finished
/// meeting, identified by its folder name.
#[tauri::command]
#[specta::specta]
pub fn summarize_meeting(app: AppHandle, name: String) -> Result<(), String> {
    let dir = crate::meeting::meeting_dir(&app, &name).map_err(|e| format!("{e:#}"))?;
    crate::meeting::start_summary(&app, &dir);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn open_meeting_folder(app: AppHandle, name: String) -> Result<(), String> {
    let dir = crate::meeting::meeting_dir(&app, &name).map_err(|e| format!("{e:#}"))?;
    crate::meeting::open_meeting_dir(&app, &dir);
    Ok(())
}

/// Copy a meeting's summary to the clipboard.
#[tauri::command]
#[specta::specta]
pub fn copy_meeting_summary(app: AppHandle, name: String) -> Result<(), String> {
    let dir = crate::meeting::meeting_dir(&app, &name).map_err(|e| format!("{e:#}"))?;
    crate::meeting::copy_summary(&app, &dir).map_err(|e| format!("{e:#}"))
}

/// Meetings for the History tab, newest first.
#[tauri::command]
#[specta::specta]
pub fn list_meetings(app: AppHandle) -> Vec<MeetingInfo> {
    history::list_meetings(&app)
}

/// Provider and model the AI step will use (never the key).
#[tauri::command]
#[specta::specta]
pub fn get_meeting_ai_provider(app: AppHandle) -> Option<AiProviderInfo> {
    summary::provider_info(&app)
}

/// The best transcript of a meeting: revised, else final, else live.
#[tauri::command]
#[specta::specta]
pub fn get_meeting_transcript(app: AppHandle, name: String) -> Result<String, String> {
    let dir = meeting::meeting_dir(&app, &name).map_err(|e| format!("{e:#}"))?;
    history::best_transcript(&dir).map_err(|e| e.to_string())
}

/// Open one of the meeting's documents (summary, minutes, transcripts).
#[tauri::command]
#[specta::specta]
pub fn open_meeting_file(app: AppHandle, name: String, file: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !history::OPENABLE_FILES.contains(&file.as_str()) {
        return Err(format!("unknown meeting file: {file}"));
    }
    let path = meeting::meeting_dir(&app, &name)
        .map_err(|e| format!("{e:#}"))?
        .join(&file);
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn open_meetings_folder(app: AppHandle) -> Result<(), String> {
    let root = meeting::meetings_root(&app).map_err(|e| format!("{e:#}"))?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    meeting::open_meeting_dir(&app, &root);
    Ok(())
}

/// Delete only what the AI step generated, so it can be redone.
#[tauri::command]
#[specta::specta]
pub fn delete_meeting_ai_files(app: AppHandle, name: String) -> Result<(), String> {
    let dir = meeting::meeting_dir(&app, &name).map_err(|e| format!("{e:#}"))?;
    history::delete_ai_files(&dir).map_err(|e| format!("{e:#}"))?;
    meeting::meetings_changed(&app);
    Ok(())
}

/// Write the final transcript of an interrupted meeting, or retry speaker
/// separation.
#[tauri::command]
#[specta::specta]
pub fn finalize_meeting(app: AppHandle, name: String) -> Result<(), String> {
    let dir = meeting::meeting_dir(&app, &name).map_err(|e| format!("{e:#}"))?;
    meeting::finalize_meeting(&app, &dir).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
#[specta::specta]
pub fn delete_meeting(app: AppHandle, name: String) -> Result<(), String> {
    let dir = meeting::meeting_dir(&app, &name).map_err(|e| format!("{e:#}"))?;
    meeting::delete_meeting(&app, &dir).map_err(|e| format!("{e:#}"))
}
