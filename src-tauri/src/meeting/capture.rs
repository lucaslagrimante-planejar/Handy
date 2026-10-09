//! Blocking capture of a PulseAudio/PipeWire source as 16 kHz mono f32.
//!
//! The regular recorder goes through ALSA via cpal, which cannot see sink
//! monitors, so meeting mode talks to the Pulse API directly. The server does
//! the resampling and downmixing to the format the VAD and models expect.

use anyhow::{anyhow, Result};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;

/// What the computer is playing (the other meeting participants).
pub const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";
/// The user's default microphone.
pub const DEFAULT_SOURCE: &str = "@DEFAULT_SOURCE@";

/// Samples returned per read: 100 ms keeps the loop cheap and stop responsive.
pub const READ_SAMPLES: usize = WHISPER_SAMPLE_RATE as usize / 10;

pub struct PulseSource {
    stream: Simple,
    bytes: Vec<u8>,
}

impl PulseSource {
    pub fn open(device: &str, stream_name: &str) -> Result<Self> {
        let spec = Spec {
            format: Format::F32le,
            channels: 1,
            rate: WHISPER_SAMPLE_RATE,
        };
        let fragment_bytes = (READ_SAMPLES * std::mem::size_of::<f32>()) as u32;
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: fragment_bytes,
        };
        let stream = Simple::new(
            None,
            "Handy",
            Direction::Record,
            Some(device),
            stream_name,
            &spec,
            None,
            Some(&attr),
        )
        .map_err(|e| anyhow!("Failed to open audio source {device}: {e}"))?;
        Ok(Self {
            stream,
            bytes: vec![0; fragment_bytes as usize],
        })
    }

    /// Block until the next `READ_SAMPLES` samples are captured.
    pub fn read(&mut self, out: &mut Vec<f32>) -> Result<()> {
        self.stream
            .read(&mut self.bytes)
            .map_err(|e| anyhow!("Audio capture read failed: {e}"))?;
        out.clear();
        out.extend(
            self.bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b)),
        );
        Ok(())
    }
}
