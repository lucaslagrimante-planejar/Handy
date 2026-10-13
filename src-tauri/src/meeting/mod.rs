//! Meeting mode: records the microphone ("Eu") and the system audio (the
//! other participants) as separate channels, transcribes each utterance as
//! soon as its speaker pauses, and keeps everything crash-safe on disk:
//!
//! ```text
//! <app data>/meetings/<YYYY-MM-DD_HH-MM-SS>/
//!   eu-001.wav, eu-002.wav, ...         microphone, 5-minute chunks
//!   outros-001.wav, ...                 system audio, 5-minute chunks
//!   transcript.md / transcript.jsonl    live transcript, one line per utterance
//!   transcript-final.md                 after stop: sorted, speakers separated
//! ```
//!
//! When the meeting stops, Sortformer splits the system channel into
//! "Pessoa 1..4" (see [`diarize`]).
//!
//! A meeting starts from its own shortcut, from the overlay button that
//! converts the dictation in progress (its audio becomes the meeting's first
//! seconds), or from the invite shown when a call is detected ([`detect`]).
//! While it runs, the transcribe shortcut ends it.

pub mod diarize;
pub mod history;
mod segmenter;
pub mod summary;
mod writer;

mod capture;
mod detect;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use log::{error, info, warn};
use tauri::{AppHandle, Manager};

use crate::managers::audio::AudioRecordingManager;
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::overlay;
use crate::tray::{set_tray_state, TrayIconState};
use segmenter::Utterance;
use writer::{Channel, TranscriptEntry, TranscriptWriter};

const CHUNK_SECONDS: u32 = 5 * 60;
const MAX_UTTERANCE_MS: u64 = 20_000;
/// Longer than dictation's tail: a short breath mid-sentence should not split
/// a meeting line.
const MEETING_HANGOVER_MS: u64 = 800;
const SILERO_THRESHOLD: f32 = 0.3;

/// Model for meetings, separate from the dictation model: best accuracy on
/// spontaneous pt-BR speech among the catalog models (CORAA-MUPE sample: 15.0%
/// WER vs 16-23% for the others) and also the fastest.
const MEETING_MODEL: &str =
    "handy-computer/parakeet-tdt-0.6b-v3-gguf/parakeet-tdt-0.6b-v3-Q8_0.gguf";

/// Event for the History tab: a meeting started, finished or changed files.
pub const MEETINGS_CHANGED: &str = "meetings-changed";

/// Meetings whose transcript is still being written: recording, or stopped
/// and draining / separating speakers.
static IN_PROGRESS: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

fn set_in_progress(dir: &Path, busy: bool) {
    let mut set = IN_PROGRESS.lock().unwrap();
    let set = set.get_or_insert_with(HashSet::new);
    if busy {
        set.insert(dir.to_path_buf());
    } else {
        set.remove(dir);
    }
}

pub fn is_in_progress(dir: &Path) -> bool {
    IN_PROGRESS
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|set| set.contains(dir))
}

/// Tell the History tab and the tray menu that the meetings changed.
pub fn meetings_changed(app: &AppHandle) {
    use tauri::Emitter;
    let _ = app.emit(MEETINGS_CHANGED, ());
    crate::tray::update_tray_menu(app);
}

pub struct MeetingManager {
    app: AppHandle,
    active: Mutex<Option<ActiveMeeting>>,
    /// Engine running [`MEETING_MODEL`], created on the first meeting and
    /// kept so later meetings reuse its worker process.
    engine: Mutex<Option<Arc<TranscriptionManager>>>,
    /// The user closed the call invite; it stays away until the call ends.
    invite_dismissed: AtomicBool,
}

/// What happens once a meeting stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum StopAction {
    /// Final transcript with speakers separated, then offer the AI step.
    #[default]
    Transcribe,
    /// Final transcript, then the AI step right away.
    Summarize,
    /// Keep the audio and the live transcript as they are; the meeting can
    /// be finished later from the History tab.
    Nothing,
}

struct ActiveMeeting {
    dir: PathBuf,
    stop: Arc<AtomicBool>,
    /// Read by the transcriber once the captures end.
    then: Arc<Mutex<StopAction>>,
    started: Instant,
    /// Dictation audio adopted when the meeting was converted from it.
    lead_in_ms: u64,
}

