# Modo reunião

| Versão | Atualizado em |
| ------ | ------------- |
| 1      | 13/10/2026    |

Funcionalidade própria deste fork (não existe no upstream Handy). Grava a reunião, transcreve
localmente, separa falantes e gera resumo e ata. Decisões de produto em
[ADR 0001](../../adr/0001-comercializacao-do-fork-handy.md) e
[ADR 0002](../../adr/0002-processamento-local-e-provedor-escolhido-pelo-usuario.md), e de
plataforma em [ADR 0003](../../adr/0003-modo-reuniao-multiplataforma.md).

## Arquitetura atual

Código em `src-tauri/src/meeting/`:

| Arquivo        | Responsabilidade                                                                                           |
| -------------- | ---------------------------------------------------------------------------------------------------------- |
| `mod.rs`       | `MeetingManager`: início e parada, laço de captura por canal, chunks, card pós-reunião, watcher de chamada |
| `capture.rs`   | Captura de microfone ("Eu") e áudio do sistema ("outros") em 16 kHz mono (ver ADR 0003)                    |
| `segmenter.rs` | Silero VAD: corta a fala em enunciados (máx. 20 s, hangover de 800 ms)                                     |
| `writer.rs`    | Grava `transcript.md/.jsonl` ao vivo e os artefatos finais                                                 |
| `diarize.rs`   | Separação de falantes com Sortformer (subprocesso `--diarize-meeting`)                                     |
| `summary.rs`   | Revisão da transcrição + `resumo.md` + `ata.md` via LLM                                                    |
| `detect.rs`    | Detecção de chamada (Webex, Zoom, Teams, Meet, Jitsi…) para o convite no overlay                           |
| `history.rs`   | Listagem do histórico de reuniões                                                                          |

Fluxo:

1. **Início**: por atalho `meeting`, pelo tray, pelo botão no overlay (converte um ditado em
   reunião) ou aceitando o convite quando uma chamada é detectada.
2. **Captura**: dois canais independentes (mic e sistema), em WAV de 5 minutos
   (`eu-NNN.wav`, `outros-NNN.wav`).
3. **Transcrição ao vivo**: cada enunciado detectado pelo VAD vai para um engine dedicado com
   **Parakeet TDT 0.6B v3** (`MEETING_MODEL` em `mod.rs`, hoje hardcoded).
4. **Parada**: menu com `StopAction` (`transcribe` | `summarize` | `nothing`), seguido do
   card pós-reunião persistente (finalizando → pronto → resumindo → resumido), com ações
   copiar resumo e "depois".
5. **Pós-processamento**: diarização (Sortformer 4spk v2.1) e geração de
   `transcript-final.md/.jsonl/.srt`. Opcionalmente, resumo e ata:
   - revisão leve da transcrição em lotes de 6k caracteres → `transcricao-revisada.md/.srt`;
   - `resumo.md` e `ata.md` em paralelo, com prompts em português (constantes em `summary.rs`).

Armazenamento: `<app data>/meetings/<AAAA-MM-DD_HH-MM-SS>/`.

UI: card e menu no overlay (`src/overlay/RecordingOverlay.tsx`), histórico em
`src/components/settings/history/MeetingHistory.tsx`, comandos Tauri em
`src-tauri/src/commands/meeting.rs`.

## Modelos de IA (todos rodando localmente por padrão)

| Etapa       | Modelo atual                                                                                   | Licença                                         |
| ----------- | ---------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| Transcrição | Parakeet TDT 0.6B v3 (GGUF Q8_0, `handy-computer/parakeet-tdt-0.6b-v3-gguf`)                   | CC-BY-4.0 (atribuição à NVIDIA)                 |
| Diarização  | Sortformer streaming 4spk **v2.1** (`handy-computer/diar_streaming_sortformer_4spk-v2.1-gguf`) | `other`: **confirmar termo** (a v2 é CC-BY-4.0) |
| VAD         | Silero VAD v4                                                                                  | MIT                                             |
| Resumo/ata  | O provider/modelo ativo do **pós-processamento do ditado**                                     | depende do escolhido                            |

## Roadmap acordado

### 1. LLM dedicado à reunião

Hoje `summary.rs` usa `Llm::from_settings`, ou seja, o mesmo provider/modelo do pós-processamento
do ditado. As necessidades são opostas (o ditado quer modelo pequeno e rápido; o resumo/ata quer
contexto longo e qualidade), então será criada uma configuração própria `meeting_llm`
(local | servidor local | nuvem), independente de `post_process_*`.

### 2. Seleção de modelos na tela inicial

- Segundo seletor **"IA (resumo/ata)"** ao lado do seletor de modelo de transcrição, no rodapé
  da tela inicial (`src/components/footer/Footer.tsx` → `<ModelSelector/>`), reaproveitando
  `ModelDropdown`, `ModelCard` e `stores/modelStore.ts`.
- A tela atual de pós-processamento (`PostProcessingSettings`) continua como modo avançado.
- Onboarding: etapa opcional "Quer resumo e ata?" com recomendação pelo hardware.

### 3. Catálogo de LLMs locais

- Campo `kind: "asr" | "llm"` no `catalog.json`, reaproveitando o pipeline de download,
  progresso e sha256 de `managers/model.rs`.
- **Só modelos com licença comercial** (lista em
  [`../geral/licenciamento-e-modelos.md`](../geral/licenciamento-e-modelos.md)).
- Recomendação por RAM: ~3–4B Q4 para 8 GB; 7–8B Q4 para 16 GB ou mais. Uma reunião de 1 h tem
  ~10k tokens, então preferir modelos com contexto ≥ 32k (Qwen3).

### 4. Runtime de LLM local

Hoje não há runtime embutido (`llm_client.rs` só fala HTTP compatível com OpenAI). Duas fases:

1. **Curto prazo**: detectar Ollama / LM Studio rodando localmente e listar os modelos deles. O
   provider `custom` (`http://localhost:11434/v1`) e `fetch_post_process_models` já permitem isso.
2. **Médio prazo**: embutir **llama.cpp** (MIT) como processo worker, no mesmo padrão do
   `engine_supervisor/` usado pelo ASR, para o usuário não precisar instalar nada além do app.

### 5. Multiplataforma

Captura e detecção para Windows e macOS: ver [ADR 0003](../../adr/0003-modo-reuniao-multiplataforma.md).

### 6. Diferenciais comerciais (ADR 0001)

Ata por tipo de reunião, exportação .docx/PDF, participantes nomeados, histórico pesquisável,
integrações (agenda, Drive, Notion, e-mail).

## Pendências técnicas conhecidas

- `MEETING_MODEL` e o repositório do Sortformer estão hardcoded e fora do catálogo. Passá-los pelo
  catálogo (com sha256 e sem mirror do upstream).
- Aviso de consentimento dos participantes (LGPD) no início da gravação.
- Aviso de envio para a nuvem na primeira vez que um provider externo for usado.
