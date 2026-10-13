//! Call detection: a meeting is likely in progress when a call app uses audio.
//! The rules ([`classify`]) are shared; each OS only supplies the data — who
//! has a live audio stream, and the window titles (see `linux.rs`,
//! `windows.rs`, `macos.rs`).
//!
//! Native call apps count on their own (recording the microphone, or — for the
//! dedicated softphone/meeting clients — playing call audio while muted);
//! browsers only when one of their windows looks like a meeting (they use the
//! mic for plenty else).
//!
//! Apps are matched by substring on both the process binary and the audio
//! stream's application name, since the same client shows up under several
//! names (Webex: `Webex`, `CiscoCollabHost`, `WebexAudioCapture_<id>`, ...).

use std::collections::HashMap;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    pub fn audio_users() -> Vec<super::AudioUser> {
        Vec::new()
    }
    pub fn window_titles_by_pid() -> std::collections::HashMap<u32, Vec<String>> {
        Default::default()
    }
}

/// `(fragment, name shown to the user, counts on playback alone)`.
/// Playback alone only counts for dedicated call clients: chat apps play
/// notification sounds and music all the time.
const NATIVE_CALL_APPS: &[(&str, &str, bool)] = &[
    ("webex", "Webex", true),
    ("ciscocollabhost", "Webex", true),
    // Webex Meetings on Windows; the Webex app's macOS bundle id.
    ("atmgr", "Webex", true),
    ("cisco-systems.spark", "Webex", true),
    ("zoom", "Zoom", true),
    ("teams", "Teams", true),
    ("skype", "Skype", true),
    ("bluejeans", "BlueJeans", true),
    ("ringcentral", "RingCentral", true),
    ("jitsi", "Jitsi", true),
    ("slack", "Slack", false),
    ("discord", "Discord", false),
];

const BROWSERS: &[&str] = &[
    "chrome",
    "chromium",
    "firefox",
    "brave",
    "msedge",
    "microsoft-edge",
    "vivaldi",
    "opera",
    "zen",
    "librewolf",
    // macOS: Edge's helpers, and Safari's audio, which plays from WebKit's
    // GPU process.
    "microsoft edge",
    "safari",
    "webkit",
];

/// Window-title fragments of browser meetings, with the name shown to the user.
const BROWSER_MEETING_TITLES: &[(&str, &str)] = &[
    ("meet -", "Google Meet"),
    ("meet –", "Google Meet"),
    ("meet —", "Google Meet"),
    ("meet:", "Google Meet"),
    ("meet.google.com", "Google Meet"),
    ("google meet", "Google Meet"),
    ("zoom meeting", "Zoom"),
    ("zoom.us", "Zoom"),
    ("zoom", "Zoom"),
    ("microsoft teams", "Teams"),
    ("teams.microsoft", "Teams"),
    ("teams.live", "Teams"),
    ("webex", "Webex"),
    ("jitsi", "Jitsi"),
    ("whereby", "Whereby"),
    ("bluejeans", "BlueJeans"),
    ("gotomeeting", "GoTo Meeting"),
    ("huddle", "Slack"),
    ("discord", "Discord"),
];

/// Which side of the audio graph an application stream is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Recording a real (non-monitor) input.
    Capture,
    /// Playing audio to an output.
    Playback,
}

/// An application with a live audio stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioUser {
    /// Executable file name (`chrome.exe`, `zoom.us`, `CiscoCollabHost`).
    pub binary: String,
    /// What the audio stack calls the app: PulseAudio's `application.name`,
    /// the Windows session display name, the macOS bundle id.
    pub app_name: String,
    pub pid: Option<u32>,
    pub direction: Direction,
}

/// Name of the call app in progress, if any.
pub fn detect_call() -> Option<String> {
    let users = platform::audio_users();
    if users.is_empty() {
        return None;
    }
    let windows = platform::window_titles_by_pid();
    classify(&users, &windows, std::process::id())
}