impl MeetingManager {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            active: Mutex::new(None),
            engine: Mutex::new(None),
            invite_dismissed: AtomicBool::new(false),
        }
    }

    fn meeting_engine(&self) -> Result<Arc<TranscriptionManager>> {
        let mut engine = self.engine.lock().unwrap();
        if let Some(tm) = engine.as_ref() {
            return Ok(tm.clone());
        }
        let models = self.app.state::<Arc<ModelManager>>().inner().clone();
        let tm = Arc::new(TranscriptionManager::new_background(&self.app, models)?);
        *engine = Some(tm.clone());
        Ok(tm)
    }

    pub fn is_active(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    /// Folder of the meeting being recorded.
    pub fn active_dir(&self) -> Option<PathBuf> {
        self.active.lock().unwrap().as_ref().map(|m| m.dir.clone())
    }

    /// Time since the meeting began, including adopted dictation audio.
    pub fn elapsed(&self) -> Option<Duration> {
        self.active
            .lock()
            .unwrap()
            .as_ref()
            .map(|meeting| meeting.started.elapsed() + Duration::from_millis(meeting.lead_in_ms))
    }

    /// Expand the meeting pill into the stop options, or collapse it back.
    pub fn show_stop_menu(&self, open: bool) {
        if !self.is_active() || !overlay::is_overlay_enabled() {
            return;
        }
        if open {
            overlay::show_overlay_state(&self.app, "stopmenu");
        } else {
            overlay::show_meeting_overlay(&self.app);
        }
    }

    /// Meeting shortcut: stop a running meeting, adopt a dictation in
    /// progress, or start a new one.
    pub fn toggle(&self) {
        let result = if self.is_active() {
            self.stop();
            return;
        } else if self
            .app
            .state::<Arc<AudioRecordingManager>>()
            .is_recording()
        {
            self.convert_dictation()
        } else {
            self.start()
        };
        if let Err(e) = result {
            error!("Failed to start meeting: {e:#}");
            notify("Falha ao iniciar a reunião", &format!("{e:#}"));
        }
    }

    pub fn start(&self) -> Result<PathBuf> {
        self.start_with(Vec::new())
    }

    /// Turn the dictation being recorded into a meeting: its audio so far
    /// opens the meeting and nothing is pasted.
    pub fn convert_dictation(&self) -> Result<PathBuf> {
        let samples = self
            .app
            .state::<Arc<AudioRecordingManager>>()
            .take_recording()
            .unwrap_or_default();
        crate::utils::cancel_current_operation(&self.app);
        self.start_with(samples)
    }

    fn start_with(&self, lead_in: Vec<f32>) -> Result<PathBuf> {
        let mut active = self.active.lock().unwrap();
        if let Some(meeting) = active.as_ref() {
            return Ok(meeting.dir.clone());
        }

        let name = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
        let dir = crate::portable::app_data_dir(&self.app)?
            .join("meetings")
            .join(&name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        let engine = self.meeting_engine()?;
        let dictation = self
            .app
            .state::<Arc<TranscriptionManager>>()
            .inner()
            .clone();

        let stop = Arc::new(AtomicBool::new(false));
        let then = Arc::new(Mutex::new(StopAction::default()));
        let lead_in = Arc::new(lead_in);
        let lead_in_ms = samples_to_ms(lead_in.len());
        let meter = Arc::new(Meter::default());
        let (utterances_tx, utterances_rx) = mpsc::channel();
        let vad_model = self
            .app
            .path()
            .resolve(
                "resources/models/silero_vad_v4.onnx",
                tauri::path::BaseDirectory::Resource,
            )
            .context("resolving the VAD model")?;

        for channel in [Channel::Mic, Channel::System] {
            if let Err(e) = start_capture(CaptureJob {
                channel,
                dir: dir.clone(),
                vad_model: vad_model.clone(),
                stop: stop.clone(),
                utterances: utterances_tx.clone(),
                lead_in: lead_in.clone(),
                meter: meter.clone(),
                app: self.app.clone(),
            }) {
                // Release the channel that did open.
                stop.store(true, Ordering::Release);
                return Err(e);
            }
        }
        drop(utterances_tx);

        let title = format!("Reunião {name}");
        let worker_dir = dir.clone();
        let app = self.app.clone();
        let worker_then = then.clone();
        set_in_progress(&dir, true);
        thread::Builder::new()
            .name("meeting-transcriber".into())
            .spawn(move || {
                let model = MeetingModel::pick(engine, dictation);
                run_transcriber(&model, &worker_dir, &title, utterances_rx);
                let then = *worker_then.lock().unwrap();
                if then == StopAction::Nothing {
                    info!("Meeting kept unfinished: {}", worker_dir.display());
                } else {
                    finish_meeting(&app, &worker_dir, then);
                }
                set_in_progress(&worker_dir, false);
                meetings_changed(&app);
            })
            .inspect_err(|_| set_in_progress(&dir, false))?;

        info!("Meeting started: {}", dir.display());
        if overlay::is_overlay_enabled() {
            overlay::show_meeting_overlay(&self.app);
        } else {
            notify(
                "Reunião em gravação",
                "Microfone e áudio do sistema sendo transcritos.",
            );
        }
        set_tray_state(&self.app, TrayIconState::Recording);
        *active = Some(ActiveMeeting {
            dir: dir.clone(),
            stop,
            then,
            started: Instant::now(),
            lead_in_ms,
        });
        drop(active);
        // Meeting mode must leave system audio alone: release the mute a
        // dictation may have applied (also covers a dictation converted
        // into this meeting).
        self.app.state::<Arc<AudioRecordingManager>>().remove_mute();
        meetings_changed(&self.app);
        Ok(dir)
    }

    /// Stop with the default action (shortcut, tray): transcribe.
    pub fn stop(&self) -> Option<PathBuf> {
        self.stop_with(StopAction::Transcribe)
    }

    /// Signal the capture threads to stop. The transcriber drains the queued
    /// utterances, then does `then` in the background.
    pub fn stop_with(&self, then: StopAction) -> Option<PathBuf> {
        let meeting = self.active.lock().unwrap().take()?;
        *meeting.then.lock().unwrap() = then;
        meeting.stop.store(true, Ordering::Release);
        info!("Meeting stopping ({then:?}): {}", meeting.dir.display());
        set_tray_state(&self.app, TrayIconState::Idle);
        if then == StopAction::Nothing {
            overlay::hide_recording_overlay(&self.app);
        } else if overlay::is_overlay_enabled() {
            let chained = then == StopAction::Summarize;
            show_card_step(&self.app, "finishing", &meeting.dir, None, chained);
        } else {
            overlay::hide_recording_overlay(&self.app);
            notify(
                "Reunião encerrada",
                "Finalizando a transcrição e separando os falantes...",
            );
        }
        meetings_changed(&self.app);
        Some(meeting.dir)
    }

    /// The user closed the call invite or the post-meeting card. A dismissed
    /// invite is not shown again during the same call.
    pub fn dismiss_invite(&self) {
        match overlay::current_overlay_state().as_deref() {
            Some("invite") => {
                self.invite_dismissed.store(true, Ordering::Release);
                overlay::hide_recording_overlay(&self.app);
            }
            state if is_card_stage(state) => dismiss_card(&self.app),
            _ => {}
        }
    }
}

fn samples_to_ms(samples: usize) -> u64 {
    samples as u64 * 1000 / crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE as u64
}

/// Waveform bars per channel, in the overlay's `mic-level` format (16 values
/// in 0..1, of which the overlay draws the first few).
const LEVEL_BARS: usize = 16;
const DRAWN_BARS: usize = 9;

/// Latest levels of both channels; the overlay shows the louder of the two so
/// it moves whoever is talking.
#[derive(Default)]
struct Meter {
    channels: Mutex<[[f32; LEVEL_BARS]; 2]>,
}

impl Meter {
    fn update(&self, app: &AppHandle, channel: Channel, samples: &[f32]) {
        let mut levels = [0.0; LEVEL_BARS];
        let window = samples.len().div_ceil(DRAWN_BARS).max(1);
        for (level, chunk) in levels.iter_mut().zip(samples.chunks(window)) {
            let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
            *level = (rms * 12.0).sqrt().min(1.0);
        }
        let combined = {
            let mut channels = self.channels.lock().unwrap();
            channels[channel as usize] = levels;
            std::array::from_fn::<f32, LEVEL_BARS, _>(|i| channels[0][i].max(channels[1][i]))
        };
        overlay::emit_levels(app, &combined);
    }
}

struct CaptureJob {
    channel: Channel,
    dir: PathBuf,
    vad_model: PathBuf,
    stop: Arc<AtomicBool>,
    utterances: mpsc::Sender<(Channel, Utterance)>,
    /// Audio recorded before the meeting began (microphone only); the system
    /// channel gets the same length of silence so both timelines line up.
    lead_in: Arc<Vec<f32>>,
    meter: Arc<Meter>,
    app: AppHandle,
}

/// Spawn the capture thread for one channel, returning once its audio source
/// is open (or with the error if it could not be opened). The thread runs
/// until `stop` is set.
fn start_capture(job: CaptureJob) -> Result<()> {
    use crate::audio_toolkit::vad::{
        frames_for_duration_ms, SileroVad, SmoothedVad, VoiceActivityDetector, VAD_ONSET_MS,
        VAD_PREFILL_MS,
    };
    use capture::AudioSource;
    use segmenter::Segmenter;
    use writer::ChunkedWavWriter;

    let CaptureJob {
        channel,
        dir,
        vad_model,
        stop,
        utterances,
        lead_in,
        meter,
        app,
    } = job;
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    thread::Builder::new()
        .name(format!("meeting-capture-{}", channel.wav_prefix()))
        .spawn(move || {
            let setup = || -> Result<(Box<dyn AudioSource>, Segmenter)> {
                let stream_name = format!("Reunião ({})", channel.wav_prefix());
                let source = match channel {
                    Channel::Mic => capture::open_microphone(&stream_name)?,
                    Channel::System => capture::open_system_audio(&stream_name)?,
                };
                let silero = SileroVad::new(&vad_model, SILERO_THRESHOLD)?;
                let frame = silero.frame_samples();
                let vad = SmoothedVad::new(
                    Box::new(silero),
                    frames_for_duration_ms(VAD_PREFILL_MS, frame),
                    frames_for_duration_ms(MEETING_HANGOVER_MS, frame),
                    frames_for_duration_ms(VAD_ONSET_MS, frame),
                );
                Ok((source, Segmenter::new(Box::new(vad), MAX_UTTERANCE_MS)))
            };
            let (mut source, mut segmenter) = match setup() {
                Ok(parts) => {
                    let _ = ready_tx.send(Ok(()));
                    parts
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };

            let mut wav = ChunkedWavWriter::new(&dir, channel.wav_prefix(), CHUNK_SECONDS);
            if !lead_in.is_empty() {
                let lead_in = match channel {
                    Channel::Mic => lead_in.to_vec(),
                    Channel::System => vec![0.0; lead_in.len()],
                };
                if let Err(e) = wav.write(&lead_in) {
                    error!("Failed to save {:?} audio: {e:#}", channel);
                }
                for utterance in segmenter.push(&lead_in) {
                    let _ = utterances.send((channel, utterance));
                }
            }
            let mut buffer = Vec::with_capacity(capture::READ_SAMPLES);
            while !stop.load(Ordering::Acquire) {
                if let Err(e) = source.read(&mut buffer) {
                    error!("{:?} capture stopped: {e:#}", channel);
                    notify("Captura de áudio interrompida", &format!("{e:#}"));
                    break;
                }
                if let Err(e) = wav.write(&buffer) {
                    error!("Failed to save {:?} audio: {e:#}", channel);
                }
                meter.update(&app, channel, &buffer);
                for utterance in segmenter.push(&buffer) {
                    let _ = utterances.send((channel, utterance));
                }
            }
            if let Some(utterance) = segmenter.flush() {
                let _ = utterances.send((channel, utterance));
            }
            if let Err(e) = wav.finish() {
                error!("Failed to finalize {:?} audio: {e:#}", channel);
            }
        })?;

    ready_rx
        .recv()
        .context("capture thread exited during setup")?
        .with_context(|| format!("starting {:?} capture", channel))
}

/// The engine transcribing a meeting: the dedicated meeting model, or the
/// dictation model when that one cannot be loaded (e.g. not downloaded).
struct MeetingModel {
    tm: Arc<TranscriptionManager>,
    dedicated: bool,
}

impl MeetingModel {
    fn pick(engine: Arc<TranscriptionManager>, dictation: Arc<TranscriptionManager>) -> Self {
        if engine.is_model_loaded() {
            return Self {
                tm: engine,
                dedicated: true,
            };
        }
        match engine.load_model(MEETING_MODEL) {
            Ok(()) => Self {
                tm: engine,
                dedicated: true,
            },
            Err(e) => {
                warn!("Meeting model unavailable, using the dictation model: {e:#}");
                dictation.initiate_model_load();
                Self {
                    tm: dictation,
                    dedicated: false,
                }
            }
        }
    }

    /// Reload after an idle unload.
    fn ensure_loaded(&self) {
        if self.tm.is_model_loaded() {
            return;
        }
        if !self.dedicated {
            self.tm.initiate_model_load();
        } else if let Err(e) = self.tm.load_model(MEETING_MODEL) {
            warn!("Failed to reload the meeting model: {e:#}");
        }
    }
}

/// Transcribe utterances in arrival order until both capture threads end.
fn run_transcriber(
    model: &MeetingModel,
    dir: &std::path::Path,
    title: &str,
    utterances: mpsc::Receiver<(Channel, Utterance)>,
) {
    let mut transcript = match TranscriptWriter::create(dir, title) {
        Ok(writer) => writer,
        Err(e) => {
            error!("Cannot write the meeting transcript: {e:#}");
            notify("Falha ao salvar a transcrição", &format!("{e:#}"));
            return;
        }
    };

    for (channel, utterance) in utterances {
        model.ensure_loaded();
        let text = match model.tm.transcribe(utterance.samples) {
            Ok(text) => text.trim().to_string(),
            Err(e) => {
                // The audio is still in the WAV chunks and can be redone.
                warn!("Meeting utterance transcription failed: {e:#}");
                "[falha na transcrição]".to_string()
            }
        };
        if text.is_empty() {
            continue;
        }
        let entry = TranscriptEntry {
            start_ms: utterance.start_ms,
            end_ms: utterance.end_ms,
            channel,
            speaker: channel.default_speaker().to_string(),
            text,
        };
        if let Err(e) = transcript.append(&entry) {
            error!("Failed to append to the meeting transcript: {e:#}");
        }
    }
}

fn finish_meeting(app: &AppHandle, dir: &Path, then: StopAction) {
    let has_transcript =
        std::fs::metadata(dir.join(writer::TRANSCRIPT_JSONL)).is_ok_and(|meta| meta.len() > 0);
    if !has_transcript {
        info!("Meeting ended without speech: {}", dir.display());
        dismiss_card(app);
        notify("Reunião finalizada", "Nenhuma fala foi detectada.");
        return;
    }

    let note = match diarize::diarize_meeting(dir) {
        Ok(path) => {
            info!("Meeting transcript ready: {}", path.display());
            None
        }
        Err(e) => {
            warn!("Speaker separation failed: {e:#}");
            if let Err(write_err) = diarize::write_undiarized(dir) {
                error!("Cannot write the final transcript: {write_err:#}");
            }
            Some("Sem separar falantes".to_string())
        }
    };
    meetings_changed(app);
    if then == StopAction::Summarize {
        summarize(app, dir, true);
    } else {
        offer_summary(app, dir, note);
    }
}

/// Post-meeting card on the overlay. It stays up until the user acts on it,
/// and comes back when something else borrowed the overlay meanwhile (a
/// dictation, a call invite). Stages, in order: `finishing` (transcription
/// and speaker separation), `done` (offer the AI step), `summarizing`,
/// `summarized` (copy the summary, open the folder).
#[derive(Clone, serde::Serialize)]
struct MeetingCard {
    stage: &'static str,
    /// Meeting folder name.
    name: String,
    /// Shown under the title: a warning, or where the AI step sends the text.
    note: Option<String>,
    /// Part of "transcribe and summarize": the loading shows the step.
    chained: bool,
}

const CARD_STAGES: &[&str] = &["finishing", "done", "summarizing", "summarized"];

static CARD: Mutex<Option<MeetingCard>> = Mutex::new(None);

fn is_card_stage(state: Option<&str>) -> bool {
    state.is_some_and(|state| CARD_STAGES.contains(&state))
}

/// Show (or park, if the overlay is busy) the card of a meeting.
fn show_card(app: &AppHandle, stage: &'static str, dir: &Path, note: Option<String>) {
    show_card_step(app, stage, dir, note, false);
}

fn show_card_step(
    app: &AppHandle,
    stage: &'static str,
    dir: &Path,
    note: Option<String>,
    chained: bool,
) {
    let Some(name) = meeting_name(dir) else {
        return;
    };
    let card = MeetingCard {
        stage,
        name,
        note,
        chained,
    };
    *CARD.lock().unwrap() = Some(card.clone());
    present_card(app, &card);
}

fn present_card(app: &AppHandle, card: &MeetingCard) {
    use tauri::Emitter;

    let free = match overlay::current_overlay_state().as_deref() {
        None => true,
        // The pill (or stop menu) of a meeting that already stopped.
        Some("meeting" | "stopmenu") => !app.state::<Arc<MeetingManager>>().is_active(),
        state => is_card_stage(state),
    };
    if !overlay::is_overlay_enabled() || !free {
        return;
    }
    let _ = app.emit_to("recording_overlay", "meeting-card", card);
    overlay::show_overlay_state(app, card.stage);
}

/// The user closed the card.
pub fn dismiss_card(app: &AppHandle) {
    CARD.lock().unwrap().take();
    if is_card_stage(overlay::current_overlay_state().as_deref()) {
        overlay::hide_recording_overlay(app);
    }
}

/// Bring the parked card back once the overlay is free again. Called after
/// every overlay hide; waits out the hide's fade first.
pub fn restore_card(app: &AppHandle) {
    if CARD.lock().unwrap().is_none() {
        return;
    }
    let app = app.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(400));
        let busy = overlay::current_overlay_state().is_some()
            || app.state::<Arc<AudioRecordingManager>>().is_recording()
            || app.state::<Arc<MeetingManager>>().is_active();
        if busy {
            return;
        }
        let card = CARD.lock().unwrap().clone();
        if let Some(card) = card {
            present_card(&app, &card);
        }
    });
}

