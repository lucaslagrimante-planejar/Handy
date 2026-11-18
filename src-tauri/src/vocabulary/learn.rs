//! Learns custom words from the dictation history. Each unread dictation is
//! re-transcribed with a more accurate local model (the reference); the
//! post-processing AI then compares the two texts, and the post-processed one
//! when there is one, and names the terms the dictation model got wrong.
//! Only text goes to the AI, never audio.
//!
//! What the AI proposes is checked against the texts before it is kept: a
//! word must be in the reference (or post-processed) text and missing from
//! what the dictation model wrote.
//!
//! Dictation only, both ways: it reads only the dictation history
//! (`transcription_history`, which meetings never write; their audio lives
//! in `meetings/`), and the words it learns are used only by the dictation
//! engine, never by the meeting one.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use specta::Type;
use tauri::{AppHandle, Emitter, Manager};

use super::store::{self, contains_ignoring_case};
use crate::audio_toolkit::read_wav_samples;
use crate::managers::history::{HistoryManager, VocabularySample};
use crate::managers::model::{ModelInfo, ModelManager};
use crate::managers::transcription::TranscriptionManager;
use crate::meeting::summary::Llm;
use crate::settings::get_settings;

/// Dictations read per click: the reference model is slow, so a run stays
/// short and the next click goes on from there.
const MAX_ENTRIES_PER_RUN: usize = 40;
/// Learned words kept per model. Too many custom words make a model worse
/// (Whisper even reads them back), so the oldest make room for new ones.
pub const MAX_WORDS_PER_MODEL: usize = 40;
/// Characters of transcripts per AI request.
const BATCH_CHARS: usize = 6_000;
/// Longer dictations are skipped rather than re-transcribed.
const MAX_SAMPLES: usize = 16_000 * 300;
const MIN_WORD_CHARS: usize = 2;
const MAX_WORD_CHARS: usize = 50;

