//! Optional AI step, run by hand after a meeting: lightly revises the
//! transcript (punctuation and obvious mis-hearings, line by line, never a
//! rewrite) and writes a summary and minutes from it. Uses the
//! post-processing provider configured in Handy; only text is sent, never
//! audio.
//!
//! ```text
//! transcricao-revisada.md / .srt   revised transcript (originals untouched)
//! resumo.md                        executive summary by topic
//! ata.md                           minutes: agenda, decisions, action items
//! ```

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{stream, StreamExt};
use log::{info, warn};
use serde::Deserialize;
use serde_json::json;
use tauri::AppHandle;

use super::diarize::{self, render_final, render_srt};
use super::writer::{format_timestamp, read_transcript, TranscriptEntry};
use crate::settings::{get_settings, PostProcessProvider};

pub const REVISED_MD: &str = "transcricao-revisada.md";
pub const REVISED_SRT: &str = "transcricao-revisada.srt";
pub const SUMMARY_MD: &str = "resumo.md";
pub const MINUTES_MD: &str = "ata.md";

/// Characters of transcript per revision request: small enough for a fast,
/// faithful answer, large enough to give the model context.
const REVISION_BATCH_CHARS: usize = 6_000;
const REVISION_CONCURRENCY: usize = 4;
/// A revised line this much shorter or longer than the original is a rewrite,
/// not a fix; the original is kept.
const MAX_LENGTH_RATIO: f32 = 1.5;

/// Meetings with the AI step currently running.
static RUNNING: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

pub fn is_running(dir: &Path) -> bool {
    RUNNING
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|running| running.contains(dir))
}

pub fn is_done(dir: &Path) -> bool {
    dir.join(SUMMARY_MD).exists() && dir.join(MINUTES_MD).exists()
}

struct Llm {
    provider: PostProcessProvider,
    api_key: String,
    model: String,
}

impl Llm {
    fn from_settings(app: &AppHandle) -> Result<Self> {
        let settings = get_settings(app);
        let provider = settings
            .active_post_process_provider()
            .cloned()
            .ok_or_else(|| anyhow!("Nenhum provedor de pós-processamento selecionado"))?;
        let model = settings
            .post_process_models
            .get(&provider.id)
            .cloned()
            .unwrap_or_default();
        if model.trim().is_empty() {
            bail!(
                "O provedor de pós-processamento '{}' não tem modelo configurado",
                provider.id
            );
        }
        let api_key = settings
            .post_process_api_keys
            .get(&provider.id)
            .cloned()
            .unwrap_or_default();
        Ok(Self {
            provider,
            api_key,
            model,
        })
    }

    async fn ask(
        &self,
        system: &str,
        user: String,
        schema: Option<serde_json::Value>,
    ) -> Result<String> {
        let schema = schema.filter(|_| self.provider.supports_structured_output);
        let disable_reasoning = matches!(self.provider.id.as_str(), "custom" | "openrouter");
        let content = crate::llm_client::send_chat_completion_with_schema(
            &self.provider,
            self.api_key.clone(),
            &self.model,
            user,
            Some(system.to_string()),
            schema,
            disable_reasoning,
        )
        .await
        .map_err(|e| anyhow!(e))?
        .ok_or_else(|| anyhow!("O modelo não retornou conteúdo"))?;
        Ok(strip_think_block(&content).trim().to_string())
    }
}

/// What the AI step will use, shown next to its button. Never the key.
#[derive(serde::Serialize, specta::Type, Clone, Debug)]
pub struct AiProviderInfo {
    pub provider: String,
    pub model: String,
    pub has_key: bool,
}

pub fn provider_info(app: &AppHandle) -> Option<AiProviderInfo> {
    let settings = get_settings(app);
    let provider = settings.active_post_process_provider()?;
    let model = settings
        .post_process_models
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();
    let has_key = settings
        .post_process_api_keys
        .get(&provider.id)
        .is_some_and(|key| !key.trim().is_empty());
    Some(AiProviderInfo {
        provider: provider.label.clone(),
        model,
        has_key,
    })
}

