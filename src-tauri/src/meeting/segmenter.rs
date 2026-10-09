//! Splits one continuous 16 kHz channel into utterances with a smoothed VAD,
//! so each utterance can be transcribed as soon as its speaker pauses.

use log::warn;

use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;
use crate::audio_toolkit::vad::{VadFrame, VoiceActivityDetector};

const SAMPLES_PER_MS: u64 = WHISPER_SAMPLE_RATE as u64 / 1000;

/// Utterances shorter than this are VAD blips (clicks, breaths), not speech.
const MIN_UTTERANCE_MS: u64 = 300;

#[derive(Debug)]
pub struct Utterance {
    /// Offset of the first sample from the start of the channel.
    pub start_ms: u64,
    pub end_ms: u64,
    pub samples: Vec<f32>,
}

pub struct Segmenter {
    vad: Box<dyn VoiceActivityDetector>,
    frame_samples: usize,
    max_samples: usize,
    /// Input not yet long enough to fill a VAD frame.
    pending: Vec<f32>,
    /// Samples already pushed through the VAD.
    consumed: u64,
    current: Vec<f32>,
    current_start: u64,
}

impl Segmenter {
    /// `max_utterance_ms` force-splits long monologues so the transcript keeps
    /// moving and a single run never gets an unbounded buffer.
    pub fn new(vad: Box<dyn VoiceActivityDetector>, max_utterance_ms: u64) -> Self {
        let frame_samples = vad.frame_samples();
        Self {
            vad,
            frame_samples,
            max_samples: (max_utterance_ms * SAMPLES_PER_MS) as usize,
            pending: Vec::with_capacity(frame_samples * 2),
            consumed: 0,
            current: Vec::new(),
            current_start: 0,
        }
    }

    /// Feed captured samples; returns every utterance closed by this input.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Utterance> {
        self.pending.extend_from_slice(samples);
        let mut closed = Vec::new();
        let mut offset = 0;
        while self.pending.len() - offset >= self.frame_samples {
            let frame_end = offset + self.frame_samples;
            let frame_start_sample = self.consumed;
            self.consumed += self.frame_samples as u64;

            let speech = match self.vad.push_frame(&self.pending[offset..frame_end]) {
                Ok(VadFrame::Speech(audio)) => Some(audio.to_vec()),
                Ok(VadFrame::Noise) => None,
                Err(e) => {
                    warn!("Meeting VAD failed on a frame, treating it as silence: {e}");
                    None
                }
            };
            offset = frame_end;

            match speech {
                Some(audio) => {
                    if self.current.is_empty() {
                        // On onset the VAD hands back its pre-roll along with
                        // this frame, so the utterance starts before it.
                        let preroll = audio.len().saturating_sub(self.frame_samples) as u64;
                        self.current_start = frame_start_sample.saturating_sub(preroll);
                    }
                    self.current.extend_from_slice(&audio);
                    if self.current.len() >= self.max_samples {
                        closed.extend(self.take_current());
                    }
                }
                None => closed.extend(self.take_current()),
            }
        }
        self.pending.drain(..offset);
        closed
    }

    /// Close the utterance in progress, e.g. when the meeting stops mid-sentence.
    pub fn flush(&mut self) -> Option<Utterance> {
        self.take_current()
    }

    fn take_current(&mut self) -> Option<Utterance> {
        if self.current.is_empty() {
            return None;
        }
        let samples = std::mem::take(&mut self.current);
        let start_ms = self.current_start / SAMPLES_PER_MS;
        let end_ms = (self.current_start + samples.len() as u64) / SAMPLES_PER_MS;
        self.current_start += samples.len() as u64;
        (end_ms - start_ms >= MIN_UTTERANCE_MS).then_some(Utterance {
            start_ms,
            end_ms,
            samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    /// Treats any frame whose first sample is non-zero as speech.
    struct ThresholdVad;

    impl VoiceActivityDetector for ThresholdVad {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
            Ok(if frame[0] != 0.0 {
                VadFrame::Speech(frame)
            } else {
                VadFrame::Noise
            })
        }

        fn frame_samples(&self) -> usize {
            160 // 10 ms
        }
    }

    fn ms(len: u64, value: f32) -> Vec<f32> {
        vec![value; (len * SAMPLES_PER_MS) as usize]
    }

    #[test]
    fn speech_between_silences_becomes_one_timed_utterance() {
        let mut seg = Segmenter::new(Box::new(ThresholdVad), 20_000);
        let mut input = ms(1000, 0.0);
        input.extend(ms(500, 0.5));
        input.extend(ms(200, 0.0));

        let out = seg.push(&input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start_ms, 1000);
        assert_eq!(out[0].end_ms, 1500);
        assert!(seg.flush().is_none());
    }

    #[test]
    fn input_split_across_pushes_is_reassembled() {
        let mut seg = Segmenter::new(Box::new(ThresholdVad), 20_000);
        let speech = ms(400, 0.5);
        assert!(seg.push(&speech[..1234]).is_empty());
        assert!(seg.push(&speech[1234..]).is_empty());
        let out = seg.push(&ms(20, 0.0));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].samples.len(), speech.len());
    }

    #[test]
    fn long_speech_is_force_split_and_contiguous() {
        let mut seg = Segmenter::new(Box::new(ThresholdVad), 1000);
        let mut out = seg.push(&ms(2500, 0.5));
        out.extend(seg.flush());
        let spans: Vec<_> = out.iter().map(|u| (u.start_ms, u.end_ms)).collect();
        assert_eq!(spans, vec![(0, 1000), (1000, 2000), (2000, 2500)]);
    }

    #[test]
    fn blips_shorter_than_minimum_are_dropped() {
        let mut seg = Segmenter::new(Box::new(ThresholdVad), 20_000);
        let mut input = ms(100, 0.5);
        input.extend(ms(50, 0.0));
        assert!(seg.push(&input).is_empty());
    }
}
