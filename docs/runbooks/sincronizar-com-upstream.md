# Sincronizar com o upstream (cjpais/Handy)

| Versão | Atualizado em |
| ------ | ------------- |
| 2      | 18/11/2026    |

Procedimento para trazer atualizações do projeto original sem perder as garantias de licença e
de distribuição do produto (ver [ADR 0001](../adr/0001-comercializacao-do-fork-handy.md)).

## Remotes

```bash
git remote -v
# origin    git@github.com:lucaslagrimante-planejar/Handy.git
# upstream  https://github.com/cjpais/Handy.git
```

## Passos

1. Buscar o upstream:
   ```bash
   git fetch upstream
   ```
2. **Conferir a licença antes de qualquer merge.** Se o upstream trocar a MIT por uma licença
   restritiva (BSL, SSPL, "fair-code"…), os commits **a partir da troca** vêm com a restrição
   nova. Tudo o que foi publicado antes continua MIT.
   ```bash
   git diff HEAD upstream/main -- LICENSE
   git log --oneline HEAD..upstream/main -- LICENSE
   ```
   Se houver mudança, **parar** e decidir: fazer merge só até o commit anterior à troca ou
   abandonar a sincronização daí em diante.
3. **Conferir o catálogo de modelos.** Modelos novos do upstream não entram automaticamente no
   catálogo comercial:
   ```bash
   git diff HEAD upstream/main -- src-tauri/src/catalog/catalog.json
   ```
   Para cada modelo novo, validar a licença conforme
   [`features/geral/licenciamento-e-modelos.md`](../features/geral/licenciamento-e-modelos.md).
   Remover os não comerciais e manter o mirror `blob.handy.computer` fora.
4. **Conferir o updater e a identidade.** O merge não pode reintroduzir o endpoint/pubkey do
   upstream nem o identifier/nomes originais:
   ```bash
   git diff HEAD upstream/main -- src-tauri/tauri.conf.json
   ```
5. Fazer o merge numa branch própria:
   ```bash
   git switch -c sync/upstream-$(date +%Y%m%d)
   git merge upstream/main
   ```
   Os conflitos esperados são poucos e mecânicos (ver [Onde o fork encosta no upstream](#onde-o-fork-encosta-no-upstream)):
   - `src/bindings.ts` é gerado: fique com a versão do upstream e regenere (build de debug,
     `bun run tauri dev`), ou junte os dois blocos à mão, já que nada ali é escrito por gente.
   - Nos arquivos de ganchos, mantenha as linhas dos dois lados.
6. Validar (`bun run lint`, `bun run format:check`, `bun run check:translations`, `cargo test`
   em `src-tauri`) e abrir PR para a branch principal do fork.

## Onde o fork encosta no upstream

O código do fork mora em arquivos próprios, que o upstream não tem e por isso nunca conflitam.
Os arquivos do upstream só recebem ganchos de poucas linhas que chamam esse código. Ao criar
funcionalidade nova, siga o mesmo padrão: código novo em arquivo novo, gancho mínimo no arquivo
do upstream.

| Parte               | Código do fork                                                                                   | Gancho no upstream                                                                                                                                  |
| ------------------- | ------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| Textos (26 idiomas) | `src/i18n/locales/*/fork.json`, `src/i18n/fork.ts`                                               | `src/i18n/index.ts` (mescla os dois), `src-tauri/build.rs` + `build_fork.rs` (tray), `scripts/check-translations.ts` + `check-translations-fork.ts` |
| Overlay (tela)      | `src/overlay/meeting/` (`useMeetingOverlay.tsx`, `useOverlayHitRegion.ts`, `MeetingOverlay.css`) | `src/overlay/RecordingOverlay.tsx`: import, tipo do estado, chamada dos hooks, botão de reunião, timer e `renderMeeting`                            |
| Overlay (janela)    | `src-tauri/src/meeting/overlay_ext.rs`                                                           | `src-tauri/src/overlay.rs`: tamanho dos estados, estado atual, `on_hidden`, `pub(crate)` em dois `static`                                           |
| Bandeja             | `src-tauri/src/meeting/tray_ext.rs`                                                              | `src-tauri/src/tray.rs` (campo `meetings` e submenu), `src-tauri/src/lib.rs` (evento do menu)                                                       |
| Comandos            | `src-tauri/src/commands/meeting.rs`                                                              | `src-tauri/src/lib.rs` (registro na lista de comandos)                                                                                              |

Os `translation.json` precisam continuar idênticos aos do upstream: texto novo do fork vai no
`fork.json` do idioma, nunca no `translation.json`.

Ainda há desvios diretos, menos sujeitos a conflito: `managers/transcription.rs` (instância de
fundo da reunião e `emit_model_state`), `managers/audio.rs`, `actions.rs`, `settings.rs`,
`shortcut/handler.rs`, `HistorySettings.tsx` e `Cargo.toml`.
