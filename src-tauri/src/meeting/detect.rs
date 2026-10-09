//! Call detection: a meeting is likely in progress when a call app uses audio.
//! Native call apps count on their own (recording the microphone, or — for the
//! dedicated softphone/meeting clients — playing call audio while muted);
//! browsers only when one of their windows looks like a meeting (they use the
//! mic for plenty else).
//!
//! Apps are matched by substring on both the process binary and the audio
//! stream's application name, since the same client shows up under several
//! names (Webex: `Webex`, `CiscoCollabHost`, `WebexAudioCapture_<id>`, ...).

use std::collections::HashMap;
use std::process::Command;

use serde_json::Value;

/// `(fragment, name shown to the user, counts on playback alone)`.
/// Playback alone only counts for dedicated call clients: chat apps play
/// notification sounds and music all the time.
const NATIVE_CALL_APPS: &[(&str, &str, bool)] = &[
    ("webex", "Webex", true),
    ("ciscocollabhost", "Webex", true),
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
    pub binary: String,
    pub app_name: String,
    pub pid: Option<u32>,
    pub direction: Direction,
}

/// Name of the call app in progress, if any.
pub fn detect_call() -> Option<String> {
    let mut users = mic_users().unwrap_or_default();
    users.extend(playback_users().unwrap_or_default());
    if users.is_empty() {
        return None;
    }
    let windows = window_titles_by_pid();
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

fn pactl_json(args: &[&str]) -> anyhow::Result<Value> {
    let output = Command::new("pactl")
        .args(["-f", "json", "list"])
        .args(args)
        .output()?;
    anyhow::ensure!(output.status.success(), "pactl failed");
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn mic_users() -> anyhow::Result<Vec<AudioUser>> {
    let sources = pactl_json(&["sources"])?;
    let outputs = pactl_json(&["source-outputs"])?;
    Ok(parse_mic_users(&sources, &outputs))
}

fn playback_users() -> anyhow::Result<Vec<AudioUser>> {
    let inputs = pactl_json(&["sink-inputs"])?;
    Ok(parse_playback_users(&inputs))
}

fn audio_user(stream: &Value, direction: Direction) -> Option<AudioUser> {
    let props = &stream["properties"];
    let binary = props["application.process.binary"].as_str();
    let app_name = props["application.name"].as_str();
    if binary.is_none() && app_name.is_none() {
        return None;
    }
    Some(AudioUser {
        binary: binary.unwrap_or_default().to_string(),
        app_name: app_name.unwrap_or_default().to_string(),
        pid: props["application.process.id"]
            .as_str()
            .and_then(|pid| pid.parse().ok()),
        direction,
    })
}

pub fn parse_mic_users(sources: &Value, outputs: &Value) -> Vec<AudioUser> {
    let monitors: Vec<u64> = sources
        .as_array()
        .into_iter()
        .flatten()
        .filter(|source| {
            source["name"]
                .as_str()
                .is_some_and(|name| name.ends_with(".monitor"))
        })
        .filter_map(|source| source["index"].as_u64())
        .collect();

    outputs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|output| {
            output["source"]
                .as_u64()
                .is_some_and(|index| !monitors.contains(&index))
                && output["corked"].as_bool() != Some(true)
        })
        .filter_map(|output| audio_user(output, Direction::Capture))
        .collect()
}

pub fn parse_playback_users(inputs: &Value) -> Vec<AudioUser> {
    inputs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|input| input["corked"].as_bool() != Some(true))
        .filter_map(|input| audio_user(input, Direction::Playback))
        .collect()
}

/// Window titles grouped by owning pid, via `wmctrl -lp` (X11).
fn window_titles_by_pid() -> HashMap<u32, Vec<String>> {
    Command::new("wmctrl")
        .arg("-lp")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| parse_wmctrl(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

pub fn parse_wmctrl(listing: &str) -> HashMap<u32, Vec<String>> {
    let mut windows: HashMap<u32, Vec<String>> = HashMap::new();
    for line in listing.lines() {
        // <id> <desktop> <pid> <host> <title...>
        let mut fields = line.split_whitespace();
        let (Some(_id), Some(_desktop), Some(pid), Some(host)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(pid) = pid.parse() else { continue };
        let title = line
            .split_once(host)
            .map(|(_, rest)| rest.trim())
            .unwrap_or_default();
        windows.entry(pid).or_default().push(title.to_string());
    }
    windows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn mic_users_skip_monitors_corked_streams() {
        let sources = json!([
            {"index": 1, "name": "alsa_input.usb-headset.mono"},
            {"index": 2, "name": "alsa_output.usb-headset.monitor"}
        ]);
        let outputs = json!([
            {"source": 1, "corked": false, "properties": {"application.process.binary": "webex", "application.process.id": "10"}},
            {"source": 2, "corked": false, "properties": {"application.process.binary": "obs", "application.process.id": "11"}},
            {"source": 1, "corked": true, "properties": {"application.process.binary": "chrome", "application.process.id": "12"}}
        ]);
        assert_eq!(parse_mic_users(&sources, &outputs), vec![user("webex", 10)]);
    }

    #[test]
    fn playback_users_skip_corked_streams() {
        let inputs = json!([
            {"corked": false, "properties": {"application.name": "WebexAudioPlayback_1", "application.process.binary": "CiscoCollabHost", "application.process.id": "20"}},
            {"corked": true, "properties": {"application.name": "Spotify", "application.process.id": "21"}}
        ]);
        assert_eq!(
            parse_playback_users(&inputs),
            vec![playback("CiscoCollabHost", "WebexAudioPlayback_1", 20)]
        );
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
        let windows = parse_wmctrl(
            "0x01  0 500   chorao Meet - abc-defg-hij - Google Chrome\n\
             0x02  0 600   chorao Claude Code - Visual Studio Code\n",
        );
        assert_eq!(
            classify(&[user("chrome", 777)], &windows, 1).as_deref(),
            Some("Google Meet")
        );
        let no_meeting = parse_wmctrl("0x02  0 600   chorao YouTube - Google Chrome\n");
        assert_eq!(classify(&[user("chrome", 777)], &no_meeting, 1), None);
    }

    #[test]
    fn browser_playback_alone_is_not_a_call() {
        let windows = parse_wmctrl("0x01  0 500   chorao Meet - abc-defg-hij - Google Chrome\n");
        let users = [playback("chrome", "Google Chrome", 777)];
        assert_eq!(classify(&users, &windows, 1), None);
    }

    #[test]
    fn own_process_is_ignored() {
        assert_eq!(classify(&[user("webex", 42)], &HashMap::new(), 42), None);
    }
}
