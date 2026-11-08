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
fora a formatação. Depois dessa comparação o fork ganhou mais dois arquivos/trechos:
**`crates/codecs/src/subtitles.rs`** (divergência 4, módulo novo registrado em
`crates/codecs/src/lib.rs`) e **`crates/codecs/src/mp4.rs`** (divergência 5, empty edit de corte).

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

## Divergência 3 — decoder de vídeo do `platform` recortado (`crates/hwdec`, crate `filmcraft-hwdec`)

**O que muda.** O decoder de vídeo por hardware do Windows saiu de `filmcraft-platform` como um
crate próprio no fork:

- copiados sem mudança de comportamento: `media_foundation/{mod,gpu,mft,stream}.rs`, `annexb.rs`,
  `biplanar.rs`, `hybrid.rs` (o upstream inteiro, menos `cursor`/encoders);
- `lib.rs` reescrito só com o registro de decoder (`register`/`registered`/`available`) e a factory
  de Media Foundation — sem NVENC, VideoToolbox encode, VA-API, NVDEC nem as probes de export;
- `codec.rs` novo: família/bit depth/decisão puros (testáveis sem GPU) e o `probe()` do `--probe`,
  que responde "provável" sem criar decoder MFT (só enumera o MFT e consulta o perfil DXVA na GPU);
- `media_foundation::supports` + `mft::has_decoder` novos para isso.

**Por quê.** O `filmcraft-platform` do upstream depende de `filmcraft-export`, `filmcraft-project`,
`filmcraft-render` e do engine porque carrega três famílias de coisa: decoders, encoders de
hardware e as probes de export (formatos/HDR). Um player só precisa do decoder de vídeo; puxar o
resto é arrastar o editor. O recorte também deixa o SW pure-Rust como fundação explícita: o crate
não depende de nada do app.

**Estado atual (fork).** `register()` no Windows registra a factory de Media Foundation e liga o
`set_hw_backend("Media Foundation")`; em qualquer outra plataforma devolve
`Availability::Unavailable` e o decode fica 100% software. Promover Linux (VA-API/NVDEC) e macOS
(VideoToolbox) é acrescentar o módulo `cfg` correspondente e o braço no `register()`, como no
upstream.

**Proposta de upstream (não submetida).** Extrair `crates/platform` em dois: `filmcraft-hwdec`
(decode: VideoToolbox / Media Foundation / VA-API / NVDEC + `hybrid`) e `filmcraft-platform`
(encode + probes de export), com `platform` dependendo de `hwdec`. Ganho para eles: apps/bench que
só decodificam param de compilar project/render/export; o bench do `ui-egui` e o player usariam o
mesmo crate. Enquanto isso, este fork mantém a cópia e anota cada ponto de mudança com
`// fork local-player-rs:`.

**Pendências herdadas do upstream.** O readback é GPU→CPU (staging texture) e o upload para a wgpu
continua no app: zero-copy D3D11 → wgpu é bônus, não implementado.

## Divergência 4 — faixas de legenda embutidas (`crates/codecs/src/subtitles.rs`, novo)

**O que muda.** Módulo novo no hub, sem tocar o `MediaInfo` do craft-media:

- `subtitles::probe(&SharedReader)` — lista as faixas de legenda do container (índice, ID, idioma,
  nome, codec): isobmff (handler `sbtl`/`subt`/`text`/`clcp`) e matroska (`TrackType` 0x11);
- `subtitles::extract` / `extract_path` — abre o demuxer já vendorizado e devolve, por faixa, as
  amostras com `start`/`end` em segundos e o payload cru, em ordem de tempo;
- `SubtitleCodec` diz como interpretar o payload: `S_TEXT/UTF8` → `SubRip`;
  `S_TEXT/WEBVTT` (e `D_WEBVTT/*`) → `WebVtt`; `S_TEXT/ASS`/`S_TEXT/SSA` (com o `CodecPrivate`
  como cabeçalho) → `Ass`; `tx3g`/`text` → `Tx3g`; qualquer outro (PGS/DVD bitmap) fica `Other`,
  listado mas não suportado.

**Por quê.** O hub do FilmCraft é do editor e só expõe vídeo/áudio no `MediaInfo`; o player precisa
da legenda que o mpv lia ("tem nesses arquivos"). O player consome `filmcraft-codecs` como
biblioteca, então a superfície nova fica aqui — o `craft-media` vendored não é alterado.

