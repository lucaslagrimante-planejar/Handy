//! Crash-safe meeting output: audio is split into fixed-length WAV chunks
//! whose headers are refreshed every few seconds, and every transcript line is
//! appended and synced as soon as it is produced.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hound::{WavSpec, WavWriter};
use serde::{Deserialize, Serialize};

use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;

pub const TRANSCRIPT_JSONL: &str = "transcript.jsonl";
pub const TRANSCRIPT_MD: &str = "transcript.md";

/// Rewrite the WAV header this often so a crash loses at most this much audio.
const FLUSH_EVERY_SAMPLES: usize = WHISPER_SAMPLE_RATE as usize * 5;

const WAV_SPEC: WavSpec = WavSpec {
    channels: 1,
    sample_rate: WHISPER_SAMPLE_RATE,
    bits_per_sample: 16,
    sample_format: hound::SampleFormat::Int,
};

/// Writes `<prefix>-001.wav`, `<prefix>-002.wav`, ... of `chunk_seconds` each.
pub struct ChunkedWavWriter {
    dir: PathBuf,
    prefix: &'static str,
    chunk_samples: usize,
    index: usize,
    in_chunk: usize,
    since_flush: usize,
    writer: Option<WavWriter<BufWriter<File>>>,
}

impl ChunkedWavWriter {
    pub fn new(dir: &Path, prefix: &'static str, chunk_seconds: u32) -> Self {
        Self {
            dir: dir.to_path_buf(),
            prefix,
            chunk_samples: (chunk_seconds * WHISPER_SAMPLE_RATE) as usize,
            index: 0,
            in_chunk: 0,
            since_flush: 0,
            writer: None,
        }
    }

    /// Sorted chunk files of `prefix` in `dir`.
    pub fn chunk_paths(dir: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "wav")
                    && path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .and_then(|stem| stem.strip_prefix(prefix))
                        .is_some_and(|rest| rest.starts_with('-'))
            })
            .collect();
        paths.sort();
        Ok(paths)
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        for &sample in samples {
            if self.writer.is_none() {
                self.index += 1;
                let path = self
                    .dir
                    .join(format!("{}-{:03}.wav", self.prefix, self.index));
                self.writer = Some(
                    WavWriter::create(&path, WAV_SPEC)
                        .with_context(|| format!("creating {}", path.display()))?,
                );
                self.in_chunk = 0;
            }
            let writer = self.writer.as_mut().expect("opened above");
            writer.write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
            self.in_chunk += 1;
            self.since_flush += 1;

            if self.in_chunk >= self.chunk_samples {
                self.finish()?;
            } else if self.since_flush >= FLUSH_EVERY_SAMPLES {
                writer.flush()?;
                self.since_flush = 0;
            }
        }
        Ok(())
    }

    /// Finalize the open chunk; the next write starts a new one.
    pub fn finish(&mut self) -> Result<()> {
        if let Some(writer) = self.writer.take() {
            writer.finalize()?;
        }
        self.since_flush = 0;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// The user's microphone.
    Mic,
    /// Everything the computer plays: the other participants.
    System,
}

impl Channel {
    pub fn wav_prefix(self) -> &'static str {
        match self {
            Channel::Mic => "eu",
            Channel::System => "outros",
        }
    }

    pub fn default_speaker(self) -> &'static str {
        match self {
            Channel::Mic => "Eu",
            Channel::System => "Outros",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub start_ms: u64,
    pub end_ms: u64,
    pub channel: Channel,
    pub speaker: String,
    pub text: String,
}

pub fn format_timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

/// Appends each line to `transcript.jsonl` (machine-readable, used by the
/// diarization pass) and `transcript.md` (readable while the meeting runs).
pub struct TranscriptWriter {
    jsonl: File,
    md: File,
}

impl TranscriptWriter {
    pub fn create(dir: &Path, title: &str) -> Result<Self> {
        let open = |name: &str| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(name))
                .with_context(|| format!("opening {name}"))
        };
        let mut md = open(TRANSCRIPT_MD)?;
        writeln!(md, "# {title}\n")?;
        Ok(Self {
            jsonl: open(TRANSCRIPT_JSONL)?,
            md,
        })
    }

    pub fn append(&mut self, entry: &TranscriptEntry) -> Result<()> {
        writeln!(self.jsonl, "{}", serde_json::to_string(entry)?)?;
        writeln!(
            self.md,
            "[{}] **{}:** {}\n",
            format_timestamp(entry.start_ms),
            entry.speaker,
            entry.text
        )?;
        self.jsonl.sync_data()?;
        self.md.sync_data()?;
        Ok(())
    }
}

pub fn read_transcript(dir: &Path) -> Result<Vec<TranscriptEntry>> {
    let content = std::fs::read_to_string(dir.join(TRANSCRIPT_JSONL))?;
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("parsing transcript line"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_roll_over_and_stay_readable() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = ChunkedWavWriter::new(dir.path(), "outros", 1);
        writer.write(&vec![0.25; 40_000]).unwrap();
        writer.finish().unwrap();

        let paths = ChunkedWavWriter::chunk_paths(dir.path(), "outros").unwrap();
        let lengths: Vec<u32> = paths
            .iter()
            .map(|p| hound::WavReader::open(p).unwrap().duration())
            .collect();
        assert_eq!(lengths, vec![16_000, 16_000, 8_000]);
        assert!(ChunkedWavWriter::chunk_paths(dir.path(), "eu")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn transcript_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = TranscriptWriter::create(dir.path(), "Reunião").unwrap();
        writer
            .append(&TranscriptEntry {
                start_ms: 3_723_000,
                end_ms: 3_725_000,
                channel: Channel::System,
                speaker: "Outros".into(),
                text: "Bom dia".into(),
            })
            .unwrap();
        let entries = read_transcript(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].channel, Channel::System);
        let md = std::fs::read_to_string(dir.path().join(TRANSCRIPT_MD)).unwrap();
        assert!(md.contains("[01:02:03] **Outros:** Bom dia"));
    }
}
