//! Meeting mode's part of the recording overlay: window sizes of its states,
//! the state being shown, click-through and keyboard control. Kept apart from
//! overlay.rs (upstream), which only calls in at a few hook lines.

use std::sync::atomic::Ordering;
use std::sync::Mutex;

use tauri::{AppHandle, Manager};

#[cfg(target_os = "linux")]
use gtk_layer_shell::{KeyboardMode, LayerShell};

use crate::overlay::{self, OVERLAY_ENABLED};

#[cfg(target_os = "linux")]
use crate::overlay::LAYER_SHELL_ACTIVE;

// Meeting pills with buttons — the call invite (label, action, dismiss) and
// the post-meeting card (title, note and a row of actions).
const OVERLAY_HEIGHT: f64 = 50.0;
const OVERLAY_INVITE_WIDTH: f64 = 340.0;
/// Post-meeting card with its actions row (MeetingOverlay.css `.cresult`).
const OVERLAY_CARD_HEIGHT: f64 = 104.0;
/// Meeting stop options: header plus three choices (`.cstop`).
const OVERLAY_STOP_MENU_HEIGHT: f64 = 172.0;
/// Participant names field of the stop options and of the "done" card
/// (`.sparticipants`), absent on macOS, whose panel can't take the keyboard.
const OVERLAY_PARTICIPANTS_HEIGHT: f64 = if cfg!(target_os = "macos") { 0.0 } else { 38.0 };

/// Window size (logical) of a meeting state, or `None` for upstream's states.
pub fn dimensions(state: &str) -> Option<(f64, f64)> {
    Some(match state {
        "invite" => (OVERLAY_INVITE_WIDTH, OVERLAY_HEIGHT),
        "done" => (
            OVERLAY_INVITE_WIDTH,
            OVERLAY_CARD_HEIGHT + OVERLAY_PARTICIPANTS_HEIGHT,
        ),
        "summarized" => (OVERLAY_INVITE_WIDTH, OVERLAY_CARD_HEIGHT),
        "stopmenu" => (
            OVERLAY_INVITE_WIDTH,
            OVERLAY_STOP_MENU_HEIGHT + OVERLAY_PARTICIPANTS_HEIGHT,
        ),
        _ => return None,
    })
}

/// State last shown by the overlay, or `None` while it is hidden.
static CURRENT_OVERLAY_STATE: Mutex<Option<String>> = Mutex::new(None);

/// What the overlay is showing right now (`"recording"`, `"meeting"`, ...).
pub fn current_overlay_state() -> Option<String> {
    CURRENT_OVERLAY_STATE.lock().unwrap().clone()
}

/// Called by overlay.rs when it shows a state (`Some`) or hides (`None`).
pub fn set_current_state(state: Option<&str>) {
    *CURRENT_OVERLAY_STATE.lock().unwrap() = state.map(str::to_string);
}

/// Called by overlay.rs once it has hidden the overlay.
pub fn on_hidden(app_handle: &AppHandle) {
    // A post-meeting card waiting under whatever was just hidden.
    super::restore_card(app_handle);
}

pub fn is_overlay_enabled() -> bool {
    OVERLAY_ENABLED.load(Ordering::Relaxed)
}

/// Shows the compact meeting overlay (icon, level, timer, stop button)
pub fn show_meeting_overlay(app_handle: &AppHandle) {
    overlay::show_overlay_state(app_handle, "meeting");
}

/// Shows the "a call is in progress, transcribe it?" invite
pub fn show_meeting_invite(app_handle: &AppHandle) {
    overlay::show_overlay_state(app_handle, "invite");
}

/// Limits where the overlay window takes clicks to the visible card, given in
/// the webview's CSS pixels. The window is a transparent rectangle larger than
/// the card (and kept sized for the biggest state), so without this its empty
/// corners swallow clicks meant for the windows below. An empty rectangle lets
/// every click through, which the overlay sends while it is hidden.
#[tauri::command]
#[specta::specta]
pub fn set_overlay_hit_region(app: AppHandle, x: f64, y: f64, width: f64, height: f64) {
    #[cfg(target_os = "linux")]
    {
        // GTK widgets are only safe to touch from the main thread.
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(overlay_window) = handle.get_webview_window("recording_overlay") else {
                return;
            };
            match overlay_window.gtk_window() {
                Ok(gtk_window) => {
                    use gtk::cairo::{RectangleInt, Region};
                    use gtk::prelude::WidgetExt;

                    let region = if width > 0.0 && height > 0.0 {
                        let left = x.floor() as i32;
                        let top = y.floor() as i32;
                        Region::create_rectangle(&RectangleInt::new(
                            left,
                            top,
                            (x + width).ceil() as i32 - left,
                            (y + height).ceil() as i32 - top,
                        ))
                    } else {
                        Region::create()
                    };
                    gtk_window.input_shape_combine_region(Some(&region));
                }
                Err(error) => log::error!("Failed to access GTK overlay window: {error}"),
            }
        });
    }

    // Windows and macOS keep the whole window clickable for now.
    #[cfg(not(target_os = "linux"))]
    let _ = (app, x, y, width, height);
}

/// Lets the overlay take the keyboard while one of its text fields is in use
/// (`enabled`), and gives it back afterwards. The overlay never takes focus
/// otherwise, so it doesn't steal typing from the window below.
#[tauri::command]
#[specta::specta]
pub fn set_overlay_keyboard(app: AppHandle, enabled: bool) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(overlay_window) = handle.get_webview_window("recording_overlay") else {
            return;
        };
        #[cfg(target_os = "linux")]
        if LAYER_SHELL_ACTIVE.load(Ordering::SeqCst) {
            if let Ok(gtk_window) = overlay_window.gtk_window() {
                gtk_window.set_keyboard_mode(if enabled {
                    KeyboardMode::OnDemand
                } else {
                    KeyboardMode::None
                });
            }
            return;
        }
        // macOS: the panel is built non-key (`can_become_key_window: false`)
        // and is left alone.
        #[cfg(not(target_os = "macos"))]
        {
            let _ = overlay_window.set_focusable(enabled);
            if enabled {
                let _ = overlay_window.set_focus();
            }
        }
        #[cfg(target_os = "macos")]
        let _ = (overlay_window, enabled);
    });
}
