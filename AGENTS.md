# AGENTS.md — Local Player RS

Player de mídia 100% local e offline em Rust puro (sucessor em teste do
`local-player`, que usava mpv + IPC). Rust puro + egui, app single-crate.
Sem conta, sem telemetria, sem dependência de servidores de terceiros, sem
processo externo de playback.

## Regras
- **Dependências:** toda dep que entra/sai/atualiza exige `Cargo.lock` commitado +
  linha no inventário do repo `Anon5T4R/local-deps` (regra completa em
  `local-deps/WORKFLOW.md`).
- **Sem fontes git:** nenhuma dependência pode apontar para `git+` (o CI valida).
  Tudo vem do crates.io (pinado pelo lock) ou de `libs/` (vendored).
- **Commits:** português, sem Co-Authored-By.
- **Release:** tag `vX.Y.Z` → o Actions builda Windows (MSI + portable zip, WiX v5 via
  `packaging/windows/package.ps1`) e Linux (`packaging/linux/package.sh`, molde do
  local-office: AppImage + tar.gz FHS **com `install.sh` na raiz** + deb + rpm), valida
  o PKGBUILD real (`packaging/arch/`, container Arch + `makepkg --printsrcinfo`) e
  publica. Mínimo local.
- **Disco local:** nunca deixe `target/` para trás — `cargo clean` após uso local;
  builds pesados são do CI.

## Estrutura
- `src/` — app single-crate; `src/engine/` é o motor de áudio da fase 2 (thread
  própria: decode symphonia/filmcraft, ring SPSC e saída cpal); `src/pickers.rs`:
  seletor nativo async via rfd + pollster (portal XDG no Linux)
- `icons/` — ícones do app (master PNG 1024)
- `assets/app-icon/hicolor/` — árvore de ícones commitada (gerada por `packaging/icons.sh`)
- `libs/` — vendored, `exclude` do workspace do app:
  - `egui/` + `winit/` — forks do padrão craft
  - `craft-libs/` — tema/i18n/update e `craft-media`/`craft-docpath`
  - `filmcraft/` — workspace próprio (`libs/filmcraft/Cargo.toml`) com os crates
    de container/codec do FilmCraft usados pelo motor; `crates/hwdec`
    (`filmcraft-hwdec`) é o recorte do `filmcraft-platform` do upstream: decoders de
    hardware do Windows (Media Foundation/DXVA) e do Linux (VA-API/NVDEC) com
    fallback SW transparente
- `packaging/windows/package.ps1` + `local-player-rs.wxs` — MSI per-machine (WiX v5,
  Program Files\Local Player RS) e portable zip
- `packaging/linux/` — `package.sh` (AppImage + tar FHS + install.sh + deb/rpm),
  `local-player-rs.desktop`, `local-player-rs.metainfo.xml.in`, `nfpm.yaml`, `postinst.sh`
- `packaging/arch/PKGBUILD` — pacote Arch a partir do tarball FHS da release

## Build
```
cargo check                                # motor + shell (sem target/ depois: cargo clean)
cargo build --locked                       # debug
cargo test --locked                        # testes
cargo xtask version X.Y.Z                  # bump (Cargo.toml + lock)
cargo xtask check                          # confere as versões sincronizadas
```

## Upstream e a pasta `<root>/storytold`

Menções a `storytold/*` em código, comentários, docs e atribuições (NOTICE/ATTRIBUTION) são referência histórica e obrigação de licença. **Nada** é lido de `<root>/storytold` (Windows `C:\Dev\storytold` · Arch `~/Dev/dev-arch/storytold`) em build, teste ou release: tudo vem do próprio repo (crates.io pinado pelo lock + `libs/`/`vendor/` vendored + CI que busca craft-fonts/craft-i18n do GitHub). Essa pasta existe somente como referência local para a skill `sync-upstream`.

Divergências entre o craft-libs vendored e o que o FilmCraft espera se resolvem
**no fork** (`libs/filmcraft`), nunca em `libs/craft-libs`.
