# Palavras aprendidas por modelo

| Versão | Atualizado em |
| ------ | ------------- |
| 4      | 18/11/2026    |

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
   existe). A IA devolve, em JSON:
   - `words`: os termos que o modelo grafou errado, com a grafia certa e como A escreveu
     (`heard_as`). Inclui palavras já aprendidas que o modelo errou de novo;
   - `unknown`: termos de A que parecem errados ou fora de contexto, mas cuja forma certa nem B nem
     C mostram (um produto interno que nenhum dos dois modelos conhece), com um trecho de contexto.
4. Um filtro determinístico confere cada termo de `words`. Ele só conta se:
   - aparece em B ou C e não aparece em A, comparando palavra inteira e sem diferenciar
     maiúsculas;
   - tem de 2 a 50 caracteres;
   - não é palavra manual do usuário.

   O termo conta **um por ditado** em que essa evidência vale. Para `unknown`, o termo precisa
   aparecer em A, não ser palavra conhecida e não estar explicado por um termo de `words`.

5. Grava em `learned_vocabulary.json`, no diretório de dados do app, e marca os ditados do lote como
   lidos (ver [Contador, edição e termos a revisar](#contador-edição-e-termos-a-revisar)).

## Contador, edição e termos a revisar

Cada palavra aprendida guarda `count` (em quantos ditados o modelo a errou), as grafias que o modelo
usou (`heard_as`, até 6) e `last_seen`. Uma palavra já aprendida que o modelo erra de novo soma ao
contador em vez de ser descartada, então as mais erradas ficam evidentes:

- **Ordem:** na tela e na lista enviada ao modelo, das mais erradas para as menos, depois as mais
  recentes.
- **Limite:** continua 40 por modelo. Ao estourar, sai a mais fraca (menor contador, depois a mais
  antiga), não a mais antiga.
- **Edição:** clicar na palavra abre a edição da grafia, que mantém o contador. Se a nova grafia já
  existe, as duas se juntam e os contadores somam.

Os termos de `unknown` viram **suspeitos**, na seção "A revisar" da tela, com contador e contexto. O
usuário escreve a palavra certa (o suspeito vira palavra aprendida com o contador que tinha) ou
ignora (o suspeito fica guardado como dispensado, para não voltar). Se um aprendizado posterior
descobrir a grafia certa de um suspeito, ele se junta à palavra. Até 40 suspeitos por modelo, saindo
os mais fracos.

O arquivo da primeira versão (lista simples de palavras por modelo) continua sendo lido: cada palavra
entra com contador 1.

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

| Arquivo                                               | Responsabilidade                                                  |
| ----------------------------------------------------- | ----------------------------------------------------------------- |
| `src-tauri/src/vocabulary/learn.rs`                   | Orquestra o fluxo, o prompt, o filtro e a escolha da referência   |
| `src-tauri/src/vocabulary/store.rs`                   | `learned_vocabulary.json`, contador, suspeitos, edição e limite   |
| `src-tauri/src/commands/vocabulary.rs`                | Comandos Tauri (aprender, editar, remover, suspeitos, referência) |
| `src/components/settings/vocabulary/LearnedWords.tsx` | Botão, progresso, seletor de referência e lista do modelo atual   |
| `src-tauri/src/managers/history.rs`                   | Colunas `model_id`/`vocab_reviewed`, consultas de pendentes       |