/// Where the AI step sends the transcript, shown before the user opts in.
fn ai_note(app: &AppHandle) -> Option<String> {
    summary::provider_info(app).map(|ai| format!("IA: {} · {} · só o texto", ai.provider, ai.model))
}

/// Announce a finished meeting: the overlay card when the overlay is on, else
/// a notification that stays on screen until the user acts on it.
fn offer_summary(app: &AppHandle, dir: &Path, note: Option<String>) {
    if overlay::is_overlay_enabled() {
        show_card(app, "done", dir, note.or_else(|| ai_note(app)));
        return;
    }

    let summary = match &note {
        Some(note) => format!("Transcrição finalizada · {note}"),
        None => "Transcrição finalizada".to_string(),
    };
    let body = dir.join(diarize::FINAL_MD).display().to_string();
    let app = app.clone();
    let dir = dir.to_path_buf();
    notify_with_actions(
        &summary,
        &body,
        &[
            ("transcript", "Abrir transcrição"),
            ("summarize", "Gerar resumo e ata"),
            ("dismiss", "Dispensar"),
        ],
        true,
        move |action| match action.as_str() {
            "transcript" => open_meeting_file(&app, &dir.join(diarize::FINAL_MD)),
            "summarize" => start_summary(&app, &dir),
            _ => {}
        },
    );
}

