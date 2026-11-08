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
- [x] **5. Release de teste** — v0.1.0 testada; v0.1.1 (fixes de pipeline) **aprovada
      pelo João 2026-11-07**: MKV 1080p H.264 liso, sem travadas. Bench: decode a
      328 fps (11x de folga).
- [x] **5b. Legendas completas + UI (v0.1.2)** — VTT completo (settings/NOTE/tags),
      ASS básico (styles, cores, \an, contorno/caixa; sem \pos/\fad/karaokê — ver
      limitações em src/subtitle/ass.rs), mouse esconde junto com os controles,
      overlay tabular com pos/dura, letterbox borda-a-borda (tolerância 1%).
- [x] **7a. hwdec no Windows** — Media Foundation + DXVA como **primeira opção**,
      extraído do `filmcraft-platform` para o crate `filmcraft-hwdec` (sem as deps do
      editor: project/render/export/encoders), com fallback SW pure-Rust transparente
      (`HybridDecoder`). Overlay D e log de sessão mostram o decoder usado; `--probe` diz
      "provável"; `--bench --hw off` mede o baseline. MKV 1080p H.264 (Ado): MF
      **448 fps / 2,23 ms / p95 4,9 ms** vs SW **354 fps / 2,83 ms / p95 15,0 ms** (~27%
      mais vazão, p95 3x menor). O readback GPU→CPU é o teto; zero-copy D3D11→wgpu é
      pendência.
- [ ] **7b. hwdec Linux/macOS** — VA-API/NVDEC e VideoToolbox no mesmo crate
      (`register()` por plataforma); SW puro até lá.
- [ ] **8. Maturação** — legenda embutida no MKV (requer exposição de faixa no hub),
      tonemap HDR, VFR, extração do motor como crate, contribuição dos patches
      upstream (`docs/upstream/`).

## Pendências conhecidas

- **hwdec 4K HEVC** — MF/DXVA cobre Main/Main 10 e VP9/AV1; falta validar 4K HEVC real em
  hardware (o SW pode não segurar sozinho, e o readback GPU→CPU cresce com a resolução).
- **HDR → SDR tonemap** — vídeo HDR sai sem conversão (cores lavadas).
- **VFR** — o pipeline assume a taxa nominal declarada pelo container.
- **ASS** — básico (styles/cores/\an/contorno); `\pos`/`\move`/`\fad`/karaokê/`\t`/desenho
  ignorados (avisos no stderr); sem colisão de linhas nem quebra automática.
- **Hot-swap de device de áudio** — trocar a saída exige reabrir faixa/app.
- **Gapless** — auto-avanço da playlist funciona, mas sem gapless fora de MP3.

## Dívida técnica

- Sync A/V e seek GOP-aware são o trabalho duro; os codecs em si não.
- Perf SW 4K HEVC pode exigir hwdec antes do esperado.
- `libs/craft-libs` vendored (rev local) ≠ rev b926b5c que o FilmCraft espera:
  divergências de API se resolvem **no fork** do FilmCraft, nunca no craft-libs.
