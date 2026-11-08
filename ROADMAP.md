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
- [x] **5c. Legenda embutida + chrome da playlist (dev, pós-v0.2.1)** — faixas de legenda do
      próprio container (MKV `S_TEXT/UTF8|ASS|WEBVTT` e MP4 `tx3g`): auto-seleção pelo idioma do
      app/sistema (primeira como fallback), externa vence embutida, S alterna e C cicla faixas;
      extração em thread própria. A superfície vem do fork (`divergência 4` em
      `docs/upstream/fork-patches.md`); `--probe` passou a listar as faixas. Painel `Itens` com a
      altura do conteúdo (1 item não deixa buraco) e cap de 4 linhas com scroll interno; relógio e
      botão CC ganharam respiro próprio na barra. Inclui o fix do **empty edit de corte por
      stream-copy** no MP4 (divergência 5): o `corte.mp4` do teste ficava com o primeiro frame
      congelado por 55 s e duração 5:00 herdada do arquivo original; agora abre em 4:04 e toca
      desde o primeiro segundo.
- [ ] **8. Maturação** — tonemap HDR, VFR, extração do motor como crate, contribuição dos
      patches upstream (`docs/upstream/`). Legenda embutida: feita na 5c.
- [x] **9. Janela no tamanho do vídeo** (pedido do João; dev, pós-v0.3.0) — na abertura de uma
      faixa de vídeo a janela vai para o tamanho de EXIBIÇÃO do vídeo (PAR anamórfico + rotação
      do usuário), 1:1 — pixels do vídeo = pixels da tela — somando o chrome INTERNO do app
      (topo + controles + lista) e com cap na área útil (`lab_ui::workarea::fit_to_work_area_with_chrome`:
      vídeo maior que o monitor abre no maior tamanho que cabe, nunca numa janela maior que a
      tela). **Decisão:** o ajuste só AGE quando a resolução de exibição MUDA — episódios do
      mesmo tamanho não re-ajustam a janela, então um resize manual do usuário sobrevive à
      playlist; faixas de tamanhos diferentes voltam ao 1:1 (como o player antigo abria). Áudio
      puro e fullscreen não mexem na janela; rotação não redimensiona (o R é do usuário). A math
      pura ganhou testes (`fit_size` em `lab-ui/src/workarea.rs`, `window_video_px` em
      `src/main.rs`).
- [x] **10. Avaliação low-end (dev, pós-v0.3.0)** — medido com `--bench --seconds 30 --hw off`
      (decode SW; o MF/DXVA não usa os pools) no MKV 1080p H.264 do teste, Ryzen 5 5500U (6C/12T),
      mediana de 3 repetições:
      | threads de decode | máquina livre | 3 loops de CPU | 8 loops (≈ rustc) |
      | --- | --- | --- | --- |
      | auto (12) | ~309 fps | ~239 fps (−23%) | ~166 fps (−46%) |
      | 2 | ~142 fps | ~112 fps (−24%) | ~68 fps (−54%) |
      | 1 | ~70 fps | — | — |
      **Primeiro achado (corrigiu o diagnóstico):** o pool que o `gop` do fork cria (agenda cada
      decode) NÃO é o que consome CPU — capá-lo sozinho (12 vs 2 vs 1) dava o mesmo fps. A
      paralelização real está nos pools dos PRÓPRIOS codecs (`h264-*`, `hevc-*`, `vp9-*`, `av1-*`,
      gerados de `available_parallelism`, máx. 16). O limite agora vale para os dois
      (`set_decode_pool_threads` + `set_decoder_threads` no fork), e aí o trade-off aparece:
      12→2 threads corta o fps pela metade e 2→1 pela metade de novo (escala ~2,2×).
      **Leitura de máquina fraca:** 1080p a 29,97 fps precisa de 30 fps → 1 thread = 2,3× de
      folga, 2 threads = 4,7×, auto = 10×. Em H.264 1080p, `LOCALPLAYER_DECODE_THREADS=2` é o
      ponto doce: libera CPU (10 threads de sobra para UI/áudio/compilador) mantendo folga de
      quase 5×. Não extrapolar para 4K/HEVC sem medir (a folga encurta e o pool faz mais
      diferença). Sob contenção o cap NÃO recupera throughput (12 threads ainda vence 2): o
      ganho é ceder CPU, não decodificar mais rápido.
      Avaliado e **não** implementado (sem número que justifique): *pacing de 250 ms* — só existe
      com o player parado/pausado; tocando, o repaint segue o deadline do frame, então não muda
      nada em máquina fraca; *repaint sob demanda* já existe; *prioridade* — a thread
      `video-decode` já é below-normal, mas as threads dos pools (gop e codecs) ficam em
      prioridade normal: candidata futura (`spawn_handler`), não medida porque o bench não tem UI.
      Caveat: o bench roda sem UI/GPU — o ganho de responder a UI com o rustc comendo CPU precisa
      de QA manual no notebook.