pub fn classify(
    users: &[AudioUser],
    windows: &HashMap<u32, Vec<String>>,
    own_pid: u32,
) -> Option<String> {
    for user in users {
        if user.pid == Some(own_pid) {
            continue;
        }
        let binary = user.binary.to_lowercase();
        let app_name = user.app_name.to_lowercase();
        let matches = |fragment: &str| binary.contains(fragment) || app_name.contains(fragment);

        if let Some((_, name, playback_counts)) = NATIVE_CALL_APPS
            .iter()
            .find(|(fragment, _, _)| matches(fragment))
        {
            if user.direction == Direction::Capture || *playback_counts {
                return Some(name.to_string());
            }
            continue;
        }
        if user.direction == Direction::Capture && BROWSERS.iter().any(|b| matches(b)) {
            // Browser audio usually comes from a helper process, so look at
            // every window rather than only the recording pid's.
            let meeting = windows.values().flatten().find_map(|title| {
                let title = title.to_lowercase();
                BROWSER_MEETING_TITLES
                    .iter()
                    .find(|(fragment, _)| title.contains(fragment))
                    .map(|(_, name)| name.to_string())
            });
            if meeting.is_some() {
                return meeting;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(binary: &str, pid: u32) -> AudioUser {
        AudioUser {
            binary: binary.into(),
            app_name: String::new(),
            pid: Some(pid),
            direction: Direction::Capture,
        }
    }

    fn playback(binary: &str, app_name: &str, pid: u32) -> AudioUser {
        AudioUser {
            binary: binary.into(),
            app_name: app_name.into(),
            pid: Some(pid),
            direction: Direction::Playback,
        }
    }

    /// pid -> window titles.
    fn windows(entries: &[(u32, &str)]) -> HashMap<u32, Vec<String>> {
        let mut map: HashMap<u32, Vec<String>> = HashMap::new();
        for (pid, title) in entries {
            map.entry(*pid).or_default().push(title.to_string());
        }
        map
    }

    #[test]
    fn native_call_apps_count_on_mic_use_alone() {
        let found = classify(&[user("webex", 10)], &HashMap::new(), 1);
        assert_eq!(found.as_deref(), Some("Webex"));
    }

    #[test]
    fn webex_variations_are_recognized() {
        // The Webex client records through "CiscoCollabHost", and only the
        // application name says "Webex".
        for binary in [
            "CiscoCollabHost",
            "Webex",
            "webex.bin",
            "CiscoCollabHostCef",
        ] {
            let found = classify(&[user(binary, 10)], &HashMap::new(), 1);
            assert_eq!(found.as_deref(), Some("Webex"), "{binary}");
        }
        let by_name = AudioUser {
            binary: String::new(),
            app_name: "WebexAudioCapture_84116072".into(),
            pid: Some(10),
            direction: Direction::Capture,
        };
        assert_eq!(
            classify(&[by_name], &HashMap::new(), 1).as_deref(),
            Some("Webex")
        );
    }

    #[test]
    fn windows_and_macos_names_are_recognized() {
        for (binary, app_name, expected) in [
            ("ms-teams.exe", "", "Teams"),
            ("Zoom.exe", "", "Zoom"),
            ("atmgr.exe", "", "Webex"),
            ("CiscoCollabHost.exe", "", "Webex"),
            ("Webex", "Cisco-Systems.Spark", "Webex"),
            ("zoom.us", "us.zoom.xos", "Zoom"),
            ("MSTeams", "com.microsoft.teams2", "Teams"),
        ] {
            let found = classify(
                &[AudioUser {
                    binary: binary.into(),
                    app_name: app_name.into(),
                    pid: Some(10),
                    direction: Direction::Capture,
                }],
                &HashMap::new(),
                1,
            );
            assert_eq!(found.as_deref(), Some(expected), "{binary}");
        }
    }

    #[test]
    fn muted_call_is_seen_through_playback() {
        // Muted in the call: no capture stream, call audio still plays.
        let users = [playback("CiscoCollabHost", "WebexAudioPlayback_1", 20)];
        assert_eq!(
            classify(&users, &HashMap::new(), 1).as_deref(),
            Some("Webex")
        );
    }

    #[test]
    fn chat_apps_need_the_mic() {
        let users = [playback("discord", "Discord", 30)];
        assert_eq!(classify(&users, &HashMap::new(), 1), None);
        assert_eq!(
            classify(&[user("discord", 30)], &HashMap::new(), 1).as_deref(),
            Some("Discord")
        );
    }

    #[test]
    fn browsers_need_a_meeting_window() {
        let meeting = windows(&[
            (500, "Meet - abc-defg-hij - Google Chrome"),
            (600, "Claude Code - Visual Studio Code"),
        ]);
        assert_eq!(
            classify(&[user("chrome", 777)], &meeting, 1).as_deref(),
            Some("Google Meet")
        );
        assert_eq!(
            classify(&[user("chrome.exe", 777)], &meeting, 1).as_deref(),
            Some("Google Meet")
        );
        let no_meeting = windows(&[(600, "YouTube - Google Chrome")]);
        assert_eq!(classify(&[user("chrome", 777)], &no_meeting, 1), None);
    }

    #[test]
    fn safari_meetings_need_a_meeting_window() {
        let meeting = windows(&[(500, "Meet - abc-defg-hij")]);
        let webkit = user("com.apple.WebKit.GPU", 777);
        assert_eq!(
            classify(std::slice::from_ref(&webkit), &meeting, 1).as_deref(),
            Some("Google Meet")
        );
        assert_eq!(classify(&[webkit], &HashMap::new(), 1), None);
    }

    #[test]
    fn browser_playback_alone_is_not_a_call() {
        let meeting = windows(&[(500, "Meet - abc-defg-hij - Google Chrome")]);
        let users = [playback("chrome", "Google Chrome", 777)];
        assert_eq!(classify(&users, &meeting, 1), None);
    }

    #[test]
    fn own_process_is_ignored() {
        assert_eq!(classify(&[user("webex", 42)], &HashMap::new(), 42), None);
    }
}
