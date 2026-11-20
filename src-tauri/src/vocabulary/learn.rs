//! Learns custom words from the dictation history. Each unread dictation is
//! re-transcribed with a more accurate local model (the reference); the
//! post-processing AI then compares the two texts, and the post-processed one
//! when there is one, and names the terms the dictation model got wrong.
//! Only text goes to the AI, never audio.
//!
//! What the AI proposes is checked against the texts before it is kept: a
//! word must be in the reference (or post-processed) text and missing from
//! what the dictation model wrote. Each word counts the dictations it was
//! misheard in, also after it is learned, so the strongest stand out.
//!
//! The AI also names terms that look wrong in what the model wrote but that
//! no text corrects (an internal product name neither model knows). Those
//! wait as suspects, with their count, for the user to name or dismiss.
//!
//! Dictation only, both ways: it reads only the dictation history
//! (`transcription_history`, which meetings never write; their audio lives
//! in `meetings/`), and the words it learns are used only in dictation,
//! never in a meeting, even when one falls back to the dictation engine
//! (see [`super::store::without_learned_words`]).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use specta::Type;
use tauri::{AppHandle, Emitter, Manager};

use super::store::{self, contains_ignoring_case, Hit, SuspectHit};
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
/// Suspects kept per model, dismissed ones included.
pub const MAX_SUSPECTS_PER_MODEL: usize = 40;
/// Characters of a suspect's context excerpt.
const MAX_CONTEXT_CHARS: usize = 160;
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
    /// Words already learned that were misheard again, per model id.
    pub reinforced: BTreeMap<String, Vec<String>>,
    /// Terms the AI couldn't correct, new or seen again, per model id.
    pub suspects: BTreeMap<String, Vec<String>>,
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Finding {
    word: String,
    #[serde(default)]
    heard_as: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Unknown {
    heard_as: String,
    #[serde(default)]
    context: String,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
struct Findings {
    #[serde(default)]
    words: Vec<Finding>,
    #[serde(default)]
    unknown: Vec<Unknown>,
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
        let findings = parse_findings(&answer)?;
        let hits = filter_candidates(&findings.words, batch, &manual);
        let suspects = filter_suspects(&findings.unknown, batch, &known, &hits);
        info!(
            "Vocabulary learning for {model_id}: {} proposed, {} kept, {} suspects",
            findings.words.len(),
            hits.len(),
            suspects.len()
        );
        if !hits.is_empty() || !suspects.is_empty() {
            let now = store::now();
            let mut learned_before = Vec::new();
            store::update(&app, |vocabulary| {
                let model = vocabulary.models.entry(model_id.clone()).or_default();
                learned_before = model.words.iter().map(|w| w.word.clone()).collect();
                store::record_hits(model, &hits, now, MAX_WORDS_PER_MODEL);
                store::record_suspects(model, &suspects, now, MAX_SUSPECTS_PER_MODEL);
            })?;
            for hit in hits {
                let list = if contains_ignoring_case(&learned_before, &hit.word) {
                    &mut report.reinforced
                } else {
                    &mut report.learned
                };
                list.entry(model_id.clone()).or_default().push(hit.word);
            }
            report
                .suspects
                .entry(model_id.clone())
                .or_default()
                .extend(suspects.into_iter().map(|s| s.heard_as));
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
1. Em \"words\", liste os termos que o modelo de ditado escreveu errado e que valeria ensinar a ele como vocabulário: \
nomes próprios, nomes de produtos e empresas, siglas, termos técnicos, jargões, palavras estrangeiras ou incomuns. \
Dê a grafia correta como aparece em B ou C, e em heard_as como A escreveu. \
Inclua também os termos da lista de já conhecidos que A escreveu errado de novo: isso conta quantas vezes o modelo erra cada um.\n\
2. Em \"unknown\", liste os termos de A que parecem errados ou fora de contexto (provavelmente um nome, produto interno ou sigla \
que o modelo não conhece), mas cuja forma certa nem B nem C mostram. Dê heard_as exatamente como aparece em A \
e context com um trecho curto de A em volta dele. Não repita aqui o que já está em \"words\".\n\
Ignore nos dois: diferenças só de pontuação, maiúsculas ou acentos; palavras comuns do idioma usadas no sentido certo; números; \
trechos em que B também parece errado mas A está certo.\n\
Responda só com JSON no formato \
{\"words\":[{\"word\":\"grafia correta\",\"heard_as\":[\"como A escreveu\"]}],\"unknown\":[{\"heard_as\":\"como A escreveu\",\"context\":\"trecho de A\"}]}. \
Sem nada a apontar, responda {\"words\":[],\"unknown\":[]}.";

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
            },
            "unknown": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "heard_as": { "type": "string" },
                        "context": { "type": "string" }
                    },
                    "required": ["heard_as", "context"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["words", "unknown"],
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

/// The AI's findings, tolerating a code fence or text around the JSON
/// object.
fn parse_findings(answer: &str) -> Result<Findings> {
    let start = answer.find('{');
    let end = answer.rfind('}');
    let json = match (start, end) {
        (Some(start), Some(end)) if start < end => &answer[start..=end],
        _ => bail!("A IA não respondeu no formato esperado"),
    };
    serde_json::from_str(json).map_err(|e| anyhow!("A IA não respondeu no formato esperado: {e}"))
}

fn usable_length(word: &str) -> bool {
    (MIN_WORD_CHARS..=MAX_WORD_CHARS).contains(&word.chars().count())
}

/// The proposed words worth keeping, each with the dictations it was
/// misheard in: in the reference or post-processed text while missing from
/// what the dictation model wrote. The user's own words are left out; the
/// learned ones count again. Repeats merge.
fn filter_candidates(proposed: &[Finding], batch: &[Pair], manual: &[String]) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    for finding in proposed {
        let word = clean_word(&finding.word);
        if !usable_length(&word) || contains_ignoring_case(manual, &word) {
            continue;
        }
        let misheard = batch
            .iter()
            .filter(|pair| {
                let corrected = contains_term(&pair.reference, &word)
                    || pair
                        .post_processed
                        .as_deref()
                        .is_some_and(|text| contains_term(text, &word));
                corrected && !contains_term(&pair.original, &word)
            })
            .count() as u32;
        if misheard == 0 {
            continue;
        }
        let heard_as: Vec<String> = finding
            .heard_as
            .iter()
            .map(|h| clean_word(h))
            .filter(|h| usable_length(h))
            .collect();
        let lower = word.to_lowercase();
        match hits.iter_mut().find(|h| h.word.to_lowercase() == lower) {
            // The same word twice in one answer is the same dictations.
            Some(hit) => {
                for spelling in heard_as {
                    if !contains_ignoring_case(&hit.heard_as, &spelling) {
                        hit.heard_as.push(spelling);
                    }
                }
            }
            None => hits.push(Hit {
                word,
                heard_as,
                dictations: misheard,
            }),
        }
    }
    hits
}

/// The terms the AI couldn't correct, each with the dictations whose model
/// text has it. Known words and what `hits` already explain are left out.
fn filter_suspects(
    unknown: &[Unknown],
    batch: &[Pair],
    known: &[String],
    hits: &[Hit],
) -> Vec<SuspectHit> {
    let mut suspects: Vec<SuspectHit> = Vec::new();
    for finding in unknown {
        let heard_as = clean_word(&finding.heard_as);
        let explained = hits.iter().any(|hit| {
            hit.word.to_lowercase() == heard_as.to_lowercase()
                || contains_ignoring_case(&hit.heard_as, &heard_as)
        });
        if !usable_length(&heard_as)
            || explained
            || contains_ignoring_case(known, &heard_as)
            || suspects
                .iter()
                .any(|s| s.heard_as.to_lowercase() == heard_as.to_lowercase())
        {
            continue;
        }
        let dictations = batch
            .iter()
            .filter(|pair| contains_term(&pair.original, &heard_as))
            .count() as u32;
        if dictations == 0 {
            continue;
        }
        let context: String = finding
            .context
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_CONTEXT_CHARS)
            .collect();
        suspects.push(SuspectHit {
            heard_as,
            context,
            dictations,
        });
    }
    suspects
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

    fn found(list: &[&str]) -> Vec<Finding> {
        list.iter()
            .map(|w| Finding {
                word: w.to_string(),
                heard_as: Vec::new(),
            })
            .collect()
    }

    fn kept_words(hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|h| h.word.clone()).collect()
    }

    fn unknown(heard_as: &str, context: &str) -> Unknown {
        Unknown {
            heard_as: heard_as.into(),
            context: context.into(),
        }
    }

    #[test]
    fn keeps_a_term_the_reference_has_and_the_dictation_missed() {
        let batch = [pair(1, "abre o jay que grid", "abre o jqGrid", None)];
        let kept = filter_candidates(&found(&["jqGrid"]), &batch, &[]);
        assert_eq!(kept_words(&kept), words(&["jqGrid"]));
        assert_eq!(kept[0].dictations, 1);
    }

    #[test]
    fn counts_the_dictations_a_word_was_misheard_in() {
        let batch = [
            pair(1, "abre o jay que grid", "abre o jqGrid", None),
            pair(2, "o jqGrid travou", "o jqGrid travou", None),
            pair(3, "fecha o jota que grid", "fecha o jqGrid", None),
        ];
        let proposed = [Finding {
            word: "jqGrid".into(),
            heard_as: words(&["jay que grid", "jota que grid"]),
        }];
        let kept = filter_candidates(&proposed, &batch, &[]);
        assert_eq!(kept[0].dictations, 2);
        assert_eq!(kept[0].heard_as, words(&["jay que grid", "jota que grid"]));
    }

    #[test]
    fn drops_a_term_the_dictation_already_wrote() {
        let batch = [pair(1, "o siplan caiu", "o Siplan caiu", None)];
        assert!(filter_candidates(&found(&["Siplan"]), &batch, &[]).is_empty());
    }

    #[test]
    fn drops_a_term_no_text_supports() {
        let batch = [pair(1, "abre o arquivo", "abre o arquivo", None)];
        assert!(filter_candidates(&found(&["Kubernetes"]), &batch, &[]).is_empty());
    }

    #[test]
    fn the_post_processed_text_counts_as_evidence() {
        let batch = [pair(
            1,
            "roda o cloud code",
            "roda o cloud code",
            Some("roda o Claude Code"),
        )];
        let kept = filter_candidates(&found(&["Claude Code"]), &batch, &[]);
        assert_eq!(kept_words(&kept), words(&["Claude Code"]));
    }

    #[test]
    fn drops_the_users_words_repeats_and_bad_lengths() {
        let batch = [pair(1, "x", "Handy jqGrid Z", None)];
        let kept = filter_candidates(
            &found(&["handy", "jqGrid", "JQGRID", "Z", "\"jqGrid\""]),
            &batch,
            &words(&["Handy"]),
        );
        assert_eq!(kept_words(&kept), words(&["jqGrid"]));
        assert_eq!(kept[0].dictations, 1);
    }

    #[test]
    fn a_suspect_must_be_in_what_the_model_wrote() {
        let batch = [
            pair(1, "abre o sisplan agora", "abre o sisplan agora", None),
            pair(2, "o sisplan caiu", "o sisplan caiu", None),
        ];
        let suspects = filter_suspects(
            &[
                unknown("sisplan", "abre o sisplan agora"),
                unknown("Sisplan", "repetido"),
                unknown("inventado", "não está em A"),
            ],
            &batch,
            &[],
            &[],
        );
        assert_eq!(suspects.len(), 1);
        assert_eq!(suspects[0].heard_as, "sisplan");
        assert_eq!(suspects[0].dictations, 2);
        assert_eq!(suspects[0].context, "abre o sisplan agora");
    }

    #[test]
    fn a_suspect_already_explained_or_known_is_dropped() {
        let batch = [pair(
            1,
            "o jay que grid e o handy",
            "o jqGrid e o Handy",
            None,
        )];
        let hits = [Hit {
            word: "jqGrid".into(),
            heard_as: words(&["jay que grid"]),
            dictations: 1,
        }];
        let suspects = filter_suspects(
            &[unknown("jay que grid", ""), unknown("handy", "")],
            &batch,
            &words(&["Handy"]),
            &hits,
        );
        assert!(suspects.is_empty());
    }

    #[test]
    fn a_term_inside_another_word_is_not_a_match() {
        assert!(!contains_term("o transcrição", "trans"));
        assert!(contains_term("usa o React, ok", "react"));
        assert!(contains_term("ação", "ação"));
    }

    #[test]
    fn reads_the_answer_inside_a_code_fence() {
        let answer = "```json\n{\"words\":[{\"word\":\"jqGrid\",\"heard_as\":[\"jay que grid\"]}],\
            \"unknown\":[{\"heard_as\":\"sisplan\",\"context\":\"o sisplan caiu\"}]}\n```";
        let findings = parse_findings(answer).unwrap();
        assert_eq!(findings.words[0].word, "jqGrid");
        assert_eq!(findings.words[0].heard_as, words(&["jay que grid"]));
        assert_eq!(findings.unknown, vec![unknown("sisplan", "o sisplan caiu")]);
        assert!(parse_findings("não sei").is_err());
    }

    #[test]
    fn an_answer_without_unknown_still_reads() {
        let findings = parse_findings(r#"{"words":[]}"#).unwrap();
        assert!(findings.unknown.is_empty());
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