**Decisão de desenho.** O fork **não interpreta texto**: entrega o payload cru + os tempos do
container (o único que sabe o timing real: `stts`/edit list no MP4, `BlockDuration`/`TimestampScale`
no MKV). Os parsers (SRT/VTT/ASS do app e o `tx3g` novo) ficam em `src/subtitle/`, onde o render
vive. Matroska é aberto com `OpenOptions { index: true }` (scan de cabeçalhos de cluster, nunca o
payload do vídeo); o app roda a extração numa thread própria.

**Testes no fork.** `extrai_legenda_subrip_de_mkv` gera um MKV com o próprio `MkvWriter` (faixa
`S_TEXT/UTF8`, `language=por`) e confere idioma, codec e tempos (1 s→3 s, 4 s→5,5 s);
`mapeia_codecs_de_legenda` cobre o mapeamento. Rodados com `cargo test -p filmcraft-codecs` no
workspace do fork.

**Proposta de upstream (não submetida).** `MediaInfo` ganhar `subtitles: Vec<SubtitleStreamInfo>`
e um `MediaSource::subtitle(track, time)`; enquanto isso o player usa este módulo, que não muda
nenhum comportamento existente (`probe` e `extract` são opt-in).

## Divergência 5 — empty edit de corte por stream copy (`crates/codecs/src/mp4.rs`)

**O que muda.** O `Mp4Source` ignora o *empty edit* inicial longo (`media_time = -1`, mais de
`CUT_LEAD_IN_SECS` = 1 s) que um `ffmpeg -ss … -t … -c copy` deixa no `elst`:

- `ignore_leading_empty_edits` subtrai o vazio do `edit_offset` de cada faixa;
- `presentation_duration` deixa de somar esse vazio (a duração passa a ser a soma dos edits que
  tocam, que coincide com o `mdhd` no arquivo cortado);
- o empty edit **curto** (o de um frame que alguns encoders escrevem para o atraso de reordenação)
  continua valendo — o teste upstream `duration_follows_the_edit_list` segue passando.

**Evidência no arquivo do João.** `Ado … - corte.mp4` (23.542.071 bytes):

- `mvhd`: timescale 441.000.000, duration 132.147.210.000 = **299,654 s** (duração original);
- `mdhd`: vídeo **244,211 s**, áudio 244,320 s, legenda `tx3g` 270,749 s;
- `elst` do vídeo: `empty = 24.426.402.000` (movie ts) = **55,389 s**, depois 244,211 s de mídia
  com `media_time = 1001`; o do áudio: empty 55,334 s + 244,320 s;
- **nenhuma amostra além do EOF**: a última amostra de vídeo termina em 23.283.792, a de áudio em
  23.285.650, o arquivo tem 23.542.071 bytes (a hipótese "índice além do truncado" não se aplica a
  este arquivo — o que ficou herdado foi o `elst`, não offsets).

**Sintoma.** O demux aplica o vazio no `edit_offset`, então a posição 0 de apresentação cai antes
do primeiro sample: o GopCache devolve o **primeiro frame** e o vídeo fica congelado durante os
55,4 s do vazio (o áudio segue em paralelo) enquanto a barra de seek mostra 5:00. Depois do fix:
`--probe` diz **4:04.211** e o conteúdo anda desde o primeiro segundo (o `.mkv` do mesmo take nunca
foi afetado: Matroska não tem edit list; `--probe` sempre deu 4:04.320).

**Teste.** `leading_empty_edit_does_not_freeze_the_clip` gera um MP4 com o próprio `Mp4Writer`
(2 s de empty edit + 1 s de AAC) e confere duração (~1 s, não 3 s) e áudio não-silencioso em t=0;
a suíte do `filmcraft-codecs` passa (137 testes).

**Patch.** [`0002-mp4-leading-empty-edit.patch`](0002-mp4-leading-empty-edit.patch) — validado com
`git apply --check -p1` contra o `mp4.rs` do snapshot `8fda2f2`.

**Texto de PR (inglês, pronto para colar):**

