//! Capture through cpal (WASAPI on Windows, Core Audio on macOS), turned into
//! the blocking 16 kHz mono reads meeting mode expects.
//!
//! The real-time callback only downmixes into a lock-free ring; the capture
//! thread drains it, resamples and hands out 100 ms blocks.

use std::any::Any;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use rtrb::{Consumer, Producer, RingBuffer};

use super::{AudioSource, READ_SAMPLES};
use crate::audio_toolkit::audio::FrameResampler;
use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;
use crate::audio_toolkit::get_cpal_host;

/// Seconds of audio the ring holds while the capture thread is busy.
const RING_SECONDS: usize = 5;
const BLOCK: Duration = Duration::from_millis(100);
/// A loopback stream delivers nothing while nothing is playing. Past this
/// long without audio the gap is filled with silence, so the system channel
/// keeps the same timeline as the microphone and `read` keeps returning
/// (the capture loop checks for stop between reads).
const STARVED_AFTER: Duration = Duration::from_millis(300);
const POLL: Duration = Duration::from_millis(10);

pub struct CpalSource {
    // Dropped first: the stream must stop before whatever it records from
    // (`keep_alive`) goes away.
    _stream: cpal::Stream,
    blocks: Blocks,
    _keep_alive: Option<Box<dyn Any>>,
}

impl CpalSource {
    /// The system's default microphone.
    pub fn default_input() -> Result<Self> {
        let device = get_cpal_host()
            .default_input_device()
            .ok_or_else(|| anyhow!("No microphone found"))?;
        let config = device
            .default_input_config()
            .context("Reading the microphone format")?;
        Self::open(&device, config, None)
    }

    /// What the default output device is playing. WASAPI turns an input
    /// stream on a render device into a loopback capture; other cpal hosts
    /// do not support this.
    pub fn output_loopback() -> Result<Self> {
        let device = get_cpal_host()
            .default_output_device()
            .ok_or_else(|| anyhow!("No audio output device found"))?;
        let config = device
            .default_output_config()
            .context("Reading the output device format")?;
        Self::open(&device, config, None)
    }

    /// The input device called `name`, if the system lists it.
    pub fn find_input(name: &str) -> Result<Option<cpal::Device>> {
        Ok(get_cpal_host()
            .input_devices()
            .context("Listing audio input devices")?
            .find(|device| device.name().is_ok_and(|n| n == name)))
    }

    /// Record from `device`, keeping `keep_alive` (e.g. the virtual device it
    /// belongs to) until the stream is closed.
    pub fn open_input(device: &cpal::Device, keep_alive: Box<dyn Any>) -> Result<Self> {
        let config = device
            .default_input_config()
            .context("Reading the audio device format")?;
        Self::open(device, config, Some(keep_alive))
    }

    fn open(
        device: &cpal::Device,
        config: cpal::SupportedStreamConfig,
        keep_alive: Option<Box<dyn Any>>,
    ) -> Result<Self> {
        let rate = config.sample_rate().0 as usize;
        let channels = (config.channels() as usize).max(1);
        let (producer, consumer) = RingBuffer::new(rate * RING_SECONDS);
        let failed = Arc::new(AtomicBool::new(false));

        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(device, &config, channels, producer, &failed),
            SampleFormat::F64 => build::<f64>(device, &config, channels, producer, &failed),
            SampleFormat::I16 => build::<i16>(device, &config, channels, producer, &failed),
            SampleFormat::I32 => build::<i32>(device, &config, channels, producer, &failed),
            SampleFormat::I8 => build::<i8>(device, &config, channels, producer, &failed),
            SampleFormat::U16 => build::<u16>(device, &config, channels, producer, &failed),
            SampleFormat::U8 => build::<u8>(device, &config, channels, producer, &failed),
            other => bail!("Unsupported audio sample format {other:?}"),
        }?;
        stream.play().context("Starting audio capture")?;

        Ok(Self {
            _stream: stream,
            blocks: Blocks::new(consumer, failed, rate, STARVED_AFTER),
            _keep_alive: keep_alive,
        })
    }
}

