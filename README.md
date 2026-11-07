# Local Player RS

**Player de mídia 100% local e offline em Rust puro.** Candidato a sucessor do
[Local-Player](https://github.com/Anon5T4R/Local-Player) (que roda o mpv como
processo externo): aqui o demux e o decode são crates nossas — o
[FilmCraft](https://github.com/storytold/filmcraft) vendored em `libs/filmcraft`
mais o bootstrap de áudio do symphonia. Sem processo externo, sem download em
runtime, sem telemetria.

> **Estado: fase 4 — legenda e polimento (pré-release de teste v0.1.0).**
> O app abre, gerencia a playlist (arquivo, pasta, arrastar-e-soltar, "abrir
> com"), tema e i18n da suíte, se auto-atualiza, **toca áudio e vídeo de
> verdade** e mostra **legenda externa** por cima da imagem. O motor escolhe o
> frame pelo relógio mestre (áudio ou parede), faz seek GOP-aware e respeita
> pausa/rotação/fullscreen.

## Download

Assets de cada release em
[Releases](https://github.com/Anon5T4R/Local-Player-RS/releases):

| Asset | O que é |
| --- | --- |
| `local-player-rs-<versão>-windows-x64.msi` | Instalador MSI (WiX v5, Program Files, atalhos) |
| `local-player-rs-<versão>-windows-x64-portable.zip` | Portable (um `.exe` só) |
| `local-player-rs-<versão>-linux-x86_64.AppImage` | Linux, AppImage |
| `local-player-rs-<versão>-linux-x86_64.tar.gz` | Linux, portable |

## O que toca

- **Vídeo:** MP4/M4V/MOV, MKV, WebM, AVI e MPEG-TS — H.264, HEVC, AV1, VP8 e
  VP9. Decode em software (sem hwdec ainda), YUV→RGB pelo shader WGSL do wgpu,
  com letterbox e rotação em 90°.
- **Áudio em arquivo:** MP3, FLAC, Opus, Vorbis, WAV/AIFF e M4A (AAC/ALAC).
- **Áudio dentro de container de vídeo:** AAC, MP3, Opus, AC-3/E-AC-3, FLAC,
  Vorbis e PCM — a trilha toca junto com a imagem e é o relógio mestre.
- **Legendas externas:** `.srt`, `.vtt`, `.ass`/`.ssa` **carregadas sozinhas** ao
  lado do vídeo nesta ordem — `filme.srt`, `filme.vtt`, `filme.ass`,
  `filme.sub.srt`, `filme.pt-br.srt`, `filme.en.srt`, ... O parser tolera BOM
  UTF-8/UTF-16, CRLF, tags `<i>/<b>/<u>` e entradas quebradas (puladas, o
  arquivo continua). WebVTT: `align:`/`position:`/`line:` em %, blocos `NOTE`
  ignorados, `<c.classe>` removido. ASS/SSA: estilo do script (fonte, tamanho,
  cores, negrito/itálico, os nove alinhamentos, margens) e overrides comuns
  (`\an`, `\i`, `\b`, `\fs`, `\c`, `\N`); `\pos`/karaokê/desenho ficam fora.
- **Playlist** persistente entre sessões: `+ arquivo` / `+ pasta` (multi-arquivo,
  a listagem roda fora da UI thread — pasta de rede não trava a janela) ou
  arraste os arquivos pra janela; "abrir com" no Explorer também funciona
  (caminhos via args).
- **Autoupdate** via `craft-update`: check no GitHub da própria release
  (Windows: substitui o MSI/portable; Linux: AppImage/tar.gz).
- **Temas light/dark e i18n PT/EN/ES** comuns da suíte (crate `lab-ui`).

## Atalhos

| Tecla | Ação |
| --- | --- |
| Espaço | play/pause |
| ← / → | −5 s / +5 s |
| F | entra/sai da tela cheia |
| R / Shift+R | gira 90° (horário / anti-horário) |
| M | mute |
| S | liga/desliga a legenda |
| Z / X | atraso da legenda −0,5 s / +0,5 s (no overlay de status) |
| D | overlay de estatísticas (decode/exibição/drops/upload/RSS/cache) |
| Esc | sai da tela cheia |

## Diagnóstico

O app sai de fábrica com instrumentação sempre ligada (não depende de build de
debug nem de o usuário perceber nada). Nada é enviado para lugar nenhum: fica
tudo na sua pasta.

**Log automático de sessão** — um arquivo por execução, mantendo os últimos 10
(os mais velhos são apagados):

- Windows: `%APPDATA%\local-player-rs\logs\`
- Linux: `$XDG_DATA_HOME/local-player-rs/logs/` (ou `~/.local/share/local-player-rs/logs/`)

Cada faixa tocada grava uma linha `track` (data/hora UTC, arquivo, container,
codec de vídeo/áudio, resolução, fps, bitrate, duração). As anomalias viram
linhas `event=…`:

| Evento | Significa |
| --- | --- |
| `stall` | o frame ficou > 250 ms atrás do relógio (uma linha por episódio, com pior atraso, duração e drops) |
| `drop_burst` | um tick descartou ≥ 4 frames (o apresentador pulou o atraso) |
| `upload_ms` | subir a textura passou de 30 ms (disputa com a UI) |
| `rss_mb` / `rss_back` | working set acima de 700 MB (e quando volta) |
| `engine_dead` | a thread do motor de áudio morreu |

**Overlay ao vivo (tecla D)**: fps de decode e de exibição, drops (e quantos por
atraso), atraso vs relógio, custo do upload, intervalo de repaint, RSS/pico e
bytes no cache de frames.

**CSV por segundo** (opcional, o mais detalhado): `LOCALPLAYER_STATS=<arquivo.csv>`
— ou `LOCALPLAYER_STATS=1` para gerar em `%TEMP%`. É o artefato mais útil para
investigar uma travada.

**Linha de comando** (não abre janela; no Windows o release se anexa ao console
do PowerShell):

```bash
local-player-rs --probe filme.mkv                  # container, codecs, res, fps, bitrate, duração
local-player-rs --bench filme.mkv --seconds 120    # decode puro: fps, ms/frame (média/p95/máx), por trecho de 10 s, cache, RSS
local-player-rs --version | --help
```

`--bench` passa pelo mesmo caminho de abertura do player e roda sem áudio, GPU
ou egui: separa "o decode não acompanha o tempo real" de "a UI engargala no
upload/repaint". O teto do cache de frames decodificados é ajustável por
`LOCALPLAYER_FRAME_CACHE_MB` (padrão 128; `0` volta ao teto do fork, 384 MB).

> **PowerShell no Windows:** o `.exe` de release é subsistema GUI, então o
> PowerShell não espera o processo terminar (a saída aparece depois do prompt e
> `$LASTEXITCODE` fica vazio). Para uso em script, rode com
> `cmd /c "local-player-rs --bench filme.mkv"` ou
> `Start-Process -Wait -PassThru local-player-rs -ArgumentList '--bench','filme.mkv'`.
> A saída vai para o console (o app se anexa ao console do processo pai).

Anomalias são gravadas quando a UI volta a responder — se a janela congelou por
2 s, a linha `event=stall` aparece com `dur_ms≈2000` logo depois; o log em si
não consegue escrever durante um event loop travado.

Para reportar uma travada, mande o `player-*.log` da execução, a saída de
`--bench` no mesmo arquivo e, se possível, o CSV de `LOCALPLAYER_STATS` da
mesma reprodução.

## Build

```bash
cargo build --release
```

O `Cargo.lock` está commitado; `build.rs` embute o `icon.ico` no binário do
Windows. O workspace do vendor do FilmCraft é fechado em
`libs/filmcraft/Cargo.toml`; `libs/` é `exclude` do workspace do app.

## Arquitetura (alvo)

```
UI (egui/lab-ui, portada do local-player)
 └─ engine thread: demux → decode (filmcraft-codecs) → frame ring
      ├─ vídeo: YUV → textura wgpu pintada no CentralPanel do egui
      ├─ legenda: cue ativa pelo relógio mestre → texto da egui sobre o vídeo
      └─ áudio: cpal consumindo ring buffer (relógio mestre = áudio)
```

Licença: GPL-3.0-or-later.
