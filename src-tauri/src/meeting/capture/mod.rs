//! Meeting audio capture: the user's microphone and what the computer plays
//! (the other participants), each as a blocking stream of 16 kHz mono f32.
//!
//! One backend per platform:
//! - Linux: PulseAudio/PipeWire directly (`pulse`), since cpal goes through
//!   ALSA and cannot see sink monitors.
//! - Windows: cpal on WASAPI; system audio is a loopback stream on the default
//!   output device.
//! - macOS: cpal for the microphone; system audio through a Core Audio process
//!   tap wrapped in a private aggregate device (`macos`, macOS 14.2+).

use anyhow::Result;

use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;

// Built on every platform so Linux CI type-checks the code Windows and macOS
// run; only those two use it.
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod cpal_source;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod pulse;

/// Samples returned per read: 100 ms keeps the loop cheap and stop responsive.
pub const READ_SAMPLES: usize = WHISPER_SAMPLE_RATE as usize / 10;

/// A capture stream opened on (and read from) the capture thread.
pub trait AudioSource {
    /// Block until the next block of about `READ_SAMPLES` samples (16 kHz
    /// mono) is captured, replacing the contents of `out`.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<()>;
}

/// The user's default microphone.
pub fn open_microphone(stream_name: &str) -> Result<Box<dyn AudioSource>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(pulse::PulseSource::open(
            pulse::DEFAULT_SOURCE,
            stream_name,
        )?))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream_name;
        Ok(Box::new(cpal_source::CpalSource::default_input()?))
    }
}

/// Everything the computer is playing: the other meeting participants.
pub fn open_system_audio(stream_name: &str) -> Result<Box<dyn AudioSource>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(pulse::PulseSource::open(
            pulse::DEFAULT_MONITOR,
            stream_name,
        )?))
    }
    #[cfg(target_os = "windows")]
    {
        let _ = stream_name;
        Ok(Box::new(cpal_source::CpalSource::output_loopback()?))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = stream_name;
        macos::open_system_audio()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = stream_name;
        anyhow::bail!("System audio capture is not supported on this platform")
    }
}
