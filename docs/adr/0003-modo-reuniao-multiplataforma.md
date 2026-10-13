# ADR 0003 — Modo reunião multiplataforma (captura e detecção por sistema operacional)

| Versão | Atualizado em |
| ------ | ------------- |
| 3      | 13/10/2026    |

## Status

Aceito. Captura de áudio e detecção de chamada implementadas e integradas na branch
`feat/meeting-cross-platform` (detecção em `src-tauri/src/meeting/detect/{mod,linux,windows,macos}.rs`).
**Ainda não testado em máquina Windows/macOS real.**

## Contexto

O **aplicativo base já roda em Windows, macOS e Linux**: ditado por atalho, transcrição local,
pós-processamento, histórico e overlay. São builds Tauri multiplataforma herdados do upstream
(`bundle.targets: "all"`, macOS mínimo 10.15). Também já existem dependências por SO no
`Cargo.toml` (`windows`, `objc2`, Swift bridge da Apple Intelligence).

A plataforma limita apenas o **modo reunião**:

| Parte                                                               | Linux                               | Windows                         | macOS                                         |
| ------------------------------------------------------------------- | ----------------------------------- | ------------------------------- | --------------------------------------------- |
| Ditado, transcrição, pós-processamento, histórico                   | ✅                                  | ✅                              | ✅                                            |
| Transcrição/diarização/resumo/ata da reunião (pipeline pós-captura) | ✅                                  | ✅ (código neutro)              | ✅ (código neutro)                            |
| Captura de microfone + áudio do sistema                             | ✅ PulseAudio/PipeWire (`libpulse`) | 🧪 implementado, sem teste real | 🧪 implementado, sem teste real (macOS 14.2+) |
| Detecção automática de chamada (convite no overlay)                 | ✅ `pactl` + `wmctrl`               | 🧪 implementado, sem teste real | 🧪 implementado, sem teste real               |

Antes desta decisão, `src-tauri/src/meeting/capture.rs` falava direto com a API do Pulse (o cpal
via ALSA não enxerga os monitores de sink) e `detect.rs` lia `pactl -f json` e `wmctrl`. Os dois
ficavam atrás de `#[cfg(target_os = "linux")]` em `meeting/mod.rs`.

Windows e macOS são a maior parte do mercado-alvo (ver [ADR 0001](0001-comercializacao-do-fork-handy.md)),
então o modo reunião precisa funcionar neles para o produto ser vendável.

## Decisão

### Captura de áudio: API neutra + backend por SO

`meeting/capture` expõe uma interface única, consumida pelo laço de gravação de `mod.rs`:

- `trait AudioSource { fn read(&mut self, out: &mut Vec<f32>) -> Result<()> }`: leitura
  bloqueante de blocos de ~100 ms em **16 kHz mono f32**, o formato esperado pelo VAD e pelos
  modelos;
- `open_microphone(..)` e `open_system_audio(..)` devolvem `Box<dyn AudioSource>`.

| SO      | Microfone                        | Áudio do sistema ("outros")                                                                                                                                                                                                                                            |
| ------- | -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Linux   | Pulse `@DEFAULT_SOURCE@` (atual) | Pulse `@DEFAULT_MONITOR@` (atual)                                                                                                                                                                                                                                      |
| Windows | cpal (WASAPI) no input padrão    | **WASAPI loopback**: stream de entrada aberto sobre o dispositivo de **saída** padrão (suportado pelo cpal)                                                                                                                                                            |
| macOS   | cpal (CoreAudio) no input padrão | **Core Audio process tap** (macOS 14.2+: `CATapDescription` global estéreo dentro de um aggregate device privado "Handy Meeting Audio", com a saída padrão como clock). Exige `NSAudioCaptureUsageDescription` (já em `src-tauri/Info.plist`) e a permissão do usuário |

Downmix e resample para 16 kHz usam o resampler que já existe em `audio_toolkit`. O callback do
cpal alimenta um buffer, e o `read` bloqueante é preservado para não mudar o restante do pipeline.

Implementação (`src-tauri/src/meeting/capture/`):