pub const PROGRESS_EVENT: &str = "vocabulary-learning-progress";

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCELLED: AtomicBool = AtomicBool::new(false);
/// Engine running the reference model, kept like the meeting engine: an
/// engine's idle watcher lives as long as the app, so it is created once.
static REFERENCE_ENGINE: Mutex<Option<Arc<TranscriptionManager>>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LearnPhase {
    /// Re-transcribing dictations with the reference model.
    Transcribing,
    /// Asking the AI what was misheard.
    Comparing,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct LearnProgress {
    pub phase: LearnPhase,
    pub done: u32,
    pub total: u32,
}

#[derive(Clone, Debug, Default, Serialize, Type)]
pub struct LearnReport {
    /// Dictations read in this run.
    pub analyzed: u32,
    /// New words per model id.
    pub learned: BTreeMap<String, Vec<String>>,
    /// Models whose dictations were left unread: no more accurate model is
    /// downloaded to compare them with.
    pub without_reference: Vec<String>,
    pub cancelled: bool,
}

/// A dictation with its reference transcription.
#[derive(Clone, Debug)]
struct Pair {
    id: i64,
    original: String,
    reference: String,
    post_processed: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Finding {
    word: String,
    #[serde(default)]
    #[allow(dead_code)]
    heard_as: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Findings {
    #[serde(default)]
    words: Vec<Finding>,
}

struct RunningGuard;

impl RunningGuard {
    fn acquire() -> Result<Self> {
        if RUNNING.swap(true, Ordering::AcqRel) {
            bail!("O aprendizado de palavras já está em andamento");
        }
        CANCELLED.store(false, Ordering::Release);
        Ok(Self)
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

pub fn cancel() {
    CANCELLED.store(true, Ordering::Release);
}

fn cancelled() -> bool {
    CANCELLED.load(Ordering::Acquire)
}

fn emit_progress(app: &AppHandle, phase: LearnPhase, done: usize, total: usize) {
    let _ = app.emit(
        PROGRESS_EVENT,
        LearnProgress {
            phase,
            done: done as u32,
            total: total as u32,
        },
    );
}

/// Read the unread dictations and learn the words their models got wrong.
pub async fn learn(app: AppHandle) -> Result<LearnReport> {
    let _guard = RunningGuard::acquire()?;
    let llm = Llm::from_settings(&app)?;
    let hm = app.state::<Arc<HistoryManager>>().inner().clone();
    let models = app.state::<Arc<ModelManager>>().inner().clone();

    let samples = hm.entries_pending_vocabulary(MAX_ENTRIES_PER_RUN)?;
    let mut report = LearnReport::default();
    if samples.is_empty() {
        return Ok(report);
    }

    let selected_model = get_settings(&app).selected_model;
    let mut groups: BTreeMap<String, Vec<VocabularySample>> = BTreeMap::new();
    for sample in samples {
        let model = sample
            .model_id
            .clone()
            .unwrap_or_else(|| selected_model.clone());
        groups.entry(model).or_default().push(sample);
    }

    // Re-transcribe every group first, so the reference model loads once
    // per group and is gone before the slower AI step.
    let preferred = store::load(&app).reference_model;
    let available = models.get_available_models();
    let total: usize = groups.values().map(Vec::len).sum();
    let mut done = 0;
    let mut pairs: BTreeMap<String, Vec<Pair>> = BTreeMap::new();
    let mut unreadable = Vec::new();
    for (model_id, samples) in groups {
        let Some(reference) = pick_reference(&available, preferred.as_deref(), &model_id) else {
            report.without_reference.push(model_id);
            done += samples.len();
            continue;
        };
        let app = app.clone();
        let hm = hm.clone();
        let models = models.clone();
        let start = done;
        let (group_pairs, group_unreadable) = tauri::async_runtime::spawn_blocking(move || {
            transcribe_group(&app, &hm, models, &reference, samples, start, total)
        })
        .await
        .map_err(|e| anyhow!("A retranscrição parou: {e}"))??;
        done += group_pairs.len() + group_unreadable.len();
        unreadable.extend(group_unreadable);
        if !group_pairs.is_empty() {
            pairs.insert(model_id, group_pairs);
        }
        if cancelled() {
            break;
        }
    }
    unload_reference_engine();
    hm.mark_vocabulary_reviewed(&unreadable)?;
    report.analyzed += unreadable.len() as u32;

    let batches: Vec<(String, Vec<Pair>)> = pairs
        .into_iter()
        .flat_map(|(model_id, pairs)| {
            build_batches(pairs, BATCH_CHARS)
                .into_iter()
                .map(move |batch| (model_id.clone(), batch))
        })
        .collect();
    for (index, (model_id, batch)) in batches.iter().enumerate() {
        if cancelled() {
            report.cancelled = true;
            break;
        }
        emit_progress(&app, LearnPhase::Comparing, index, batches.len());
        let manual = get_settings(&app).custom_words;
        let known = store::custom_words_for(&app, Some(model_id), &manual);
        let answer = llm
            .ask(SYSTEM_PROMPT, render_batch(batch, &known), Some(schema()))
            .await?;
        let proposed = parse_findings(&answer)?;
        let accepted = filter_candidates(&proposed, batch, &known);
        info!(
            "Vocabulary learning for {model_id}: {} proposed, {} kept",
            proposed.len(),
            accepted.len()
        );
        if !accepted.is_empty() {
            store::update(&app, |vocabulary| {
                let learned = vocabulary.models.entry(model_id.clone()).or_default();
                store::merge_learned(learned, &accepted, MAX_WORDS_PER_MODEL);
            })?;
            report
                .learned
                .entry(model_id.clone())
                .or_default()
                .extend(accepted);
        }
        let ids: Vec<i64> = batch.iter().map(|pair| pair.id).collect();
        hm.mark_vocabulary_reviewed(&ids)?;
        report.analyzed += ids.len() as u32;
    }
    report.cancelled |= cancelled();
    emit_progress(&app, LearnPhase::Comparing, batches.len(), batches.len());
    Ok(report)
}

/// Re-transcribe `samples` with `reference`. Returns the pairs, and the ids
/// of dictations with no usable audio (to be marked read).
fn transcribe_group(
    app: &AppHandle,
    hm: &HistoryManager,
    models: Arc<ModelManager>,
    reference: &str,
    samples: Vec<VocabularySample>,
    start: usize,
    total: usize,
) -> Result<(Vec<Pair>, Vec<i64>)> {
    let engine = reference_engine(app, models)?;
    if engine.get_current_model().as_deref() != Some(reference) {
        info!("Loading {reference} as the vocabulary reference model");
        engine.load_model(reference)?;
    }
    // Keeps the model loaded between dictations.
    engine.set_job_active(true);
    let mut pairs = Vec::new();
    let mut unreadable = Vec::new();
    for (index, sample) in samples.into_iter().enumerate() {
        if cancelled() {
            break;
        }
        emit_progress(app, LearnPhase::Transcribing, start + index, total);
        let path = hm.get_audio_file_path(&sample.file_name);
        let audio = match read_wav_samples(&path) {
            Ok(audio) if !audio.is_empty() && audio.len() <= MAX_SAMPLES => audio,
            Ok(_) => {
                unreadable.push(sample.id);
                continue;
            }
            Err(e) => {
                warn!("Vocabulary learning skips {}: {e}", path.display());
                unreadable.push(sample.id);
                continue;
            }
        };
        match engine.transcribe(audio) {
            Ok(reference_text) if !reference_text.trim().is_empty() => pairs.push(Pair {
                id: sample.id,
                original: sample.transcription_text,
                reference: reference_text,
                post_processed: sample.post_processed_text,
            }),
            Ok(_) => unreadable.push(sample.id),
            // Left unread, so the next run tries it again.
            Err(e) => warn!(
                "Reference transcription of entry {} failed: {e:#}",
                sample.id
            ),
        }
    }
    engine.set_job_active(false);
    Ok((pairs, unreadable))
}

fn reference_engine(
    app: &AppHandle,
    models: Arc<ModelManager>,
) -> Result<Arc<TranscriptionManager>> {
    let mut engine = REFERENCE_ENGINE.lock().unwrap();
    if let Some(engine) = engine.as_ref() {
        return Ok(engine.clone());
    }
    let created = Arc::new(TranscriptionManager::new_background(app, models)?);
    *engine = Some(created.clone());
    Ok(created)
}

fn unload_reference_engine() {
    let engine = REFERENCE_ENGINE.lock().unwrap().clone();
    if let Some(engine) = engine {
        if engine.is_model_loaded() {
            if let Err(e) = engine.unload_model() {
                warn!("Failed to unload the vocabulary reference model: {e:#}");
            }
        }
    }
}

/// The model that re-transcribes dictations of `target`: the user's choice
/// when it is downloaded, else the most accurate downloaded speech model
/// that is more accurate than `target`.
fn pick_reference(
    available: &[ModelInfo],
    preferred: Option<&str>,
    target: &str,
) -> Option<String> {
    // Only speech models have an accuracy (the diarizer, for one, has none).
    let usable = |m: &&ModelInfo| m.is_downloaded && m.id != target && m.accuracy_score > 0.0;
    if let Some(preferred) = preferred {
        if let Some(model) = available.iter().filter(usable).find(|m| m.id == preferred) {
            return Some(model.id.clone());
        }
    }
    let target_accuracy = available
        .iter()
        .find(|m| m.id == target)
        .map_or(0.0, |m| m.accuracy_score);
    available
        .iter()
        .filter(usable)
        .filter(|m| m.accuracy_score > target_accuracy)
        .max_by(|a, b| a.accuracy_score.total_cmp(&b.accuracy_score))
        .map(|m| m.id.clone())
}

/// Split `pairs` into requests of about `max_chars` each, never empty.
fn build_batches(pairs: Vec<Pair>, max_chars: usize) -> Vec<Vec<Pair>> {
    let mut batches: Vec<Vec<Pair>> = Vec::new();
    let mut size = 0;
    for pair in pairs {
        let pair_size = pair.original.len()
            + pair.reference.len()
            + pair.post_processed.as_ref().map_or(0, String::len);
        if batches.is_empty() || (size + pair_size > max_chars && size > 0) {
            batches.push(Vec::new());
            size = 0;
        }
        size += pair_size;
        batches.last_mut().unwrap().push(pair);
    }
    batches
}

const SYSTEM_PROMPT: &str = "Você ajuda a calibrar um modelo de reconhecimento de fala (o modelo de ditado). \
Para cada ditado você recebe:\n\
A: o que o modelo de ditado transcreveu (pode ter erros);\n\
B: a transcrição do mesmo áudio por um modelo mais preciso (referência);\n\
C (às vezes): o texto final revisado.\n\
Encontre termos que o modelo de ditado escreveu errado e que valeria ensinar a ele como vocabulário: \
nomes próprios, nomes de produtos e empresas, siglas, termos técnicos, jargões, palavras estrangeiras ou incomuns. \
Dê a grafia correta como aparece em B ou C.\n\
Ignore: diferenças só de pontuação, maiúsculas ou acentos; palavras comuns do idioma; números; \
trechos em que B também parece errado; termos da lista de já conhecidos.\n\
Responda só com JSON no formato {\"words\":[{\"word\":\"grafia correta\",\"heard_as\":[\"como A escreveu\"]}]}. \
Sem nada a ensinar, responda {\"words\":[]}.";

fn schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "words": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "word": { "type": "string" },
                        "heard_as": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["word", "heard_as"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["words"],
        "additionalProperties": false
    })
}

fn render_batch(batch: &[Pair], known: &[String]) -> String {
    let mut out = String::new();
    if !known.is_empty() {
        out.push_str(&format!("Já conhecidos: {}\n\n", known.join(", ")));
    }
    for (index, pair) in batch.iter().enumerate() {
        out.push_str(&format!(
            "Ditado {}\nA: {}\nB: {}\n",
            index + 1,
            pair.original.trim(),
            pair.reference.trim()
        ));
        if let Some(post_processed) = &pair.post_processed {
            out.push_str(&format!("C: {}\n", post_processed.trim()));
        }
        out.push('\n');
    }
    out
}

/// The words in the AI's answer, tolerating a code fence or text around the
/// JSON object.
fn parse_findings(answer: &str) -> Result<Vec<String>> {
    let start = answer.find('{');
    let end = answer.rfind('}');
    let json = match (start, end) {
        (Some(start), Some(end)) if start < end => &answer[start..=end],
        _ => bail!("A IA não respondeu no formato esperado"),
    };
    let findings: Findings = serde_json::from_str(json)
        .map_err(|e| anyhow!("A IA não respondeu no formato esperado: {e}"))?;
    Ok(findings.words.into_iter().map(|f| f.word).collect())
}

/// The proposed words worth keeping: the right length, not known yet, and in
/// some dictation's reference or post-processed text while missing from what
/// the dictation model wrote. Repeats are dropped.
fn filter_candidates(proposed: &[String], batch: &[Pair], known: &[String]) -> Vec<String> {
    let mut accepted: Vec<String> = Vec::new();
    for word in proposed {
        let word = clean_word(word);
        let chars = word.chars().count();
        if !(MIN_WORD_CHARS..=MAX_WORD_CHARS).contains(&chars)
            || contains_ignoring_case(known, &word)
            || contains_ignoring_case(&accepted, &word)
        {
            continue;
        }
        let misheard = batch.iter().any(|pair| {
            let corrected = contains_term(&pair.reference, &word)
                || pair
                    .post_processed
                    .as_deref()
                    .is_some_and(|text| contains_term(text, &word));
            corrected && !contains_term(&pair.original, &word)
        });
        if misheard {
            accepted.push(word);
        }
    }
    accepted
}

/// Same normalisation as the custom words field in settings.
fn clean_word(word: &str) -> String {
    word.chars()
        .filter(|c| !matches!(c, '<' | '>' | '"' | '\''))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `text` has `term` as whole words, ignoring case.
fn contains_term(text: &str, term: &str) -> bool {
    let text = text.to_lowercase();
    let term = term.to_lowercase();
    if term.is_empty() {
        return false;
    }
    text.match_indices(&term).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + term.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::model::ModelSource;

    fn pair(id: i64, original: &str, reference: &str, post_processed: Option<&str>) -> Pair {
        Pair {
            id,
            original: original.into(),
            reference: reference.into(),
            post_processed: post_processed.map(Into::into),
        }
    }

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn keeps_a_term_the_reference_has_and_the_dictation_missed() {
        let batch = [pair(1, "abre o jay que grid", "abre o jqGrid", None)];
        let kept = filter_candidates(&words(&["jqGrid"]), &batch, &[]);
        assert_eq!(kept, words(&["jqGrid"]));
    }

    #[test]
    fn drops_a_term_the_dictation_already_wrote() {
        let batch = [pair(1, "o siplan caiu", "o Siplan caiu", None)];
        assert!(filter_candidates(&words(&["Siplan"]), &batch, &[]).is_empty());
    }

    #[test]
    fn drops_a_term_no_text_supports() {
        let batch = [pair(1, "abre o arquivo", "abre o arquivo", None)];
        assert!(filter_candidates(&words(&["Kubernetes"]), &batch, &[]).is_empty());
    }

    #[test]
    fn the_post_processed_text_counts_as_evidence() {
        let batch = [pair(
            1,
            "roda o cloud code",
            "roda o cloud code",
            Some("roda o Claude Code"),
        )];
        let kept = filter_candidates(&words(&["Claude Code"]), &batch, &[]);
        assert_eq!(kept, words(&["Claude Code"]));
    }

    #[test]
    fn drops_known_words_repeats_and_bad_lengths() {
        let batch = [pair(1, "x", "Handy jqGrid Z", None)];
        let kept = filter_candidates(
            &words(&["handy", "jqGrid", "JQGRID", "Z", "\"jqGrid\""]),
            &batch,
            &words(&["Handy"]),
        );
        assert_eq!(kept, words(&["jqGrid"]));
    }

    #[test]
    fn a_term_inside_another_word_is_not_a_match() {
        assert!(!contains_term("o transcrição", "trans"));
        assert!(contains_term("usa o React, ok", "react"));
        assert!(contains_term("ação", "ação"));
    }

    #[test]
    fn reads_the_answer_inside_a_code_fence() {
        let answer =
            "```json\n{\"words\":[{\"word\":\"jqGrid\",\"heard_as\":[\"jay que grid\"]}]}\n```";
        assert_eq!(parse_findings(answer).unwrap(), words(&["jqGrid"]));
        assert!(parse_findings("não sei").is_err());
    }

    #[test]
    fn batches_stay_near_the_size_limit() {
        let pairs = (0..5).map(|i| pair(i, "aaaa", "bbbb", None)).collect();
        let batches = build_batches(pairs, 16);
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![2, 2, 1]
        );
    }

    fn model(id: &str, accuracy: f32, downloaded: bool) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            filename: String::new(),
            source: ModelSource::Url {
                url: String::new(),
                sha256: None,
            },
            size_mb: 0,
            is_downloaded: downloaded,
            is_downloading: false,
            partial_size: 0,
            accuracy_score: accuracy,
            speed_score: 0.0,
            supports_translation: false,
            is_recommended: false,
            supported_languages: vec!["pt".into()],
            supports_language_selection: true,
            is_custom: false,
            supports_streaming: false,
            supports_language_detection: true,
        }
    }

    #[test]
    fn the_reference_is_the_most_accurate_downloaded_model_above_the_target() {
        let available = [
            model("small", 0.5, true),
            model("large", 0.9, true),
            model("huge", 0.95, false),
            model("medium", 0.7, true),
        ];
        assert_eq!(
            pick_reference(&available, None, "small").as_deref(),
            Some("large")
        );
        assert_eq!(pick_reference(&available, None, "large"), None);
        assert_eq!(
            pick_reference(&available, Some("medium"), "large").as_deref(),
            Some("medium")
        );
        assert_eq!(
            pick_reference(&available, Some("huge"), "small").as_deref(),
            Some("large")
        );
    }
}
