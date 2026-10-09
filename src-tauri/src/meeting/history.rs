//! Meetings as shown in the History tab: what each one has on disk and what
//! is still pending.

use std::path::Path;

use chrono::{Local, NaiveDateTime, TimeZone};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager};

use super::diarize::{self, DIARIZATION_JSON, FINAL_MD};
use super::summary::{self, MINUTES_MD, REVISED_MD, REVISED_SRT, SUMMARY_MD};
use super::writer::{self, TranscriptEntry, TRANSCRIPT_JSONL, TRANSCRIPT_MD};
use super::{is_in_progress, meetings_root, MeetingManager};

/// Files the History tab may open, in the order it lists them.
pub const OPENABLE_FILES: [&str; 6] = [
    SUMMARY_MD,
    MINUTES_MD,
    REVISED_MD,
    REVISED_SRT,
    FINAL_MD,
    diarize::FINAL_SRT,
];

/// How many characters of the transcript the History card shows.
const PREVIEW_CHARS: usize = 400;

#[derive(Serialize, Type, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeetingState {
    /// Still recording.
    Recording,
    /// Stopped; the last lines and speaker separation are being written.
    Finishing,
    /// Stopped without the final transcript (app closed or crashed mid-meeting).
    Unfinished,
    /// Final transcript written; the AI step has not run.
    Transcribed,
    Summarizing,
    Summarized,
}

#[derive(Serialize, Type, Clone, Debug)]
pub struct MeetingInfo {
    /// Folder name, used as the meeting id.
    pub name: String,
    /// Start time, Unix seconds.
    pub timestamp: i64,
    pub duration_secs: u32,
    pub speakers: Vec<String>,
    pub lines: u32,
    pub preview: String,
    pub state: MeetingState,
    /// Speakers were separated by the diarization model.
    pub diarized: bool,
    /// Which of [`OPENABLE_FILES`] exist.
    pub files: Vec<String>,
}

/// All meetings with a transcript, newest first.
pub fn list_meetings(app: &AppHandle) -> Vec<MeetingInfo> {
    let Ok(root) = meetings_root(app) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let active = app.state::<std::sync::Arc<MeetingManager>>().active_dir();
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    names
        .into_iter()
        .filter_map(|name| {
            let dir = root.join(&name);
            let recording = active.as_deref() == Some(dir.as_path());
            meeting_info(&dir, name, recording)
        })
        .collect()
}

fn meeting_info(dir: &Path, name: String, recording: bool) -> Option<MeetingInfo> {
    let has_final = dir.join(FINAL_MD).exists();
    let has_live = std::fs::metadata(dir.join(TRANSCRIPT_JSONL)).is_ok_and(|m| m.len() > 0);
    if !recording && !has_final && !has_live {
        // Empty meeting (nobody spoke) or not a meeting folder.
        return None;
    }
    let entries = transcript_entries(dir, has_final);
    let state = if recording {
        MeetingState::Recording
    } else if is_in_progress(dir) {
        MeetingState::Finishing
    } else if !has_final {
        MeetingState::Unfinished
    } else if summary::is_running(dir) {
        MeetingState::Summarizing
    } else if summary::is_done(dir) {
        MeetingState::Summarized
    } else {
        MeetingState::Transcribed
    };
    let mut speakers: Vec<String> = Vec::new();
    for entry in &entries {
        if !speakers.contains(&entry.speaker) {
            speakers.push(entry.speaker.clone());
        }
    }
    Some(MeetingInfo {
        timestamp: start_timestamp(&name).unwrap_or_else(|| modified_timestamp(dir)),
        name,
        duration_secs: entries
            .iter()
            .map(|e| e.end_ms)
            .max()
            .unwrap_or(0)
            .div_ceil(1000) as u32,
        speakers,
        lines: entries.len() as u32,
        preview: preview(&entries),
        state,
        diarized: dir.join(DIARIZATION_JSON).exists(),
        files: OPENABLE_FILES
            .iter()
            .filter(|file| dir.join(file).exists())
            .map(|file| file.to_string())
            .collect(),
    })
}