/// Run the AI step in the background, reporting progress and the result on
/// the overlay card (or as notifications when the overlay is off).
pub fn start_summary(app: &AppHandle, dir: &Path) {
    summarize(app, dir, false);
}

/// `chained`: the second step of "transcribe and summarize".
fn summarize(app: &AppHandle, dir: &Path, chained: bool) {
    let task = match summary::summarize_meeting(app.clone(), dir.to_path_buf()) {
        Ok(task) => task,
        Err(e) => {
            warn!("Meeting summary not started: {e:#}");
            if overlay::is_overlay_enabled() {
                show_card(app, "done", dir, Some(format!("{e:#}")));
            } else {
                notify("Falha ao gerar resumo e ata", &format!("{e:#}"));
            }
            return;
        }
    };
    if overlay::is_overlay_enabled() {
        show_card_step(app, "summarizing", dir, None, chained);
    }
    meetings_changed(app);
    let app = app.clone();
    let dir = dir.to_path_buf();
    tauri::async_runtime::spawn(async move {
        let result = task.await;
        meetings_changed(&app);
        match result {
            Ok(path) => {
                info!("Meeting summary ready: {}", path.display());
                if overlay::is_overlay_enabled() {
                    show_card(&app, "summarized", &dir, None);
                    return;
                }
                notify_with_actions(
                    "Resumo e ata prontos",
                    &path.display().to_string(),
                    &[("open", "Abrir pasta")],
                    false,
                    move |action| {
                        if action == "open" {
                            open_meeting_dir(&app, &dir);
                        }
                    },
                );
            }
            Err(e) => {
                error!("Meeting summary failed: {e:#}");
                if overlay::is_overlay_enabled() {
                    show_card(&app, "done", &dir, Some(format!("Falha no resumo: {e:#}")));
                } else {
                    notify("Falha ao gerar resumo e ata", &format!("{e:#}"));
                }
            }
        }
    });
}

