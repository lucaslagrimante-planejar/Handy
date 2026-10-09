//! Speaker separation for the system channel with NVIDIA Streaming Sortformer.
//!
//! transcribe.cpp only exposes Sortformer as a batch run over a whole
//! recording, so it runs once the meeting stops, in a child process (like the
//! transcription worker) so a GPU driver crash cannot take the app down. The
//! live transcript labels the system channel "Outros"; this pass relabels
//! those lines "Pessoa 1..4" in `transcript-final.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use hf_hub::api::tokio::ApiBuilder;
use hf_hub::{Cache, Repo, RepoType};
use log::info;
use serde::{Deserialize, Serialize};

use super::writer::{
    format_timestamp, read_transcript, Channel, ChunkedWavWriter, TranscriptEntry,
};

/// Hidden first argument that turns the executable into a one-shot diarizer.
pub const DIARIZE_FLAG: &str = "--diarize-meeting";

const MODEL_REPO: &str = "handy-computer/diar_streaming_sortformer_4spk-v2.1-gguf";
const MODEL_FILE: &str = "diar_streaming_sortformer_4spk-v2.1-Q8_0.gguf";

pub const DIARIZATION_JSON: &str = "diarization.json";
pub const FINAL_MD: &str = "transcript-final.md";
pub const FINAL_SRT: &str = "transcript.srt";
/// Speaker-labelled entries, the input for later (manual) AI post-processing.
pub const FINAL_JSONL: &str = "transcript-final.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub t0_ms: i64,
    pub t1_ms: i64,
    pub speaker_id: i32,
}

pub fn is_diarize_invocation() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == DIARIZE_FLAG)
}

/// Child-process entry: `handy --diarize-meeting <model.gguf> <meeting dir>`.
pub fn run_diarize_process() -> i32 {
    let args: Vec<PathBuf> = std::env::args_os().skip(2).map(PathBuf::from).collect();
    let [model, dir] = args.as_slice() else {
        eprintln!("usage: {DIARIZE_FLAG} <model.gguf> <meeting dir>");
        return 2;
    };
    match diarize_system_channel(model, dir) {
        Ok(turns) => {
            eprintln!("Diarization found {} speaker turns", turns.len());
            0
        }
        Err(e) => {
            eprintln!("Diarization failed: {e:#}");
            1
        }
    }
}

fn diarize_system_channel(model_path: &Path, dir: &Path) -> Result<Vec<SpeakerTurn>> {
    use transcribe_cpp::{
        Diarize, Model, ModelOptions, RunExtension, RunOptions, SortformerPreset,
        SortformerStreamOptions,
    };

    let mut pcm = Vec::new();
    for chunk in ChunkedWavWriter::chunk_paths(dir, Channel::System.wav_prefix())? {
        pcm.extend(crate::audio_toolkit::read_wav_samples(&chunk)?);
    }
    if pcm.is_empty() {
        bail!("no system-audio chunks in {}", dir.display());
    }

    transcribe_cpp::init_backends_default().context("initializing compute backends")?;
    let model = Model::load_with(model_path, &ModelOptions::default())
        .map_err(|e| anyhow!("loading {}: {e}", model_path.display()))?;
    let mut session = model.session().map_err(|e| anyhow!("{e}"))?;
    let options = RunOptions {
        diarize: Diarize::On,
        // Accuracy over latency: the whole recording is already available.
        family: Some(RunExtension::Sortformer(SortformerStreamOptions {
            preset: Some(SortformerPreset::VeryHighLatency),
        })),
        ..Default::default()
    };
    let transcript = session.run(&pcm, &options).map_err(|e| anyhow!("{e}"))?;
    let turns: Vec<SpeakerTurn> = transcript
        .speaker_segments
        .iter()
        .map(|s| SpeakerTurn {
            t0_ms: s.t0_ms,
            t1_ms: s.t1_ms,
            speaker_id: s.speaker_id,
        })
        .collect();
    std::fs::write(dir.join(DIARIZATION_JSON), serde_json::to_vec(&turns)?)?;
    Ok(turns)
}

