# ADR 0003 — Modo reunião multiplataforma (captura e detecção por sistema operacional)

| Versão | Atualizado em |
| ------ | ------------- |
| 1      | 13/10/2026    |

## Status

Aceito, implementação em andamento (branch `feat/meeting-cross-platform`).

## Contexto

O **aplicativo base já roda em Windows, macOS e Linux**: ditado por atalho, transcrição local,
pós-processamento, histórico e overlay. São builds Tauri multiplataforma herdados do upstream
(`bundle.targets: "all"`, macOS mínimo 10.15). Também já existem dependências por SO no
`Cargo.toml` (`windows`, `objc2`, Swift bridge da Apple Intelligence).

A plataforma limita apenas o **modo reunião**:

| Parte                                                               | Linux                               | Windows               | macOS                 |
| ------------------------------------------------------------------- | ----------------------------------- | --------------------- | --------------------- |
| Ditado, transcrição, pós-processamento, histórico                   | ✅                                  | ✅                    | ✅                    |
| Transcrição/diarização/resumo/ata da reunião (pipeline pós-captura) | ✅                                  | ✅ (código neutro)    | ✅ (código neutro)    |
| Captura de microfone + áudio do sistema                             | ✅ PulseAudio/PipeWire (`libpulse`) | 🚧 em desenvolvimento | 🚧 em desenvolvimento |
| Detecção automática de chamada (convite no overlay)                 | ✅ `pactl` + `wmctrl`               | 🚧 planejado          | 🚧 planejado          |

Hoje `src-tauri/src/meeting/capture.rs` fala direto com a API do Pulse (o cpal via ALSA não
enxerga os monitores de sink). `detect.rs` lê `pactl -f json` e `wmctrl`. Os dois ficam atrás de
`#[cfg(target_os = "linux")]` em `meeting/mod.rs`.

Windows e macOS são a maior parte do mercado-alvo (ver [ADR 0001](0001-comercializacao-do-fork-handy.md)),
então o modo reunião precisa funcionar neles para o produto ser vendável.

## Decisão

### Captura de áudio: API neutra + backend por SO

`meeting/capture` expõe uma interface única, consumida pelo laço de gravação de `mod.rs`:

- `trait AudioSource { fn read(&mut self, out: &mut Vec<f32>) -> Result<()> }`: leitura
  bloqueante de blocos de ~100 ms em **16 kHz mono f32**, o formato esperado pelo VAD e pelos
  modelos;
- `open_microphone(..)` e `open_system_audio(..)` devolvem `Box<dyn AudioSource>`.

| SO      | Microfone                        | Áudio do sistema ("outros")                                                                                                                                                          |
| ------- | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Linux   | Pulse `@DEFAULT_SOURCE@` (atual) | Pulse `@DEFAULT_MONITOR@` (atual)                                                                                                                                                    |
| Windows | cpal (WASAPI) no input padrão    | **WASAPI loopback**: stream de entrada aberto sobre o dispositivo de **saída** padrão (suportado pelo cpal)                                                                          |
| macOS   | cpal (CoreAudio) no input padrão | **Core Audio process tap** (macOS 14.4+: `CATapDescription` global excluindo o próprio processo + aggregate device). Exige `NSAudioCaptureUsageDescription` e a permissão do usuário |

Downmix e resample para 16 kHz usam o resampler que já existe em `audio_toolkit`. O callback do
cpal alimenta um buffer, e o `read` bloqueante é preservado para não mudar o restante do pipeline.

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
- Windows e macOS **ainda não foram testados em máquina real**. O código é escrito e checado
  em Linux, e a validação depende de build nativo em cada SO (pendência).
- macOS < 14.4: captura de microfone funciona, mas a de áudio do sistema retorna erro explicativo
  até existir um fallback.
- O app precisa pedir permissões novas no macOS (microfone já existe; captura de áudio do sistema
  é nova) e explicá-las no onboarding.