fn transcript_entries(dir: &Path, has_final: bool) -> Vec<TranscriptEntry> {
    let final_jsonl = dir.join(diarize::FINAL_JSONL);
    let mut entries = if has_final && final_jsonl.exists() {
        std::fs::read_to_string(final_jsonl)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    } else {
        writer::read_transcript(dir).unwrap_or_default()
    };
    entries.sort_by_key(|e: &TranscriptEntry| e.start_ms);
    entries
}

fn preview(entries: &[TranscriptEntry]) -> String {
    let mut text = String::new();
    for entry in entries {
        if text.chars().count() >= PREVIEW_CHARS {
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("{}: {}", entry.speaker, entry.text));
    }
    match text.char_indices().nth(PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

/// Folder names are the local start time, "2026-10-09_14-30-05".
fn start_timestamp(name: &str) -> Option<i64> {
    let naive = NaiveDateTime::parse_from_str(name, "%Y-%m-%d_%H-%M-%S").ok()?;
    Some(Local.from_local_datetime(&naive).earliest()?.timestamp())
}

fn modified_timestamp(dir: &Path) -> i64 {
    std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// Everything the AI step writes. The transcripts it starts from are never in
/// this list, so deleting these always leaves the original intact.
pub const AI_FILES: [&str; 4] = [REVISED_MD, REVISED_SRT, SUMMARY_MD, MINUTES_MD];

/// Remove the AI output so it can be generated again (e.g. with another model).
pub fn delete_ai_files(dir: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        !summary::is_running(dir),
        "o resumo desta reunião está sendo gerado"
    );
    for file in AI_FILES {
        match std::fs::remove_file(dir.join(file)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

/// The best transcript to copy: revised, else final, else live.
pub fn best_transcript(dir: &Path) -> std::io::Result<String> {
    [REVISED_MD, FINAL_MD, TRANSCRIPT_MD]
        .iter()
        .map(|file| dir.join(file))
        .find(|path| path.exists())
        .map_or_else(
            || Err(std::io::Error::other("a reunião não tem transcrição")),
            std::fs::read_to_string,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::writer::Channel;

    fn entry(start_ms: u64, speaker: &str, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            start_ms,
            end_ms: start_ms + 1_000,
            channel: Channel::System,
            speaker: speaker.into(),
            text: text.into(),
        }
    }

    #[test]
    fn preview_is_cut_on_char_boundaries() {
        let long = "ação ".repeat(200);
        let text = preview(&[entry(0, "Pessoa 1", &long)]);
        assert!(text.ends_with('…'));
        assert_eq!(text.chars().count(), PREVIEW_CHARS + 1);
        let short = preview(&[entry(0, "Eu", "Oi."), entry(1, "Pessoa 1", "Olá.")]);
        assert_eq!(short, "Eu: Oi.\nPessoa 1: Olá.");
    }

    #[test]
    fn deleting_ai_files_keeps_the_transcripts() {
        let dir = std::env::temp_dir().join(format!("handy-ai-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let kept = [
            TRANSCRIPT_JSONL,
            TRANSCRIPT_MD,
            FINAL_MD,
            diarize::FINAL_JSONL,
        ];
        for file in AI_FILES.iter().chain(kept.iter()) {
            std::fs::write(dir.join(file), "x").unwrap();
        }
        delete_ai_files(&dir).unwrap();
        delete_ai_files(&dir).unwrap(); // already gone is fine
        assert!(AI_FILES.iter().all(|file| !dir.join(file).exists()));
        assert!(kept.iter().all(|file| dir.join(file).exists()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folder_names_become_timestamps() {
        assert!(start_timestamp("2026-10-09_14-30-05").is_some());
        assert_eq!(start_timestamp("notes"), None);
    }
}