/// Path of the Sortformer GGUF, downloading it into the Hugging Face cache
/// (shared with Handy's other GGUF models) on first use.
pub fn ensure_model() -> Result<PathBuf> {
    let repo = Repo::new(MODEL_REPO.to_string(), RepoType::Model);
    if let Some(path) = Cache::from_env().repo(repo.clone()).get(MODEL_FILE) {
        return Ok(path);
    }
    info!("Downloading diarization model {MODEL_REPO}/{MODEL_FILE}");
    tauri::async_runtime::block_on(async {
        let api = ApiBuilder::from_env()
            .with_token(None)
            .with_progress(false)
            .build()?;
        api.repo(repo).get(MODEL_FILE).await
    })
    .context("downloading the diarization model")
}

/// Run the diarizer child process and write `transcript-final.md`.
pub fn diarize_meeting(dir: &Path) -> Result<PathBuf> {
    let model = ensure_model()?;
    let exe = std::env::current_exe()?;
    let output = Command::new(exe)
        .arg(DIARIZE_FLAG)
        .arg(&model)
        .arg(dir)
        .output()
        .context("starting the diarization process")?;
    let log = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!("diarization process failed: {}", log.trim());
    }
    info!("{}", log.trim());

    let turns: Vec<SpeakerTurn> =
        serde_json::from_slice(&std::fs::read(dir.join(DIARIZATION_JSON))?)?;
    let mut entries = read_transcript(dir)?;
    assign_speakers(&mut entries, &turns);
    write_outputs(dir, &entries)
}

/// Write the final files without speaker separation (diarization
/// unavailable), so every meeting still ends with a sorted transcript.
pub fn write_undiarized(dir: &Path) -> Result<PathBuf> {
    let mut entries = read_transcript(dir)?;
    entries.sort_by_key(|e| e.start_ms);
    write_outputs(dir, &entries)
}

/// Write `transcript-final.md`, `transcript.srt` and `transcript-final.jsonl`
/// from chronologically sorted entries; returns the markdown path.
fn write_outputs(dir: &Path, entries: &[TranscriptEntry]) -> Result<PathBuf> {
    let jsonl: String = entries
        .iter()
        .map(|e| serde_json::to_string(e).map(|line| line + "\n"))
        .collect::<Result<_, _>>()?;
    std::fs::write(dir.join(FINAL_JSONL), jsonl)?;
    std::fs::write(dir.join(FINAL_SRT), render_srt(entries))?;
    let path = dir.join(FINAL_MD);
    std::fs::write(&path, render_final(&title_from_dir(dir), entries))?;
    Ok(path)
}

pub(crate) fn title_from_dir(dir: &Path) -> String {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    format!("Reunião {name}")
}

/// Label each system-channel entry with the Sortformer speaker that overlaps
/// it most. Speakers are numbered by first appearance ("Pessoa 1", ...).
pub fn assign_speakers(entries: &mut [TranscriptEntry], turns: &[SpeakerTurn]) {
    entries.sort_by_key(|e| e.start_ms);
    let mut names: HashMap<i32, String> = HashMap::new();
    for entry in entries.iter_mut().filter(|e| e.channel == Channel::System) {
        let (start, end) = (entry.start_ms as i64, entry.end_ms as i64);
        let mut overlap: HashMap<i32, i64> = HashMap::new();
        for turn in turns {
            let shared = end.min(turn.t1_ms) - start.max(turn.t0_ms);
            if shared > 0 {
                *overlap.entry(turn.speaker_id).or_default() += shared;
            }
        }
        // Ties go to the lower id so the result is deterministic.
        if let Some((&speaker, _)) = overlap
            .iter()
            .max_by_key(|(&id, &shared)| (shared, std::cmp::Reverse(id)))
        {
            let next = names.len() + 1;
            entry.speaker = names
                .entry(speaker)
                .or_insert_with(|| format!("Pessoa {next}"))
                .clone();
        }
    }
}

