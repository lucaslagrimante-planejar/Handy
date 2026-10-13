---
name: build-do-fork-sem-chave-do-updater
description: Builds de release do fork precisam desligar os artefatos do updater; workflows herdados do upstream exigem certificados que o fork não tem
module: build
metadata:
  type: project
---

`src-tauri/tauri.conf.json` tem `bundle.createUpdaterArtifacts: true` e a pubkey do updater do
upstream. Sem `TAURI_SIGNING_PRIVATE_KEY`, o `tauri build` do fork falha. Build local de Linux que
funciona:

```bash
bun run tauri build --bundles deb,appimage --config '{"bundle":{"createUpdaterArtifacts":false}}'
```

O binário solto em `src-tauri/target/release/handy` não abre (falta `libtranscribe.so.0.3`, que só
vai dentro do pacote em `usr/lib/Handy/`). Para testar, use o AppImage
(`--appimage-extract-and-run --help`) ou instale o `.deb`.

No CI, os workflows herdados (`build-test.yml`, `pr-test-build.yml`) passam `sign-binaries: true`
e dependem de secrets Apple/Azure/Tauri do upstream. No fork, os builds nativos de Windows/macOS
exigem um workflow próprio que chame `build.yml` com `sign-binaries: false` e o mesmo override do
updater.

**Why:** descoberto ao fazer o primeiro build nativo do fork (13/10/2026). Sem isso, o build
quebra no fim, depois de ~8 min de compilação.

**How to apply:** use o override em todo build do fork até existir chave própria do updater (pendência
do ADR 0001); ao criar CI de build, não reaproveite os workflows assinados do upstream.

**Instalação local para teste (Linux):** o binário de `target/release/handy` procura
`libtranscribe.so.0.3` em `target/lib/Handy` (RUNPATH `$ORIGIN/../lib/Handy`), que não existe na
worktree. Extraia o `.deb` (`dpkg-deb -x`) para um diretório próprio, por exemplo
`~/.local/opt/handy-dev`, e aponte `~/.local/bin/handy` para `usr/bin/handy` dele. Antes, copie
`history.db` e `settings_store.json` de `~/.local/share/com.pais.handy`. As reuniões e gravações ficam
no mesmo lugar; o build novo só lê esses diretórios.
