---
name: cross-check-windows-macos-crate-isolado
description: Como validar código Rust específico de Windows/macOS a partir do Linux, já que o app inteiro não faz cross-compile
module: meeting
metadata:
  type: project
---

O `cargo check --target x86_64-pc-windows-msvc|aarch64-apple-darwin` no `src-tauri` inteiro falha no
Linux por causa das dependências nativas (ggml/whisper, Swift bridge, etc.). Para validar código
`cfg(windows)`/`cfg(target_os = "macos")` (ex.: `src-tauri/src/meeting/capture/`, `meeting/detect/`),
monte um crate isolado fora do repo:

- `Cargo.toml` só com as deps usadas pelos arquivos (mesmas versões do `src-tauri/Cargo.toml`) e
  uma cópia do `src-tauri/Cargo.lock`;
- symlink do diretório do módulo real para `src/meeting/<modulo>`;
- `lib.rs` com stubs mínimos do que vem de `crate::` (ex.: `audio_toolkit::constants`,
  `FrameResampler`, `get_cpal_host`);
- `cargo clippy --offline --target <target>` (targets já instalados via rustup). Se o resultado
  sair em ~0,1 s, rode `cargo clean -p <crate> --target <target>` antes para não ler cache.

**Why:** sem isso o código de Windows/macOS só é compilado pela primeira vez na máquina do
usuário; o crate isolado pegou, por exemplo, um uso de `libc` que no app real é dep global.

**How to apply:** sempre que alterar código por SO no modo reunião (ou em qualquer módulo com
`cfg` de plataforma), faça esse check nos dois targets antes de commitar. Não substitui o teste em
máquina real, que continua pendente (ver `docs/adr/0003-modo-reuniao-multiplataforma.md`).
