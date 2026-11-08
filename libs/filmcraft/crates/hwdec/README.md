# filmcraft-hwdec

Divulgação: os arquivos de `src/` (exceto `codec.rs` e `lib.rs`, adaptados) são de
**storytold/filmcraft `crates/platform`** (MIT OR Apache-2.0), snapshot `8fda2f2`. Este crate é um
recorte para uso como biblioteca pelo Local Player RS.

## O que é

Decoder de vídeo por hardware do sistema operacional na frente dos decoders pure-Rust do
`filmcraft-codecs`, com fallback transparente.

- **Windows**: Media Foundation + Direct3D 11 / DXVA para H.264, HEVC (Main/Main 10), VP9
  (profile 0/2) e AV1 (profile 0), em 4:2:0.
- **Linux**: VA-API (`vaapi`; Intel, AMD e outros drivers Mesa/vendor) e NVDEC (`nvdec`; driver
  proprietário da NVIDIA) para H.264 8-bit e HEVC 8/10-bit, 4:2:0 progressivo. O VA-API faz o parse
  no host com os parsers do decoder de software (`filmcraft-h264`/`filmcraft-hevc`); o NVDEC usa o
  parser do próprio driver (`libnvcuvid`) sobre Annex B e lê de volta pelo driver CUDA.

```text
Windows: avcC / hvcC sample ──► Annex B ──► decoder MFT ──DXVA──► NV12 / P010 texture
                                                                      │ readback (GPU → CPU)
                                                                      ▼
                                              planar Yuv8 / Yuv16 VideoFrame ──► HybridDecoder

Linux:   avcC / hvcC sample ──► parse (host) ──► VABuffers ──► VA-API surface ──► vaGetImage
         (ou)                   ──► Annex B ──► libnvcuvid ──► frame ──► cuMemcpyDtoH
                                                                      ▼
                                              planar Yuv8 / Yuv16 VideoFrame ──► HybridDecoder
```

- `register()` põe as factories na frente dos decoders do hub; streams que elas recusam continuam
  nos decoders pure-Rust, e um que falhe no meio cai para o software (`HybridDecoder`) com uma linha
  de log. Nenhum arquivo fica indecodificável por causa do hardware.
- Os quadros saem no formato do decoder de software (mesmos planos, cor, PAR, PTS e ordem de
  apresentação), então os dois são intercambiáveis.
- `probe()` responde o que um `--probe` pode dizer sem abrir decoder: família + bit depth + backend
  existente e, no Windows, MFT e perfil DXVA na GPU; no Linux o driver VA-API é consultado por uma
  configuração (sem surfaces nem contexto) ("provável").
- `codec.rs` é a decisão pura (família, bits, política, registro, famílias por plataforma) —
  testável sem GPU.

## Como o VA-API é carregado sem linkar libva

`libva.so.2` e `libva-drm.so.2` são abertos em tempo de execução com `libloading`; as declarações
(`vaapi::ffi`) são transcritas dos headers MIT da libva e **nenhum header ou biblioteca é
necessário para compilar**. Sem libva (ou sem `/dev/dri/renderD*`, ou sem driver), `probe()` devolve
o motivo e o stream decodifica em software. O mesmo vale para `libnvcuvid.so.1` e `libcuda.so.1`.

## O que ficou de fora (de propósito)

- **VideoToolbox (macOS)** — próxima unidade (se o player for para macOS). O `register()` por
  plataforma e o enum `Availability` já são o ponto de extensão.
- **Encoders de hardware** (NVENC, VideoToolbox encode) e tudo que o `filmcraft-platform` do
  editor fazia com project/render/export. Do `nvenc::device` do upstream só o contexto CUDA foi
  trazido (`nvdec::device`), porque é por ele que o NVDEC lê os quadros de volta.
- **Zero-copy D3D11 → wgpu** / **VA-API surface → wgpu**: o readback GPU→CPU→RAM é o único caminho
  hoje; o upload para a wgpu continua no app. Documentado como pendência.

## Como validar numa máquina Linux (com GPU/driver)

```bash
# 0. (opcional) confirma o driver e os perfis VA-API fora do app
vainfo

# 1. testes do crate: ABI (sempre) + front ends H.264/HEVC contra o decoder SW (precisa de ffmpeg)
cargo test -p filmcraft-hwdec

# 2. o que o diagnóstico responde para um arquivo: "hwdec provável (VA-API takes H.264 8-bit on this driver)"
cargo run --locked -- --probe /caminho/video.mkv

# 3. integração real: o bench reporta o backend e se os quadros saíram do hardware
cargo run --release --locked -- --bench /caminho/video.mkv
#    esperado: hwdec: on (VA-API)   (ou on (NVDEC) numa GPU NVIDIA)
#              decoder: VA-API H.264 (Intel iHD driver ...) (hardware)

# 4. fallback sem VA-API: rode com um usuário sem acesso a /dev/dri (ou numa VM sem GPU);
#    o register() devolve Unavailable e o --probe diz "não (no VA-API driver: ...)".
```

Acesso a `/dev/dri/renderD*` é requisito (grupo `render` / `video`, ou uma sessão gráfica);
sem ele `vaGetDisplayDRM` falha e o decode cai em software — nunca em erro.

## Testes

- Os testes unitários copiados do upstream (planos recusados, Annex B, biplanar, replay do híbrido)
  rodam em qualquer plataforma.
- `vaapi/abi_tests.rs` confere tamanhos, alinhamentos, offsets e posições de bit-field das
  estruturas FFI contra os headers (valores gerados com GCC) — em qualquer plataforma.
- `vaapi/tests.rs` roda os front ends H.264/HEVC contra um `Accel` que grava e confere os buffers,
  comparando a ordem/PTS com o decoder de software; precisa de ffmpeg e de fixtures (gera
  x264/x265 na hora), e sem ffmpeg os testes são pulados.
- A integração real (GPU) é manual: `--bench` no app reporta o decoder usado.
