# ADR 0002 — Processamento local e provedor de IA escolhido pelo usuário

| Versão | Atualizado em |
| ------ | ------------- |
| 1      | 13/10/2026    |

## Status

Aceito.

## Contexto

O modo reunião usa vários modelos: transcrição (ASR), separação de falantes (diarização) e LLM
para revisão, resumo e ata. Rodar esses modelos em infraestrutura própria na nuvem seria caro
demais para ser coberto pela assinatura.

## Decisão

1. **Sem infraestrutura de IA própria.** O produto é vendido como **processamento local**.
   Transcrição, diarização, resumo e ata rodam na máquina do usuário por padrão. Ter hardware
   capaz é **condição de uso**, informada antes da compra (requisitos mínimos).
2. **O usuário baixa os modelos.** Eles são públicos e gratuitos. **Não hospedamos modelos**:
   o download sai direto do Hugging Face, com revisão fixada e sha256 (o pipeline do
   `managers/model.rs` já faz isso).
   - **Remover o mirror `https://blob.handy.computer`** de `src-tauri/src/catalog/catalog.json`.
     É uma CDN paga pelo autor do upstream, e um produto pago não deve depender da banda dele.
   - Os repositórios `handy-computer/*` no Hugging Face são públicos (conversões GGUF do autor).
     Pode-se usá-los, mas, para não depender de quem pode apagá-los ou renomeá-los, duplicar os
     que usamos numa organização própria no HF, o que não tem custo.
3. **Nuvem é opção do usuário, com chave própria.** Quem não tiver máquina pode configurar um
   provider (OpenAI, Anthropic, OpenRouter, Groq…) com a **própria API key**:
   - a chamada sai **direto do app para o provider** (`src-tauri/src/llm_client.rs`; chave guardada
     localmente em `post_process_api_keys`), sem passar por servidor nosso;
   - o usuário fica sujeito às políticas do provider escolhido (retenção, treino, preço,
     disponibilidade). Isso está fora do nosso controle e é escolha dele;
   - **opt-in explícito**: o padrão é local. Na primeira vez, avisar "a transcrição será enviada
     para &lt;provider&gt;". O provider e o modelo ativos ficam sempre visíveis (o card pós-reunião
     já mostra "IA: provider · modelo").
4. **Responsabilidade por licença de modelo, conforme a origem:**
   - **Catálogo curado** (o que oferecemos com download em um clique): **responsabilidade
     nossa**. Só entram modelos com licença que permita uso comercial (Apache-2.0, MIT, CC-BY com
     atribuição). O uso do usuário (reunião de trabalho) é uso comercial.
   - **"Traga seu modelo"** (GGUF próprio, Ollama/LM Studio, provider em nuvem): responsabilidade
     do usuário, coberta por cláusula nos termos de uso.

## Privacidade e LGPD

- O canal "outros" grava terceiros. Processar localmente reduz o risco, mas o app deve lembrar o
  usuário de **informar os participantes** da gravação.
- Como os dados não passam por servidor nosso, não entramos na cadeia de tratamento do fluxo
  app → provider. Mesmo assim, a **política de privacidade** deve dizer: o processamento padrão é
  local; ao configurar um provider externo, valem os termos dele; o usuário responde pelo
  consentimento dos participantes.

## Consequências

- Custo marginal de IA por usuário igual a zero, o que viabiliza licença única ou assinatura barata.
- A qualidade e a velocidade dependem do hardware do usuário. Por isso é preciso recomendar o
  modelo pela RAM/GPU disponível (ver [`features/reunioes/modo-reuniao.md`](../features/reunioes/modo-reuniao.md)).
- O catálogo de modelos passa a ser artefato com revisão de licença obrigatória (ver
  [`features/geral/licenciamento-e-modelos.md`](../features/geral/licenciamento-e-modelos.md)).