/// Chronological transcript; consecutive lines of one speaker form a paragraph.
pub fn render_final(title: &str, entries: &[TranscriptEntry]) -> String {
    let mut sorted: Vec<&TranscriptEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.start_ms);
    let mut out = format!("# {title}");
    let mut last_speaker: Option<&str> = None;
    for entry in sorted {
        if last_speaker == Some(entry.speaker.as_str()) {
            out.push(' ');
        } else {
            out.push_str(&format!(
                "\n\n[{}] **{}:** ",
                format_timestamp(entry.start_ms),
                entry.speaker
            ));
            last_speaker = Some(&entry.speaker);
        }
        out.push_str(entry.text.trim());
    }
    out.push('\n');
    out
}

/// SubRip subtitles, one cue per utterance, prefixed with the speaker.
pub fn render_srt(entries: &[TranscriptEntry]) -> String {
    fn stamp(ms: u64) -> String {
        format!("{},{:03}", format_timestamp(ms), ms % 1000)
    }
    let mut sorted: Vec<&TranscriptEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.start_ms);
    sorted
        .iter()
        .enumerate()
        .map(|(i, e)| {
            format!(
                "{}\n{} --> {}\n{}: {}\n",
                i + 1,
                stamp(e.start_ms),
                stamp(e.end_ms),
                e.speaker,
                e.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(start_ms: u64, end_ms: u64, channel: Channel, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            start_ms,
            end_ms,
            channel,
            speaker: channel.default_speaker().to_string(),
            text: text.to_string(),
        }
    }

    fn turn(t0_ms: i64, t1_ms: i64, speaker_id: i32) -> SpeakerTurn {
        SpeakerTurn {
            t0_ms,
            t1_ms,
            speaker_id,
        }
    }

    #[test]
    fn system_lines_get_the_most_overlapping_speaker_numbered_by_appearance() {
        let mut entries = vec![
            entry(5_000, 7_000, Channel::System, "c"),
            entry(0, 2_000, Channel::System, "a"),
            entry(2_500, 4_000, Channel::Mic, "b"),
            entry(8_000, 9_000, Channel::System, "d"),
        ];
        let turns = vec![
            turn(0, 2_100, 3),
            turn(4_900, 6_000, 1),
            turn(6_000, 9_500, 3),
        ];
        assign_speakers(&mut entries, &turns);
        let speakers: Vec<&str> = entries.iter().map(|e| e.speaker.as_str()).collect();
        // "c" overlaps speaker 1 for 1.0 s and speaker 3 for 1.0 s: tie -> id 1.
        assert_eq!(speakers, vec!["Pessoa 1", "Eu", "Pessoa 2", "Pessoa 1"]);
    }

    #[test]
    fn lines_without_overlap_keep_the_channel_label() {
        let mut entries = vec![entry(0, 1_000, Channel::System, "a")];
        assign_speakers(&mut entries, &[turn(5_000, 6_000, 0)]);
        assert_eq!(entries[0].speaker, "Outros");
    }

    #[test]
    fn srt_cues_are_numbered_and_timed() {
        let entries = vec![
            entry(3_725_250, 3_727_000, Channel::System, "Tudo."),
            entry(1_500, 2_040, Channel::Mic, "Oi."),
        ];
        assert_eq!(
            render_srt(&entries),
            "1\n00:00:01,500 --> 00:00:02,040\nEu: Oi.\n\n\
             2\n01:02:05,250 --> 01:02:07,000\nOutros: Tudo.\n"
        );
    }

    #[test]
    fn render_merges_consecutive_lines_of_a_speaker() {
        let entries = vec![
            entry(61_000, 62_000, Channel::Mic, "Oi."),
            entry(62_500, 63_000, Channel::Mic, "Tudo bem?"),
            entry(64_000, 65_000, Channel::System, "Tudo."),
        ];
        let out = render_final("R", &entries);
        assert_eq!(
            out,
            "# R\n\n[00:01:01] **Eu:** Oi. Tudo bem?\n\n[00:01:04] **Outros:** Tudo.\n"
        );
    }
}
