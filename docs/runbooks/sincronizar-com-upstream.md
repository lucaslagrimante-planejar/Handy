# Sincronizar com o upstream (cjpais/Handy)

| Versão | Atualizado em |
| ------ | ------------- |
| 1      | 13/10/2026    |

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
5. Fazer o merge numa branch própria, resolver conflitos (principalmente em `meeting/`, que não
   existe no upstream, e nos pontos de integração `lib.rs`, `overlay.rs`, `settings.rs`):
   ```bash
   git switch -c sync/upstream-$(date +%Y%m%d)
   git merge upstream/main
   ```
6. Validar (`bun run lint`, `bun run format:check`, `cargo test` em `src-tauri`) e abrir PR para
   a branch principal do fork.
