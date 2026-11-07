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
- **Legendas externas:** `.srt` (e `.vtt` no subconjunto simples) **carregado
  sozinho** ao lado do vídeo — `filme.srt`, `filme.sub.srt`, `filme.pt-br.srt`,
  `filme.en.srt`, ... O parser tolera BOM UTF-8/UTF-16, CRLF, tags
  `<i>/<b>/<u>` e entradas quebradas (puladas, o arquivo continua).
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
| Esc | sai da tela cheia |

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