```
codecs: ignore the empty edit a stream-copy cut leaves at the start

`ffmpeg -ss … -t … -c copy` (and similar stream-copy trims) can write an `elst`
whose presentation starts with an empty edit for the discarded span while the
media samples still begin at zero. The movie header keeps the pre-cut duration,
and the demuxer applies the empty edit to `edit_offset`, so media time zero maps
before the first sample: a frame lookup clamps to the first picture and a player
shows it frozen for the whole empty span, with the stale duration in its seek
bar. Matroska files from the same trim are unaffected (no edit list), which is
why the bug only showed on the MP4 side.

Treat a leading empty edit longer than one second as a cut artifact: subtract it
from `edit_offset` and stop counting it in `presentation_duration`. Short empty
edits (the one-frame reorder delay some encoders write, covered by
`duration_follows_the_edit_list`) and `media_time`/priming edits keep their
current meaning.

Test: `leading_empty_edit_does_not_freeze_the_clip` muxes an AAC track with a
2 s empty edit plus 1 s of media and checks the clip lasts its media duration and
that audio at t=0 is not the empty edit's silence.
```

## Divergência 6 - teto de threads dos decoders e do pool do `gop` (`crates/codecs/src/{gop,lib}.rs` + `crates/{h264,hevc,vp9,av1}/src/decoder.rs`)

**O que muda.** Um limite de threads configurável pelo processo host, em dois níveis:

- `gop::set_decode_pool_threads(n)` (NOVO) — threads do pool que AGENDA o decode de cada cache
  (`decode_pool`), que antes era sempre `available_parallelism`;
- `codecs::set_decoder_threads(n)` (NOVO) — repassa para `set_threads_cap` de cada codec
  (`filmcraft-h264/hevc/vp9/av1`), que passam a limitar o `default_threads()` dos pools
  `h264-*`/`hevc-*`/`vp9-*`/`av1-*` (os que realmente decodificam em paralelo).

`0` = comportamento original em todos. Testes: `gop::tests::a_host_thread_cap_shares_the_cpu` e
`h264::decoder::cap_tests`.

**Por quê.** O player divide a máquina com a UI, o áudio e o que mais estiver rodando (rustc,
navegador, OBS). O primeiro diagnóstico (`available_parallelism` no pool do gop) estava
incompleto — medido com `--bench --seconds 30 --hw off`, capar só aquele pool não mudava o fps;
a paralelização de verdade está nos pools dos codecs. Com os dois capados (Ryzen 5 5500U,
12 threads lógicas, MKV 1080p H.264): livre ~309 fps (auto) / 142 (2) / 70 (1); com 3 loops de CPU
240 / 112; com 8 loops 166 / 68. O cap entrega CPU ao resto da máquina ao custo de folga de
decode — para 1080p a 30 fps, 2 threads ainda sobram 4,7×.

**Proposta de upstream (não submetida).** Hoje o upstream só tem controles globais para o cache
de frames; o número de threads de cada decoder é `available_parallelism` fixo. O setter espelha
`set_frame_cache_budget` (global, chamado no arranque, `0` = original). Uma alternativa melhor
para o futuro é prioridade por thread (`spawn_handler`), que não existe no upstream.

## Fora do fork (nem divergência, nem patch)

- **Prioridade da thread de decode.** O app baixa a prioridade da própria thread `video-decode`
  (`SetThreadPriority(THREAD_PRIORITY_BELOW_NORMAL)`, `src/engine/video.rs`) para o decode não
  competir com a UI pelo CPU. O fork não tem ponto de entrada para isso (o pool de decoders é
  interno e não expõe prioridade) e mexer nisso exigiria API nova: **proposta em aberto**, sem
  código preparado aqui — não queríamos mandar patch não exercitado pelo app.
- **Parser `tx3g`/conversão de cues (app).** `src/subtitle/tx3g.rs` lê o `u16` de tamanho + texto
  UTF-8 e `src/subtitle/embedded.rs` monta SRT/VTT/script ASS sintéticos para os parsers existentes.
  Fica no app porque é onde o render vive; o fork só entrega payload + tempo (divergência 4).

## Limitações conhecidas do patch

- `set_frame_cache_budget` é global (como `FRAME_BUDGET`): duas bibliotecas no mesmo processo com
  janelas diferentes teriam que coordenar. Um `GopCache::with_frame_budget(bytes)` por faixa é a
  evolução natural, mas o player só precisa do global.
- O upstream não tem teste de integração para os setters globais (estado de processo + testes em
  paralelo); mantivemos o teste no nível de `GopCache`, como os totais existentes.