fn strip_think_block(content: &str) -> &str {
    match content.trim_start().strip_prefix("<think>") {
        Some(rest) => rest
            .find("</think>")
            .map_or(content, |end| &rest[end + 8..]),
        None => content,
    }
}

/// Marks a meeting as running until dropped.
struct RunningGuard(PathBuf);

impl RunningGuard {
    fn acquire(dir: &Path) -> Result<Self> {
        let mut running = RUNNING.lock().unwrap();
        if !running
            .get_or_insert_with(HashSet::new)
            .insert(dir.to_path_buf())
        {
            bail!("O resumo desta reunião já está sendo gerado");
        }
        Ok(Self(dir.to_path_buf()))
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        if let Some(running) = RUNNING.lock().unwrap().as_mut() {
            running.remove(&self.0);
        }
    }
}

/// Mark the meeting as running now and return the AI step to await, which
/// resolves to the summary path.
pub fn summarize_meeting(
    app: AppHandle,
    dir: PathBuf,
) -> Result<impl std::future::Future<Output = Result<PathBuf>> + Send> {
    let guard = RunningGuard::acquire(&dir)?;
    Ok(async move {
        let _guard = guard;
        summarize(&app, &dir).await
    })
}

async fn summarize(app: &AppHandle, dir: &Path) -> Result<PathBuf> {
    let llm = Llm::from_settings(app)?;
    let mut entries = final_transcript(dir)?;
    if entries.is_empty() {
        bail!("A reunião não tem transcrição");
    }
    let title = diarize::title_from_dir(dir);
    let vocabulary = get_settings(app).custom_words;
    info!(
        "Summarizing meeting {} ({} lines) with {}/{}",
        dir.display(),
        entries.len(),
        llm.provider.id,
        llm.model
    );

    revise(&llm, &mut entries, &vocabulary).await;
    std::fs::write(dir.join(REVISED_MD), render_final(&title, &entries))?;
    std::fs::write(dir.join(REVISED_SRT), render_srt(&entries))?;

    let transcript = plain_transcript(&entries);
    let context = format!(
        "Título: {title}\nDuração: {}\nParticipantes identificados: {}\n\nTranscrição:\n{transcript}",
        format_timestamp(entries.last().map_or(0, |e| e.end_ms)),
        participants(&entries).join(", ")
    );
    let (summary, minutes) = tokio::join!(
        llm.ask(SUMMARY_PROMPT, context.clone(), None),
        llm.ask(MINUTES_PROMPT, context, None)
    );
    let summary_path = dir.join(SUMMARY_MD);
    std::fs::write(&summary_path, summary.context("gerando o resumo")? + "\n")?;
    std::fs::write(
        dir.join(MINUTES_MD),
        minutes.context("gerando a ata")? + "\n",
    )?;
    Ok(summary_path)
}

/// The speaker-separated transcript, or the live one if separation never ran.
fn final_transcript(dir: &Path) -> Result<Vec<TranscriptEntry>> {
    let final_path = dir.join(diarize::FINAL_JSONL);
    let mut entries = if final_path.exists() {
        std::fs::read_to_string(&final_path)?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).context("lendo a transcrição final"))
            .collect::<Result<Vec<TranscriptEntry>>>()?
    } else {
        read_transcript(dir)?
    };
    entries.sort_by_key(|e| e.start_ms);
    Ok(entries)
}

fn participants(entries: &[TranscriptEntry]) -> Vec<String> {
    let mut seen = Vec::new();
    for entry in entries {
        if !seen.contains(&entry.speaker) {
            seen.push(entry.speaker.clone());
        }
    }
    seen
}

fn plain_transcript(entries: &[TranscriptEntry]) -> String {
    entries
        .iter()
        .map(|e| {
            format!(
                "[{}] {}: {}\n",
                format_timestamp(e.start_ms),
                e.speaker,
                e.text
            )
        })
        .collect()
}

#[derive(Deserialize)]
struct RevisedLines {
    linhas: Vec<RevisedLine>,
}

#[derive(Deserialize)]
struct RevisedLine {
    i: usize,
    t: String,
}