/// Put a meeting's summary on the clipboard.
pub fn copy_summary(app: &AppHandle, dir: &Path) -> Result<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let path = dir.join(summary::SUMMARY_MD);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    app.clipboard()
        .write_text(text)
        .context("writing to the clipboard")
}

/// Write the final transcript of a meeting that stopped without it (the app
/// closed mid-meeting), or retry speaker separation.
pub fn finalize_meeting(app: &AppHandle, dir: &Path) -> Result<()> {
    anyhow::ensure!(
        !is_in_progress(dir),
        "a transcrição desta reunião ainda está em andamento"
    );
    set_in_progress(dir, true);
    meetings_changed(app);
    show_card(app, "finishing", dir, None);
    let app = app.clone();
    let dir = dir.to_path_buf();
    thread::Builder::new()
        .name("meeting-finalize".into())
        .spawn(move || {
            finish_meeting(&app, &dir, StopAction::Transcribe);
            set_in_progress(&dir, false);
            meetings_changed(&app);
        })?;
    Ok(())
}

/// Delete a whole meeting folder: audio, transcripts and AI output.
pub fn delete_meeting(app: &AppHandle, dir: &Path) -> Result<()> {
    anyhow::ensure!(
        !is_in_progress(dir) && !summary::is_running(dir),
        "a reunião ainda está em processamento"
    );
    std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    info!("Meeting deleted: {}", dir.display());
    meetings_changed(app);
    Ok(())
}