- [x] **11. TLS 100% Rust no update (experimento, dev)** — o `craft-update` vendored deste app usa
      o **fork vendored** do `rustls-rustcrypto 0.0.2-alpha` (`libs/rustls-rustcrypto`, MIT OR
      Apache-2.0) como provider ÚNICO do rustls (check de release + download do instalador).
      **Sem fallback**: não existe válvula nem segundo conjunto de agentes — handshake que falhe
      vira erro (linha de status do "Verificar atualizações" + `event=update_check_failed` no log,
      com `tls_provider=pure` na linha anterior).
      O fork existe para tirar o `rustls-webpki 0.102` do grafo: o upstream só o usava pelas
      constantes `alg_id`, que hoje moram no `rustls-pki-types` 1.14 (ver
      `docs/experimento-tls-pure.md`).
      `cargo tree -i ring` e `cargo tree -i aws-lc-sys`: **fora do grafo** — o ureq sobe com
      `rustls-no-provider` (sem `_ring`) e o provider vai sempre explícito no `TlsConfig`.
      Teste real (`cargo test --test tls_update_check -- --ignored --nocapture`): o handshake com
      `api.github.com` negociou **TLSv1.3 + TLS13_AES_128_GCM_SHA256** e o `check_for_update`
      achou a release **v0.4.0** (4 assets) — o provider puro funciona com o GitHub.
      `cargo deny`: os 5 ignores do webpki 0.102 saíram; sobram 2, sem correção possível hoje
      (`RUSTSEC-2023-0071` do `rsa`, `patched = []` também no 0.10-rc; `RUSTSEC-2026-0387` do
      `crypto-bigint 0.5`, que o rsa 0.9 exige), justificados no `deny.toml` (o cliente só verifica
      assinatura com chave pública, nunca opera chave privada). Custo: +~20 crates RustCrypto.
      Fora do escopo por decisão: não estender aos outros apps da suíte.

## Pendências conhecidas

- **hwdec 4K HEVC** — MF/DXVA cobre Main/Main 10 e VP9/AV1; falta validar 4K HEVC real em
  hardware (o SW pode não segurar sozinho, e o readback GPU→CPU cresce com a resolução).
- **HDR → SDR tonemap** — vídeo HDR sai sem conversão (cores lavadas).
- **VFR** — o pipeline assume a taxa nominal declarada pelo container.
- **ASS** — básico (styles/cores/\an/contorno); `\pos`/`\move`/`\fad`/karaokê/`\t`/desenho
  ignorados (avisos no stderr); sem colisão de linhas nem quebra automática.
- **Hot-swap de device de áudio** — trocar a saída exige reabrir faixa/app.
- **Gapless** — auto-avanço da playlist funciona, mas sem gapless fora de MP3.
- **Resize do painel `Itens`** — o painel acompanha o conteúdo com cap fixo de 4 linhas + scroll;
  sem grip de redimensionamento (o `Panel` do craft não persiste um tamanho que conviva com a
  altura automática do conteúdo). Pedido do João: avaliar quando o padrão craft expuser isso.
- **Legenda embutida** — suporta texto (SRT/VTT/ASS do MKV e tx3g do MP4). Faixas bitmap
  (PGS/VobSub) são listadas no `--probe` e ignoradas no player; `wvtt` em MP4 (amostras em caixas)
  ainda não é interpretado.
- **Janela 1:1 e chrome dinâmico** — o resize mede o chrome na abertura e recalcula em até 3
  passadas (a janela inicial é estreita e as barras quebram linha, o chrome muda depois do
  resize). Se o chrome mudar DEPOIS (linha de status da legenda, item novo na lista), o vídeo fica
  com uma borda de poucos px até a próxima faixa — perseguir o chrome por frame faria a janela
  "respirar" sozinha, então não.
- **TLS do update** — ver a fase 11: provider puro (fork vendored) sem fallback, `ring`/`aws-lc-sys`
  fora do grafo e 2 advisories ignoradas no `deny.toml` (`RUSTSEC-2023-0071` do `rsa`, sem patch
  nem no 0.10-rc; `RUSTSEC-2026-0387` do `crypto-bigint 0.5`, que só sai migrando para rsa 0.10).
  Revisitar quando o upstream do `rustls-rustcrypto` migrar de `rsa`.

## Dívida técnica

- Sync A/V e seek GOP-aware são o trabalho duro; os codecs em si não.
- Perf SW 4K HEVC pode exigir hwdec antes do esperado.
- `libs/craft-libs` vendored (rev local) ≠ rev b926b5c que o FilmCraft espera:
  divergências de API se resolvem **no fork** do FilmCraft, nunca no craft-libs.
