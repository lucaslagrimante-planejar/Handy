//! Linux data source: PulseAudio/PipeWire streams via `pactl`, window titles
//! via `wmctrl` (X11).

use std::collections::HashMap;
use std::process::Command;

use serde_json::Value;

use super::{AudioUser, Direction};

/// Apps recording a real input or playing audio.
pub fn audio_users() -> Vec<AudioUser> {
    let mut users = mic_users().unwrap_or_default();
    users.extend(playback_users().unwrap_or_default());
    users
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
pub fn window_titles_by_pid() -> HashMap<u32, Vec<String>> {
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

    fn capture(binary: &str, pid: u32) -> AudioUser {
        AudioUser {
            binary: binary.into(),
            app_name: String::new(),
            pid: Some(pid),
            direction: Direction::Capture,
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
        assert_eq!(
            parse_mic_users(&sources, &outputs),
            vec![capture("webex", 10)]
        );
    }

    #[test]
    fn playback_users_skip_corked_streams() {
        let inputs = json!([
            {"corked": false, "properties": {"application.name": "WebexAudioPlayback_1", "application.process.binary": "CiscoCollabHost", "application.process.id": "20"}},
            {"corked": true, "properties": {"application.name": "Spotify", "application.process.id": "21"}}
        ]);
        assert_eq!(
            parse_playback_users(&inputs),
            vec![AudioUser {
                binary: "CiscoCollabHost".into(),
                app_name: "WebexAudioPlayback_1".into(),
                pid: Some(20),
                direction: Direction::Playback,
            }]
        );
    }

    #[test]
    fn wmctrl_titles_are_grouped_by_pid() {
        let windows = parse_wmctrl(
            "0x01  0 500   chorao Meet - abc-defg-hij - Google Chrome\n\
             0x02  0 500   chorao Second window\n\
             0x03  0 600   chorao Claude Code - Visual Studio Code\n",
        );
        assert_eq!(
            windows[&500],
            vec!["Meet - abc-defg-hij - Google Chrome", "Second window"]
        );
        assert_eq!(windows[&600], vec!["Claude Code - Visual Studio Code"]);
    }
}