pub fn meetings_root(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::portable::app_data_dir(app)?.join("meetings"))
}

fn meeting_name(dir: &Path) -> Option<String> {
    dir.file_name()?.to_str().map(str::to_string)
}

/// Resolve a meeting folder name coming from the UI.
pub fn meeting_dir(app: &AppHandle, name: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..",
        "invalid meeting name"
    );
    let dir = meetings_root(app)?.join(name);
    anyhow::ensure!(dir.is_dir(), "meeting not found: {name}");
    Ok(dir)
}

pub fn open_meeting_dir(app: &AppHandle, dir: &Path) {
    open_meeting_file(app, dir);
}

/// Open a meeting file or folder with the system's default app.
fn open_meeting_file(app: &AppHandle, path: &Path) {
    use tauri_plugin_opener::OpenerExt;
    if let Err(e) = app.opener().open_path(path.to_string_lossy(), None::<&str>) {
        warn!("Failed to open {}: {e}", path.display());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeetingStage {
    /// Transcribed; the AI step has not run.
    Transcribed,
    Summarizing,
    Summarized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingListing {
    /// Folder name, used as the meeting id.
    pub name: String,
    /// "09/10 14:30"
    pub label: String,
    pub stage: MeetingStage,
}

/// Most recent finished meetings first.
pub fn recent_meetings(app: &AppHandle, limit: usize) -> Vec<MeetingListing> {
    let Ok(root) = meetings_root(app) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join(diarize::FINAL_MD).exists())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    names
        .into_iter()
        .take(limit)
        .map(|name| {
            let dir = root.join(&name);
            let stage = if summary::is_running(&dir) {
                MeetingStage::Summarizing
            } else if summary::is_done(&dir) {
                MeetingStage::Summarized
            } else {
                MeetingStage::Transcribed
            };
            MeetingListing {
                label: meeting_label(&name),
                name,
                stage,
            }
        })
        .collect()
}

/// "2026-10-09_14-30-05" -> "09/10 14:30"
fn meeting_label(name: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(name, "%Y-%m-%d_%H-%M-%S")
        .map(|time| time.format("%d/%m %H:%M").to_string())
        .unwrap_or_else(|_| name.to_string())
}

/// Desktop notification; meeting mode runs with no window open.
fn notify(summary: &str, body: &str) {
    #[cfg(target_os = "linux")]
    if let Err(e) = std::process::Command::new("notify-send")
        .args(["--app-name=Handy", summary, body])
        .spawn()
    {
        warn!("notify-send failed: {e}");
    }
    #[cfg(not(target_os = "linux"))]
    info!("{summary}: {body}");
}

/// How often to look for a call in progress.
const CALL_POLL: Duration = Duration::from_secs(4);
/// Polls in a row without a call before it counts as ended. Answering,
/// holding or switching devices drops the call's streams for a moment.
const CALL_END_MISSES: u32 = 3;

/// Watch for calls and offer to transcribe each one. The invite stays up for
/// the whole call — ringing, answered, on hold — until the user dismisses it
/// or starts the meeting; if dictation takes the overlay meanwhile, the
/// invite comes back afterwards. It goes away on its own only when the call
/// ends.
pub fn spawn_call_watcher(app: AppHandle) {
    use tauri::Emitter;

    let spawned = thread::Builder::new()
        .name("meeting-call-watcher".into())
        .spawn(move || {
            // App of the call in progress, kept through short detection gaps.
            let mut call: Option<String> = None;
            let mut misses = 0;
            // A meeting ran during this call: do not offer it again.
            let mut handled = false;
            let mut announced = false;
            loop {
                thread::sleep(CALL_POLL);
                let manager = app.state::<Arc<MeetingManager>>();
                if manager.is_active() {
                    handled = true;
                    continue;
                }

                match detect::detect_call() {
                    Some(found) => {
                        misses = 0;
                        if call.is_none() {
                            info!("Call detected ({found})");
                        }
                        call = Some(found);
                    }
                    None if call.is_some() => {
                        misses += 1;
                        if misses < CALL_END_MISSES {
                            continue;
                        }
                        info!("Call ended");
                        call = None;
                        handled = false;
                        announced = false;
                        manager.invite_dismissed.store(false, Ordering::Release);
                        if overlay::current_overlay_state().as_deref() == Some("invite") {
                            overlay::hide_recording_overlay(&app);
                        }
                        continue;
                    }
                    None => continue,
                }

                let Some(name) = call.as_deref() else {
                    continue;
                };
                if handled
                    || manager.invite_dismissed.load(Ordering::Acquire)
                    || !overlay::is_overlay_enabled()
                {
                    continue;
                }
                if !announced {
                    // While dictating, the overlay highlights its meeting button.
                    let _ = app.emit_to("recording_overlay", "meeting-invite", name);
                    announced = true;
                }
                let recording = app.state::<Arc<AudioRecordingManager>>().is_recording();
                // The invite outranks the post-meeting card, which comes back
                // once the invite goes away.
                let state = overlay::current_overlay_state();
                let overlay_free = state.is_none() || is_card_stage(state.as_deref());
                if !recording && overlay_free {
                    let _ = app.emit_to("recording_overlay", "meeting-invite", name);
                    info!("Offering meeting mode for the {name} call");
                    overlay::show_meeting_invite(&app);
                }
            }
        });
    if let Err(e) = spawned {
        error!("Failed to start the call watcher: {e}");
    }
}

/// Notification with buttons; `on_action` gets the key of the button clicked.
/// Runs in the background until the notification closes. A `persistent` one
/// never expires: it waits on screen for a click or for the user to close it
/// (critical urgency is what GNOME honours; `-t 0` covers the other daemons).
fn notify_with_actions(
    summary: &str,
    body: &str,
    actions: &[(&str, &str)],
    persistent: bool,
    on_action: impl FnOnce(String) + Send + 'static,
) {
    #[cfg(target_os = "linux")]
    {
        let mut command = std::process::Command::new("notify-send");
        command.args(["--app-name=Handy", "--wait"]);
        if persistent {
            command.args(["--urgency=critical", "--expire-time=0"]);
        }
        for (key, label) in actions {
            command.arg(format!("--action={key}={label}"));
        }
        command
            .args([summary, body])
            .stdout(std::process::Stdio::piped());
        let child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                warn!("notify-send failed: {e}");
                return;
            }
        };
        thread::spawn(move || {
            if let Ok(output) = child.wait_with_output() {
                let action = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !action.is_empty() {
                    on_action(action);
                }
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (actions, persistent, on_action);
        notify(summary, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_labels_are_short_dates() {
        assert_eq!(meeting_label("2026-10-09_14-30-05"), "09/10 14:30");
        assert_eq!(meeting_label("other"), "other");
    }
}
