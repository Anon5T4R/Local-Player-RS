# Contribuições para o upstream `storytold/filmcraft`

Este diretório guarda o que o Local Player RS mudou no FilmCraft vendorizado
(`libs/filmcraft`) e os patches prontos para virar PR no repositório upstream.
**Nada foi submetido** — é preparação.

| Arquivo | O que é |
| --- | --- |
| `fork-patches.md` | todas as divergências do vendor vs snapshot `8fda2f2`, método da comparação, caso de uso e texto de PR em inglês |
| `0001-gop-host-frame-cache-budget.patch` | teto do cache de frames configurável pelo host (`crates/codecs/src/gop.rs`), com teste |

## O que é patch e o que não é

Só o `gop.rs` é contribuição de código. O `Cargo.toml` do vendor (workspace recortado, deps
internas pinadas, `craft-*` apontando para o `craft-libs` do app) é empacotamento deste repo e
**não** deve ser submetido. O detalhamento está em `fork-patches.md`.

## Como submeter (quando o João decidir)

```bash
# 1. Fork/branch no GitHub (conta do storytold) e clone
git clone https://github.com/storytold/filmcraft
cd filmcraft
git switch -c feat/gop-host-frame-cache-budget 8fda2f2   # commit base do vendor

# 2. Aplicar o patch (LF; o checkout Linux do CI aplica direto)
git apply /caminho/para/0001-gop-host-frame-cache-budget.patch

# 3. Verificar como eles exigem
cargo fmt --check
cargo clippy -p filmcraft-codecs --all-targets -- -D warnings
cargo test -p filmcraft-codecs gop

# 4. Commit + push + PR
git add crates/codecs/src/gop.rs
git commit -m "codecs: let the host cap the GopCache frame budget"
git push -u origin feat/gop-host-frame-cache-budget
gh pr create --repo storytold/filmcraft --title "codecs: let the host cap the GopCache frame budget" --body-file pr-body.md
```

O texto de PR está no `fork-patches.md` (bloco ```), pronto para um `pr-body.md`.

Notas de aplicação:

- O patch foi gerado contra o blob do `8fda2f2` e validado com `git apply --check`. Se o upstream
  já tiver avançado em `gop.rs`, rebase manual nos hunks (são cinco, pequenos: static/setter,
  campo, `new()`, `cache_budget()`, teste).
- Checkout Windows com `core.autocrlf=true` recusa o patch por CRLF/LF: aplique em WSL, num
  checkout com `core.autocrlf=false` ou no Linux. O conteúdo do patch é LF, como manda o repo
  deles.
- Depois de mergeado, o fork deste app pode largar o patch: `libs/filmcraft` volta a ser cópia
  fiel do upstream + `set_frame_cache_budget` chamado em `src/engine/mod.rs`
  (`configure_frame_cache`).