| Arquivo          | Conteúdo                                                                                                                                                                                                                                                                                        |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `mod.rs`         | `AudioSource`, `open_microphone`, `open_system_audio`, escolha do backend por SO                                                                                                                                                                                                                |
| `pulse.rs`       | Backend Linux (o antigo `capture.rs`, sem mudança de comportamento)                                                                                                                                                                                                                             |
| `cpal_source.rs` | `CpalSource`: microfone padrão, loopback WASAPI, abertura por nome. O callback só faz downmix para um ring buffer (`rtrb`); o resample roda na thread de captura. Completa com silêncio após 300 ms sem pacotes (o loopback do WASAPI fica mudo quando nada toca), mantendo os canais alinhados |
| `macos.rs`       | Process tap + aggregate device. `AudioHardwareCreateProcessTap`/`DestroyProcessTap` são resolvidos por `dlsym`: o app abre em macOS antigo e a reunião mostra erro com o requisito                                                                                                              |

Dependências novas (`Cargo.toml`, macOS): `objc2-core-audio = "0.3.2"` e
`objc2-core-foundation = "0.3.2"`. No Windows, nenhuma (o cpal já cobre o WASAPI).

Alternativa descartada para macOS: ScreenCaptureKit (macOS 13+). Exige permissão de gravação de
tela, que assusta o usuário e não é necessária só para áudio. Fica como fallback se for preciso
suportar macOS 13.

### Detecção de chamada: classificação compartilhada + fonte de dados por SO

`detect::classify(..)` (regras de apps nativos, navegadores e títulos de janela) já é pura e fica
compartilhada. Só a **coleta** muda:

| SO      | Quem usa áudio                                                                                                                     | Títulos de janela                                                                                              |
| ------- | ---------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Linux   | `pactl -f json list source-outputs / sink-inputs` (atual)                                                                          | `wmctrl -lp` (atual)                                                                                           |
| Windows | `IAudioSessionManager2` (sessões de áudio por processo, render e capture)                                                          | `EnumWindows` + `GetWindowText`                                                                                |
| macOS   | `NSWorkspace.runningApplications` + `kAudioProcessPropertyIsRunningInput` (14.4+) / `kAudioDevicePropertyDeviceIsRunningSomewhere` | `CGWindowListCopyWindowInfo` (títulos exigem permissão de gravação de tela; sem ela, detectar só apps nativos) |

O `spawn_call_watcher` (debounce `CALL_END_MISSES`, `invite_dismissed`) passa a valer para
todos os SOs quando houver fonte de dados.

## Consequências

- Linux mantém exatamente o comportamento atual.
- Windows e macOS **ainda não foram testados em máquina real**. Verificado até agora:
  - Linux: `cargo test --lib meeting` com 36/36 após integrar captura e detecção, incluindo
    testes do `CpalSource` (downmix, conversão de inteiros, overflow do ring, 48k → blocos de
    16k, silêncio no loopback, erro de dispositivo);
  - Windows (`x86_64-pc-windows-msvc`) e macOS (`aarch64-apple-darwin`): `cargo clippy` sem
    avisos nos módulos de captura e de detecção, cada um compilado num crate isolado (o app
    inteiro não faz cross-compile por causa das dependências nativas).
- macOS < 14.2: captura de microfone funciona, mas a de áudio do sistema retorna erro explicativo
  até existir um fallback (ScreenCaptureKit).
- O app precisa pedir permissões novas no macOS (microfone já existe; captura de áudio do sistema
  é nova) e explicá-las no onboarding.

## Pendências

- [ ] Build nativo e teste real no Windows e no macOS (Intel e Apple Silicon).
- [ ] macOS: confirmar que o cpal enxerga o aggregate device privado na enumeração. Se não
      enxergar, ler o aggregate direto com um IOProc.
- [ ] macOS: o tap global não exclui o próprio processo, então os sons de feedback do app entram
      no canal "outros". Excluir o PID do app no `CATapDescription`.
- [ ] Microfone no Windows/macOS usa o dispositivo padrão do sistema, não o escolhido nas
      configurações (mesma limitação do Linux).
- [ ] `notify()`/`notify_with_actions()` fora do Linux só registram em log. Falta notificação
      nativa com as ações do card.
- [x] Integrar a detecção de chamada de `feat/meeting-mode` com esta branch.
