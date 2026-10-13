# Licenciamento do código e dos modelos de IA

| Versão | Atualizado em |
| ------ | ------------- |
| 1      | 13/10/2026    |

Inventário levantado em 13/10/2026 para viabilizar a venda do produto. As decisões estão em
[ADR 0001](../../adr/0001-comercializacao-do-fork-handy.md) (comercialização) e
[ADR 0002](../../adr/0002-processamento-local-e-provedor-escolhido-pelo-usuario.md) (processamento
local, regras do catálogo). Este documento é análise técnica, não parecer jurídico.

## Código

| Componente                                                                                                                                      | Licença             | Obrigação                                         |
| ----------------------------------------------------------------------------------------------------------------------------------------------- | ------------------- | ------------------------------------------------- |
| Handy (upstream, `cjpais/Handy`)                                                                                                                | MIT                 | Manter `LICENSE` + copyright "CJ Pais" no produto |
| Crates Rust: tauri, rdev, enigo, earshot, handy-recorder, handy-keys, transcribe-rs, transcribe-cpp, natural, whatlang, rodio, rusqlite, strsim | MIT e/ou Apache-2.0 | Aviso de licença                                  |
| ferrous-opencc                                                                                                                                  | Apache-2.0          | Aviso + `NOTICE`, se houver                       |
| Frontend: React, Tauri plugins, i18next, zustand, zod, lucide-react, tailwind…                                                                  | MIT / ISC           | Aviso de licença                                  |
| ggml / llama.cpp (runtime de LLM planejado)                                                                                                     | MIT                 | Aviso de licença                                  |

Não há dependência GPL/AGPL. O código novo do fork pode ser fechado.

Ao adicionar dependências, rodar uma auditoria (`cargo about`/`cargo-deny` no Rust,
`license-checker` no frontend) e gerar a lista de licenças de terceiros para a tela "Sobre".

## Modelos de transcrição (catálogo herdado, `src-tauri/src/catalog/catalog.json`)

| Licença                                                      | Qtd. | Uso comercial                        |
| ------------------------------------------------------------ | ---- | ------------------------------------ |
| apache-2.0                                                   | 25   | ✅                                   |
| mit                                                          | 21   | ✅                                   |
| cc-by-4.0 (Parakeet v2/v3, Canary 180m-flash, Canary 1b-v2…) | 15   | ✅ com atribuição à NVIDIA           |
| **cc-by-nc-4.0** (`handy-computer/canary-1b-gguf`)           | 1    | ❌ **remover** do catálogo comercial |
| **other**                                                    | 7    | ⚠️ ler termo a termo                 |

Detalhe dos `other`:

| Modelo                                                                                                     | Termo                                                           | Situação                                                                                  |
| ---------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| SenseVoiceSmall, Fun-ASR-Nano-2512, Fun-ASR-MLT-Nano-2512                                                  | FunASR Model License                                            | Permite uso, modificação e redistribuição, **exige atribuição** e manter o nome do modelo |
| nemotron-3.5-asr-streaming-0.6b, nemotron-speech-streaming-en-0.6b, multitalker-parakeet-streaming-0.6b-v1 | Provavelmente NVIDIA Open Model License (permite uso comercial) | **Confirmar** texto de cada um                                                            |
| medasr (Google)                                                                                            | Termos de uso próprios                                          | **Tratar como não liberado** até ler                                                      |

Moonshine: os modelos em inglês são MIT. As variantes não-inglês (vi, ja, ko, ar, zh) aparecem
como `license:other` no HF (Moonshine Community License, com restrição comercial). **Revisar
antes de manter.**

## Modelos usados pelo modo reunião

| Modelo                             | Licença                                                                                          |
| ---------------------------------- | ------------------------------------------------------------------------------------------------ |
| Parakeet TDT 0.6B v3               | CC-BY-4.0 ✅ (atribuição)                                                                        |
| Sortformer streaming 4spk **v2.1** | `other`: **confirmar** (a v2 é CC-BY-4.0; voltar para a v2 se a v2.1 não permitir uso comercial) |
| Silero VAD v4                      | MIT ✅                                                                                           |

## LLMs candidatos para resumo e ata (local)

| Modelo                   | Licença                         | Uso no catálogo curado                                                                           |
| ------------------------ | ------------------------------- | ------------------------------------------------------------------------------------------------ |
| Qwen3 4B / 8B            | Apache-2.0                      | ✅ recomendado (contexto longo)                                                                  |
| Qwen2.5 7B Instruct      | Apache-2.0                      | ✅                                                                                               |
| Phi-4-mini-instruct      | MIT                             | ✅                                                                                               |
| Mistral 7B Instruct v0.3 | Apache-2.0                      | ✅                                                                                               |
| Granite 3.3 8B Instruct  | Apache-2.0                      | ✅                                                                                               |
| SmolLM3 3B               | Apache-2.0                      | ✅                                                                                               |
| Llama 3.2 3B             | Llama 3.2 Community License     | ⚠️ permitido com condições ("Built with Llama", política de uso, download com acesso controlado) |
| Gemma 3 4B               | Gemma Terms of Use              | ⚠️ permitido com condições (termos e política de uso repassados ao usuário)                      |
| Qwen2.5 3B Instruct      | Qwen Research License (`other`) | ❌ evitar                                                                                        |
| Ministral 8B             | Mistral Research License        | ❌ evitar                                                                                        |

## Regras para o catálogo

1. Todo modelo novo entra com o campo `license` preenchido e conferido na página original do modelo.
2. Catálogo curado (download em um clique): só licenças comerciais (Apache-2.0, MIT, CC-BY-\*
   sem NC, ou licenças próprias que permitam uso comercial e cujas condições cumprimos).
3. Modelos com atribuição (CC-BY, FunASR) aparecem na tela "Sobre / Licenças".
4. Modelos trazidos pelo usuário ("traga seu modelo", Ollama, nuvem) são responsabilidade dele
   (termos de uso).
5. Sem mirror `blob.handy.computer`: download direto do Hugging Face com revisão fixada + sha256.