/// Revise the transcript in batches; a batch that fails keeps its original
/// lines, so this step never loses text.
async fn revise(llm: &Llm, entries: &mut [TranscriptEntry], vocabulary: &[String]) {
    let system = revision_prompt(vocabulary);
    // Owned request bodies, so the futures below borrow nothing but `llm`.
    let bodies: Vec<String> = batch_indices(entries, REVISION_BATCH_CHARS)
        .into_iter()
        .map(|batch| {
            let lines: Vec<_> = batch
                .into_iter()
                .map(|i| json!({"i": i, "falante": entries[i].speaker, "t": entries[i].text}))
                .collect();
            json!({ "linhas": lines }).to_string()
        })
        .collect();
    let mut answers: Vec<Result<String>> = Vec::with_capacity(bodies.len());
    let mut pending = stream::iter(bodies)
        .map(|body| llm.ask(&system, body, Some(revision_schema())))
        .buffered(REVISION_CONCURRENCY);
    while let Some(answer) = pending.next().await {
        answers.push(answer);
    }
    drop(pending);

    let mut revised: HashMap<usize, String> = HashMap::new();
    for answer in answers {
        match answer.and_then(|text| parse_revision(&text)) {
            Ok(lines) => revised.extend(lines.into_iter().map(|line| (line.i, line.t))),
            Err(e) => warn!("Transcript revision batch failed, keeping it as is: {e:#}"),
        }
    }
    apply_revision(entries, revised);
}

fn parse_revision(text: &str) -> Result<Vec<RevisedLine>> {
    // Without structured output the JSON may come wrapped in prose or fences.
    let start = text.find('{').ok_or_else(|| anyhow!("resposta sem JSON"))?;
    let end = text
        .rfind('}')
        .ok_or_else(|| anyhow!("resposta sem JSON"))?;
    let parsed: RevisedLines = serde_json::from_str(&text[start..=end])?;
    Ok(parsed.linhas)
}

fn apply_revision(entries: &mut [TranscriptEntry], revised: HashMap<usize, String>) {
    for (i, text) in revised {
        let Some(entry) = entries.get_mut(i) else {
            continue;
        };
        let text = text.trim();
        let (old, new) = (
            entry.text.chars().count() as f32,
            text.chars().count() as f32,
        );
        if text.is_empty() || new > old * MAX_LENGTH_RATIO || new * MAX_LENGTH_RATIO < old {
            continue;
        }
        entry.text = text.to_string();
    }
}

/// Group consecutive line indices into batches of about `max_chars`.
fn batch_indices(entries: &[TranscriptEntry], max_chars: usize) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut size = 0;
    for (i, entry) in entries.iter().enumerate() {
        if size > 0 && size + entry.text.len() > max_chars {
            size = 0;
        }
        if size == 0 {
            batches.push(Vec::new());
        }
        batches.last_mut().expect("pushed above").push(i);
        size += entry.text.len();
    }
    batches
}

fn revision_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "linhas": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "i": {"type": "integer"},
                        "t": {"type": "string"}
                    },
                    "required": ["i", "t"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["linhas"],
        "additionalProperties": false
    })
}

fn revision_prompt(vocabulary: &[String]) -> String {
    let mut prompt = String::from(
        "Você revisa a transcrição automática de uma reunião em português do Brasil. \
Recebe um JSON {\"linhas\": [{\"i\", \"falante\", \"t\"}]} e devolve {\"linhas\": [{\"i\", \"t\"}]} \
com o texto de cada linha revisado.\n\
Regras:\n\
- Faça apenas correções pequenas: pontuação, maiúsculas, acentuação, concordância evidente e \
palavras claramente mal reconhecidas pelo reconhecimento de fala (pelo contexto).\n\
- Mantenha o sentido, a ordem e o estilo falado. Não resuma, não reescreva, não junte nem divida \
linhas, não traduza e não acrescente informação.\n\
- Termos técnicos, nomes próprios e siglas: use a grafia correta quando for evidente.\n\
- Se não houver o que corrigir, devolva o texto igual. Devolva todas as linhas recebidas, com o mesmo \"i\".",
    );
    if !vocabulary.is_empty() {
        prompt.push_str(&format!(
            "\n- Vocabulário frequente nestas reuniões (grafia correta): {}.",
            vocabulary.join(", ")
        ));
    }
    prompt
}

