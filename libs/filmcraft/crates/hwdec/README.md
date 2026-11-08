# filmcraft-hwdec

Divulgação: os arquivos de `src/` (exceto `codec.rs` e `lib.rs`, adaptados) são de
**storytold/filmcraft `crates/platform`** (MIT OR Apache-2.0), snapshot `8fda2f2`. Este crate é um
recorte para uso como biblioteca pelo Local Player RS.

## O que é

Decoder de vídeo por hardware do sistema operacional na frente dos decoders pure-Rust do
`filmcraft-codecs`, com fallback transparente. Hoje **Windows**: Media Foundation + Direct3D 11 /
DXVA para H.264, HEVC (Main/Main 10), VP9 (profile 0/2) e AV1 (profile 0), em 4:2:0.

```text
avcC / hvcC sample ──► Annex B ──► decoder MFT ──DXVA──► NV12 / P010 texture
                                                            │ readback (GPU → CPU, staging)
                                                            ▼
                                    planar Yuv8 / Yuv16 VideoFrame ──► HybridDecoder
```

- `register()` põe a factory na frente dos decoders do hub; streams que ela recusa continuam nos
  decoders pure-Rust, e um que falhe no meio cai para o software (`HybridDecoder`) com uma linha
  de log. Nenhum arquivo fica indecodificável por causa do hardware.
- Os quadros saem no formato do decoder de software (mesmos planos, cor, PAR, PTS e ordem de
  apresentação), então os dois são intercambiáveis.
- `probe()` responde o que um `--probe` pode dizer sem abrir decoder: família + bit depth +
  existência de MFT e de perfil DXVA na GPU ("provável").
- `codec.rs` é a decisão pura (família, bits, política, registro) — testável sem GPU.

## O que ficou de fora (de propósito)

- **VA-API / NVDEC (Linux) e VideoToolbox (macOS)** — próximas unidades. O `register()` por
  plataforma e o enum `Availability` já são o ponto de extensão.
- **Encoders de hardware** (NVENC, VideoToolbox encode) e tudo que o `filmcraft-platform` do
  editor fazia com project/render/export.
- **Zero-copy D3D11 → wgpu**: o readback GPU→CPU→RAM é o único caminho hoje (`gpu::Readback`); o
  upload para a wgpu continua no app. Documentado como pendência.

## Testes

Os testes unitários copiados do upstream (planos recusados, Annex B, biplanar, replay do híbrido)
rodam em qualquer plataforma; a integração real de Media Foundation é manual (`--bench` no app
reporta o decoder usado).
