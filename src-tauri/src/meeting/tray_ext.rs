//! The tray's "Meetings" submenu. Kept apart from tray.rs and lib.rs
//! (upstream), which only call in at a few hook lines.

use tauri::menu::{MenuItem, PredefinedMenuItem, Submenu};
use tauri::AppHandle;

use crate::tray_i18n::TrayStrings;

use super::{MeetingListing, MeetingStage};

/// Meetings listed in the submenu.
const MENU_MEETINGS: usize = 8;

/// Recent finished meetings, newest first, for the submenu.
pub fn recent(app: &AppHandle) -> Vec<MeetingListing> {
    super::recent_meetings(app, MENU_MEETINGS)
}

/// One entry per recent meeting: run the AI step when it has not run yet,
/// otherwise open the folder.
pub fn submenu(
    app: &AppHandle,
    meetings: &[MeetingListing],
    strings: &TrayStrings,
) -> tauri::Result<Submenu<tauri::Wry>> {
    let submenu = Submenu::with_id(app, "meetings_submenu", &strings.meetings, true)?;
    for meeting in meetings {
        let (id, action, enabled) = match meeting.stage {
            MeetingStage::Transcribed => ("meeting_summarize", &strings.meeting_summarize, true),
            MeetingStage::Summarizing => ("meeting_open", &strings.meeting_summarizing, false),
            MeetingStage::Summarized => ("meeting_open", &strings.meeting_open, true),
        };
        let item = MenuItem::with_id(
            app,
            format!("{id}:{}", meeting.name),
            format!("{} — {action}", meeting.label),
            enabled,
            None::<&str>,
        )?;
        submenu.append(&item)?;
    }
    if !meetings.is_empty() {
        submenu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    submenu.append(&MenuItem::with_id(
        app,
        "meetings_folder",
        &strings.meetings_folder,
        true,
        None::<&str>,
    )?)?;
    Ok(submenu)
}

/// Whether a tray menu id belongs to the submenu.
pub fn is_menu_id(id: &str) -> bool {
    id == "meetings_folder"
        || id.starts_with("meeting_summarize:")
        || id.starts_with("meeting_open:")
}

/// Runs the submenu entry `id` (one [`is_menu_id`] accepts).
pub fn on_menu_event(app: &AppHandle, id: &str) {
    if id == "meetings_folder" {
        match super::meetings_root(app) {
            Ok(root) => {
                let _ = std::fs::create_dir_all(&root);
                super::open_meeting_dir(app, &root);
            }
            Err(e) => log::warn!("Meetings folder unavailable: {e:#}"),
        }
        return;
    }
    let Some((action, name)) = id.split_once(':') else {
        return;
    };
    match super::meeting_dir(app, name) {
        Ok(dir) if action == "meeting_summarize" => super::start_summary(app, &dir),
        Ok(dir) => super::open_meeting_dir(app, &dir),
        Err(e) => log::warn!("Meeting menu action failed: {e:#}"),
    }
}
