# ROADMAP — Local Player RS

Sucessor pure-Rust do Local-Player (mpv + IPC). Plano completo:
`C:\Dev\docs\planos\plano-local-player-rs.md`.

## Fases

- [x] **1. Compilar** — scaffold renomeado (`local-player-rs`), mpv removido,
      vendor do FilmCraft fechado em `libs/filmcraft` (workspace próprio) e
      `cargo check` verde. Na época, nenhum decode: o motor era placeholder.
- [x] **2. Áudio primeiro** — música (mp3/flac/opus/vorbis/wav/m4a) tocando com
      seek, volume/mute e playlist (symphonia/filmcraft + cpal + relógio mestre
      de áudio). Falta a validação auditiva manual do João na fase 5.
- [x] **3. Vídeo SW** — MP4/MOV/MKV/WebM/AVI/TS decodificando e pintando na
      egui (`filmcraft-codecs` + textura wgpu YUV→RGB), sync A/V pelo relógio
      mestre e seek GOP-aware. RGBA (Motion JPEG) fica fora do render.
- [x] **4. Legenda SRT + polimento** — SRT/VTT externo automático, toggle (S),
      atraso (Z/X, no overlay de status), atalhos, rotação e fullscreen.
- [ ] **5. Release de teste** — `cargo xtask version 0.1.0` + tag; João testa.
- [ ] **6. Fase 2 (pós-aprovação)** — hwdec Media Foundation, ASS, VTT completo,
      zero-copy; decisão de aposentar o Local-Player antigo.

## Pendências conhecidas

- **hwdec** Media Foundation — 4K HEVC em software pode não segurar sozinho.
- **HDR → SDR tonemap** — vídeo HDR sai sem conversão (cores lavadas).
- **VFR** — o pipeline assume a taxa nominal declarada pelo container.
- **ASS** (estilizado) fora do escopo; VTT cobre só o subconjunto simples
  (timestamps com ponto, sem estilos/regiões).
- **Hot-swap de device de áudio** — trocar a saída exige reabrir faixa/app.
- **Gapless** — auto-avanço da playlist funciona, mas sem gapless fora de MP3.

## Dívida técnica

- Sync A/V e seek GOP-aware são o trabalho duro; os codecs em si não.
- Perf SW 4K HEVC pode exigir hwdec antes do esperado.
- `libs/craft-libs` vendored (rev local) ≠ rev b926b5c que o FilmCraft espera:
  divergências de API se resolvem **no fork** do FilmCraft, nunca no craft-libs.
