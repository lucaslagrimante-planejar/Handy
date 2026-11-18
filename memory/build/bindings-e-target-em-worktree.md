---
name: bindings-e-target-em-worktree
description: Regenerar src/bindings.ts sem abrir o app (modo headless) e evitar recompilar tudo num worktree novo copiando o target de outro worktree
module: build
metadata:
  type: project
---

`bindings.ts` é exportado no início de `run()` em build debug, antes da checagem de instância única. Rode `cd src-tauri && cargo run -q -- --list-models`: o modo headless gera o arquivo sem abrir janela e sem conflitar com o Handy instalado que já está rodando (o `bun run tauri dev` esbarraria nele).

Worktree novo começa sem `src-tauri/target`, e a compilação do zero (transcribe-cpp) é longa. Antes do primeiro `cargo`, copie o target de um worktree vizinho de base próxima: `cp -a --reflink=auto <outro>/src-tauri/target <novo>/src-tauri/target` (~30 s para 8 GB). Só o crate `handy` recompila.

**Why:** usado na feature de palavras aprendidas (18/11/2026), com outra sessão buildando o main ao mesmo tempo.

**How to apply:** sempre que mudar comando/tipo Tauri, ou ao criar worktree para trabalho em Rust.
