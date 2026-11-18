# Palavras aprendidas por modelo

| Versão | Atualizado em |
| ------ | ------------- |
| 3      | 18/11/2026    |

Funcionalidade própria deste fork. Em **Avançado → Palavras Aprendidas**, o botão "Aprender com o
histórico" lê os ditados ainda não lidos e descobre os termos que o modelo de ditado errou. Esses
termos viram palavras personalizadas **só daquele modelo**, porque cada modelo erra palavras
diferentes. O envio de texto ao provedor segue o
[ADR 0002](../../adr/0002-processamento-local-e-provedor-escolhido-pelo-usuario.md): vai só texto,
nunca áudio, e para o provedor de pós-processamento que o usuário configurou.

**Só ditado, nunca reunião**, nos dois sentidos:

- **Leitura:** lê só o histórico de ditados (`transcription_history`). O modo reunião nunca grava
  ali; os áudios e transcrições de reunião ficam em `meetings/`, que esta funcionalidade não lê.
  Um ditado convertido em reunião também não entra, porque a conversão cancela o ditado antes de
  ele ir para o histórico.
- **Uso:** as palavras aprendidas valem só no ditado. A transcrição da reunião continua só com as
  palavras manuais, inclusive quando o modelo de reunião não carrega e ela usa o engine de ditado.

## Fluxo

1. Pega até 40 ditados do histórico com `vocab_reviewed` nulo e texto não vazio, dos mais novos para
   os mais antigos. Arquivos transcritos ficam de fora. Os ditados são agrupados por `model_id`;
   quando o ditado é anterior à coluna, conta como do modelo selecionado no momento.
2. Retranscreve o WAV de cada ditado com o **modelo de referência**, num engine de fundo próprio
   (`TranscriptionManager::new_background`, o mesmo padrão do modo reunião). O modelo de ditado
   continua carregado durante a análise.
3. Manda à IA de pós-processamento lotes de até cerca de 6 mil caracteres, com três textos por
   ditado: A (o que o modelo escreveu), B (a referência) e C (o texto pós-processado, quando
   existe). A IA devolve, em JSON, os termos que o modelo grafou errado.
4. Um filtro determinístico confere cada termo proposto. Ele só fica se:
   - aparece em B ou C e não aparece em A, comparando palavra inteira e sem diferenciar
     maiúsculas;
   - tem de 2 a 50 caracteres;
   - ainda não é palavra conhecida, manual ou já aprendida.
5. Salva os termos aceitos em `learned_vocabulary.json`, no diretório de dados do app, com limite
   de 40 por modelo; ao estourar, saem os mais antigos. Os ditados do lote são marcados como lidos.

## Modelo de referência

O padrão é o modelo **baixado** de maior `accuracy_score` que seja mais preciso que o modelo
analisado. Se não houver nenhum, os ditados desse modelo continuam não lidos e a tela avisa. O
usuário pode fixar outro modelo no seletor ao lado do botão.

## Uso na transcrição

No ditado, `RunSetup.custom_words` reúne as palavras manuais e as aprendidas do modelo carregado
(`vocabulary::custom_words_for`). Essa lista substitui `settings.custom_words` tanto no vocabulário
enviado ao modelo quanto na correção fuzzy.

A reunião e o modelo de referência transcrevem por `TranscriptionManager::transcribe`, caminho que
o ditado não usa. Esse método roda dentro de `vocabulary::without_learned_words`, uma flag
thread-local que faz `custom_words_for` devolver só as manuais. Assim a regra vale por **quem
chama**, não por qual engine roda: cobre o fallback da reunião para o engine de ditado. O setup de
um batch é resolvido na thread de quem chamou, então a flag chega até ele. As threads do ditado ao
vivo não são afetadas.

## Arquivos

| Arquivo                                               | Responsabilidade                                                 |
| ----------------------------------------------------- | ---------------------------------------------------------------- |
| `src-tauri/src/vocabulary/learn.rs`                   | Orquestra o fluxo, o prompt, o filtro e a escolha da referência  |
| `src-tauri/src/vocabulary/store.rs`                   | `learned_vocabulary.json`, cache em memória, merge e limite      |
| `src-tauri/src/commands/vocabulary.rs`                | Comandos Tauri (aprender, cancelar, remover, limpar, referência) |
| `src/components/settings/vocabulary/LearnedWords.tsx` | Botão, progresso, seletor de referência e lista do modelo atual  |
| `src-tauri/src/managers/history.rs`                   | Colunas `model_id`/`vocab_reviewed`, consultas de pendentes      |