/// Rules shared by the summary and the minutes: nothing beyond the transcript.
macro_rules! grounding_rules {
    () => {
        "Regras: use apenas o que está na transcrição, sem inventar nem completar. \
Os falantes aparecem como \"Eu\" (quem gravou a reunião) e \"Pessoa N\". Mantenha esses rótulos; \
só use um nome quando a própria pessoa se apresentar ou for chamada pelo nome e responder, sem \
ambiguidade — mencionar um nome não identifica quem está falando. Não afirme que algo foi \
apresentado, explicado ou decidido se isso não aparece na transcrição; se ela terminar no meio \
de um assunto, diga isso. "
    };
}

const SUMMARY_PROMPT: &str = concat!(
    "Você recebe a transcrição de uma reunião em português do Brasil, \
com marcação de tempo e falante. Escreva um resumo executivo em Markdown, em português do Brasil.\n\
Formato:\n\
# Resumo — <assunto principal da reunião> — <data>\n\
Um parágrafo curto com o objetivo e o resultado geral da reunião.\n\
## 1. <tema>\n\
(uma seção numerada por tema discutido, na ordem em que aparecem, com os pontos principais em \
tópicos, quem defendeu o quê quando for relevante e o que ficou decidido)\n\
## Próxima reunião\n\
(somente se foi mencionada)\n\
",
    grounding_rules!(),
    "Ignore conversa social e trechos sem conteúdo. Responda só com o Markdown."
);

const MINUTES_PROMPT: &str = concat!(
    "Você recebe a transcrição de uma reunião em português do Brasil, \
com marcação de tempo e falante. Redija a ata da reunião em Markdown, em português do Brasil, \
em linguagem formal e objetiva.\n\
Formato:\n\
# Ata de Reunião — <assunto principal>\n\
**Data:** <data> · **Duração:** <duração>\n\
## 1. Participantes\n\
## 2. Pauta\n\
## 3. Discussão e deliberações\n\
(### 3.1, 3.2, ... um subtítulo por item da pauta)\n\
## 4. Decisões tomadas\n\
(lista numerada)\n\
## 5. Encaminhamentos\n\
(tabela | Ação | Responsável | Prazo |, com \"não definido\" quando não foi dito)\n\
## 6. Próxima reunião\n\
",
    grounding_rules!(),
    "Quando uma seção não tiver conteúdo, escreva \"Não houve.\". Responda só com o Markdown."
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::writer::Channel;

    fn entry(text: &str) -> TranscriptEntry {
        TranscriptEntry {
            start_ms: 0,
            end_ms: 1_000,
            channel: Channel::Mic,
            speaker: "Eu".into(),
            text: text.into(),
        }
    }

    #[test]
    fn batches_split_on_size_and_keep_order() {
        let entries = vec![entry("aaaa"), entry("bbbb"), entry("cccc")];
        assert_eq!(batch_indices(&entries, 8), vec![vec![0, 1], vec![2]]);
        assert_eq!(batch_indices(&entries, 2), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn revision_rejects_rewrites_and_unknown_lines() {
        let mut entries = vec![
            entry("bom dia a todos vamos começar"),
            entry("o deploy foi ontem"),
        ];
        let revised = HashMap::from([
            (0, "Bom dia a todos, vamos começar.".to_string()),
            (1, "Deploy.".to_string()),
            (7, "linha inexistente".to_string()),
        ]);
        apply_revision(&mut entries, revised);
        assert_eq!(entries[0].text, "Bom dia a todos, vamos começar.");
        assert_eq!(entries[1].text, "o deploy foi ontem");
    }

    #[test]
    fn revision_json_is_found_inside_prose() {
        let lines =
            parse_revision("Claro!\n```json\n{\"linhas\": [{\"i\": 3, \"t\": \"Oi.\"}]}\n```")
                .unwrap();
        assert_eq!((lines[0].i, lines[0].t.as_str()), (3, "Oi."));
    }

    #[test]
    fn think_blocks_are_dropped() {
        assert_eq!(strip_think_block("<think>hmm</think>\n# Ata"), "\n# Ata");
        assert_eq!(strip_think_block("# Ata"), "# Ata");
    }
}
