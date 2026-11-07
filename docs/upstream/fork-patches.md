# Divergências do fork `libs/filmcraft` vs upstream `storytold/filmcraft`

Comparação feita contra o snapshot **`8fda2f2`** (`Merge pull request #3340 from aiongg/docking-ime`),
o commit base do vendor deste repo.

## Como a comparação foi feita

O fork não é um repo git: é uma cópia do upstream dentro de `libs/filmcraft`, reformatada com o
`rustfmt.toml` do app (`max_width = 150`, `use_small_heuristics = "Max"`) e com o workspace
recortado. Comparar byte a byte dava 424 arquivos "diferentes" — quase tudo reflow (vírgula final,
quebra de linha). O método que vale:

1. `git archive 8fda2f2` dos 28 crates vendorizados (bytes exatos do blob, LF);
2. normalização de espaços + pontuação estrutural (`,` `;` `{}` `()`) nos dois lados e comparação;
3. `git diff --word-diff` de cada arquivo que sobrou, para ler o diff de verdade.

Resultado do passo 2: **só dois arquivos diferem de fato** — `Cargo.toml` (empacotamento do
vendor) e `crates/codecs/src/gop.rs` (a mudança funcional). Os outros 422 são idênticos ao upstream
fora a formatação.

## Divergência 1 — teto do cache de frames configurável pelo host (`crates/codecs/src/gop.rs`)

**O que muda.** `GopCache` ganha um teto por cache configurado pelo processo host:

- `gop::set_frame_cache_budget(bytes)` (global, `0` = comportamento original);
- o valor é clampado em `FRAME_CACHE_CEILING` (384 MB) — o teto do upstream continua mandando;
- um orçamento vindo do host é **cap duro**: a reserva de frames pequenos (`MIN_FRAMES` = 64)
  não consegue mais elevá-lo. No upstream `cache_budget()` faz `max(reserve)`, então 16 MB
  configurados viram 64 × tamanho do frame (centenas de MB em 1080p/4K).

**Por quê (caso de uso).** O player consome `filmcraft-codecs` como biblioteca, não como editor:
quer uma janela pequena em volta do playhead, não centenas de MB por faixa. O padrão deste app é
128 MB por cache (`LOCALPLAYER_FRAME_CACHE_MB`); sem o cap duro o teto era inalcançável e a
máquina pequena sentia "quase congelou".

**Patch.** [`0001-gop-host-frame-cache-budget.patch`](0001-gop-host-frame-cache-budget.patch) —
validado com `git apply --check` contra o `gop.rs` do snapshot `8fda2f2`. Inclui o teste unitário
`a_host_budget_is_a_hard_cap`.

**Texto de PR (inglês, pronto para colar):**

```
codecs: let the host cap the GopCache frame budget

A player embedding filmcraft-codecs as a library wants a small decode window
around the playhead, not hundreds of MB of decoded frames per source. The
existing knobs do not allow that: GopCache::budget is private in practice (the
only public controls are the process-wide FRAME_BUDGET and the per-source
ceiling), and cache_budget() raises any budget below the small-frame reserve
(MIN_FRAMES * frame_bytes). Setting a small budget therefore has no effect.

This adds gop::set_frame_cache_budget(bytes), mirroring the existing global
knobs (FRAME_BUDGET / Pool::max_decoders):

- the value applies to every GopCache created after the call (set once at
  start-up);
- 0 restores the built-in behaviour, values are clamped to
  FRAME_CACHE_CEILING (the existing safety cap wins);
- a host-provided budget is a hard cap: the small-frame reserve may not
  enlarge it, because a player's window is small by design.

Use case: a media player sets 128 MB per cache instead of the 384 MB clip
ceiling, so decoded frames + decoder working set + UI fit on a small machine.

Tests: a_host_budget_is_a_hard_cap covers the cap, the reserve no longer
raising it, and the built-in ceiling winning over a larger host budget.
Existing behaviour is unchanged when the setter is never called.
```

## Divergência 2 — workspace do vendor (`Cargo.toml`) — **não vai para PR**

O `Cargo.toml` do fork é o do app, não o do upstream:

- `members` recortado para os 28 crates que o player usa (fora projeto, engine, ui-egui, gpu,
  export, captions, cfb, geom, edit...);
- deps internas pinadas com `version = "0.13.0"` além do `path` — o `cargo-deny` do app recusa
  path dep sem versão (wildcard);
- `craft-docpath`/`craft-media` apontam para `../craft-libs/crates/...` (vendor do app), com
  `craft-media` em `default-features = false` — a regra da suíte proíbe fonte `git+`.

Nada disso é mudança de código upstream: é empacotamento do vendor. **Não submeter.**

## Fora do fork (nem divergência, nem patch)

- **Prioridade da thread de decode.** O app baixa a prioridade da própria thread `video-decode`
  (`SetThreadPriority(THREAD_PRIORITY_BELOW_NORMAL)`, `src/engine/video.rs`) para o decode não
  competir com a UI pelo CPU. O fork não tem ponto de entrada para isso (o pool de decoders é
  interno e não expõe prioridade) e mexer nisso exigiria API nova: **proposta em aberto**, sem
  código preparado aqui — não queríamos mandar patch não exercitado pelo app.
- **Legendas embutidas no MKV.** O hub (`MediaSource`/`MediaInfo`) não expõe faixas de legenda
  (só vídeo e áudio), então o player só lê legenda externa. O demuxer `craft-media::matroska` já
  reconhece `SubRip`/`WebVtt`/`Ass`; uma contribuição possível seria um `MediaSource::subtitles()`
  no filmcraft-media. Não implementado, não submetido — anotado para a fase de maturação.

## Limitações conhecidas do patch

- `set_frame_cache_budget` é global (como `FRAME_BUDGET`): duas bibliotecas no mesmo processo com
  janelas diferentes teriam que coordenar. Um `GopCache::with_frame_budget(bytes)` por faixa é a
  evolução natural, mas o player só precisa do global.
- O upstream não tem teste de integração para os setters globais (estado de processo + testes em
  paralelo); mantivemos o teste no nível de `GopCache`, como os totais existentes.