impl AudioSource for CpalSource {
    fn read(&mut self, out: &mut Vec<f32>) -> Result<()> {
        self.blocks.next(out)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    channels: usize,
    mut producer: Producer<f32>,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let failed = Arc::clone(failed);
    device
        .build_input_stream(
            &config.config(),
            move |data: &[T], _: &cpal::InputCallbackInfo| push_mono(data, channels, &mut producer),
            // Runs on the audio thread: just flag it, `read` reports it.
            move |_err| failed.store(true, Ordering::Release),
            None,
        )
        .context("Opening the audio stream")
}

/// Real-time callback body: downmix interleaved frames into the ring,
/// dropping what does not fit. No allocation, locks or logging.
fn push_mono<T>(data: &[T], channels: usize, producer: &mut Producer<f32>)
where
    T: Sample,
    f32: FromSample<T>,
{
    let frames = (data.len() / channels).min(producer.slots());
    if frames == 0 {
        return;
    }
    if let Ok(chunk) = producer.write_chunk_uninit(frames) {
        chunk.fill_from_iter(data.chunks_exact(channels).take(frames).map(|frame| {
            frame.iter().map(|&s| s.to_sample::<f32>()).sum::<f32>() / channels as f32
        }));
    }
}

/// Capture-thread side: drains the ring into 100 ms blocks at 16 kHz.
struct Blocks {
    samples: Consumer<f32>,
    failed: Arc<AtomicBool>,
    resampler: FrameResampler,
    ready: VecDeque<Vec<f32>>,
    /// When audio last arrived, or up to where silence was already filled in.
    idle_since: Instant,
    starved_after: Duration,
}

impl Blocks {
    fn new(
        samples: Consumer<f32>,
        failed: Arc<AtomicBool>,
        rate: usize,
        starved_after: Duration,
    ) -> Self {
        Self {
            samples,
            failed,
            resampler: FrameResampler::new(rate, WHISPER_SAMPLE_RATE as usize, BLOCK),
            ready: VecDeque::new(),
            idle_since: Instant::now(),
            starved_after,
        }
    }

    fn next(&mut self, out: &mut Vec<f32>) -> Result<()> {
        loop {
            if let Some(block) = self.ready.pop_front() {
                out.clear();
                out.extend_from_slice(&block);
                return Ok(());
            }
            if self.failed.load(Ordering::Acquire) {
                bail!("The audio device stopped (disconnected or changed)");
            }

            let available = self.samples.slots();
            if available > 0 {
                let chunk = self
                    .samples
                    .read_chunk(available)
                    .expect("the consumer just reported this many readable slots");
                let (first, second) = chunk.as_slices();
                let ready = &mut self.ready;
                self.resampler.push(first, |b| ready.push_back(b.to_vec()));
                self.resampler.push(second, |b| ready.push_back(b.to_vec()));
                chunk.commit_all();
                self.idle_since = Instant::now();
                continue;
            }

            let idle = self.idle_since.elapsed();
            if idle >= self.starved_after {
                let missing = (idle.as_millis() / BLOCK.as_millis()) as u32;
                for _ in 0..missing {
                    self.ready.push_back(vec![0.0; READ_SAMPLES]);
                }
                self.idle_since += BLOCK * missing;
                continue;
            }
            thread::sleep(POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(rate: usize, starved_after: Duration) -> (Producer<f32>, Blocks) {
        let (producer, consumer) = RingBuffer::new(rate * RING_SECONDS);
        let failed = Arc::new(AtomicBool::new(false));
        (producer, Blocks::new(consumer, failed, rate, starved_after))
    }

    #[test]
    fn downmixes_interleaved_frames() {
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(16);
        push_mono(&[1.0f32, 0.0, 0.5, 0.5, -1.0, -1.0], 2, &mut producer);
        let got: Vec<f32> = std::iter::from_fn(|| consumer.pop().ok()).collect();
        assert_eq!(got, vec![0.5, 0.5, -1.0]);
    }

    #[test]
    fn converts_integer_samples() {
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(4);
        push_mono(&[i16::MAX, i16::MIN], 1, &mut producer);
        assert!((consumer.pop().unwrap() - 1.0).abs() < 1e-3);
        assert!((consumer.pop().unwrap() + 1.0).abs() < 1e-3);
    }

    #[test]
    fn drops_what_does_not_fit_without_panicking() {
        let (mut producer, consumer) = RingBuffer::<f32>::new(2);
        push_mono(&[0.1f32, 0.2, 0.3], 1, &mut producer);
        assert_eq!(consumer.slots(), 2);
    }

    #[test]
    fn resamples_48k_to_100ms_blocks_at_16k() {
        let (mut producer, mut blocks) = blocks(48_000, Duration::from_secs(60));
        let second = vec![0.25f32; 48_000];
        push_mono(&second, 1, &mut producer);
        let mut out = Vec::new();
        for _ in 0..9 {
            blocks.next(&mut out).unwrap();
            assert_eq!(out.len(), READ_SAMPLES);
        }
    }

    #[test]
    fn fills_silence_while_nothing_plays() {
        let (_producer, mut blocks) = blocks(16_000, Duration::from_millis(30));
        let started = Instant::now();
        let mut out = vec![1.0];
        blocks.next(&mut out).unwrap();
        assert_eq!(out.len(), READ_SAMPLES);
        assert!(out.iter().all(|&s| s == 0.0));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn reports_a_failed_device() {
        let (_producer, mut blocks) = blocks(16_000, Duration::from_secs(60));
        blocks.failed.store(true, Ordering::Release);
        assert!(blocks.next(&mut Vec::new()).is_err());
    }
}
