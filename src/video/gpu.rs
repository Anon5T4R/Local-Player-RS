//! Rasterização do frame de vídeo na GPU do eframe/wgpu.
//!
//! Um shader WGSL faz YUV → R'G'B' (a matriz vem do container, ver
//! [`crate::video::color`]) e a rotação em quartos vem na transformação de UV.
//! O frame é pintado por um **paint callback** do egui (sem roundtrip por CPU
//! dos pixels convertidos) num retângulo letterboxed.
//!
//! Um único [`VideoGpu`] é compartilhado entre a UI (upload) e o callback
//! (paint) por `Arc<Mutex<…>>`; as texturas só são recriadas quando o tamanho,
//! o croma ou o formato de amostra muda.
//!
//! ## Profundidade de bits
//!
//! 8 bits usa `R8Unorm`; 10/12 bits usa `R16Unorm` e preserva a precisão real
//! até o tonemap (o caminho antigo shiftava para 8 bits na thread de decode e
//! o PQ em 8 bits fazia banding nas cenas escuras). `R16Unorm` exige a feature
//! `TEXTURE_FORMAT_16BIT_NORM` — pedida no arranque (src/main.rs); num device
//! que não a tenha o upload cai para `R8Unorm` (high byte da amostra), sem
//! quebrar o playback. O overlay da tecla D mostra o caminho em uso.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use eframe::egui_wgpu::wgpu;
use eframe::egui_wgpu::{CallbackResources, CallbackTrait};

use super::color::yuv_to_rgb;
use super::geometry::{self, Rect};
use super::tone::{self, Operator, ToneMapMode, ToneMapParams};
use super::{chroma_dims, Frame, Planes};

/// Uma textura plana do frame atual.
struct Textures {
    y: wgpu::Texture,
    u: wgpu::Texture,
    v: wgpu::Texture,
    size: (u32, u32),
    chroma: filmcraft_frame::Chroma,
    format: wgpu::TextureFormat,
}

struct Inner {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
    textures: Option<Textures>,
    /// Cor do último frame (para reescrever o uniform na rotação).
    color: filmcraft_color::ColorInfo,
    rotation: u8,
    /// Tamanho do último frame e PAR: a rotação recalcula o aspecto mesmo sem
    /// upload novo (pausa), mantendo o letterbox do overlay de legenda igual.
    size: (u32, u32),
    par: (u32, u32),
    /// Alvo `*Srgb`: o shader entrega luz linear e o hardware codifica de volta.
    linearize: bool,
    /// Pico default do PQ (env `LOCALPLAYER_HDR_PEAK`, lido no arranque).
    tone_peak: f32,
    /// Operador de tonemap (env `LOCALPLAYER_HDR_TONEMAP`; default BT.2390).
    tone_operator: Operator,
    /// Pico sinalizado pelo container (`MaxCLL`/mastering display) do frame
    /// atual; vence o env quando existe.
    frame_peak: Option<f32>,
    /// `R16Unorm` é aceito por este device (`TEXTURE_FORMAT_16BIT_NORM`).
    unorm16: bool,
    /// Formato do último upload (rótulo da origem, bits e textura) para o overlay.
    pixel: Option<(String, u32, wgpu::TextureFormat)>,
    /// Maior lado que o device aceita (≤ 8K).
    max_side: u32,
}

impl Inner {
    /// Parâmetros de HDR do frame atual (SDR → `Off`, sem tonemap).
    fn tone_params(&self) -> ToneMapParams {
        ToneMapParams::for_color(&self.color, self.frame_peak.unwrap_or(self.tone_peak), self.tone_operator)
    }

    fn write_uniform(&self) {
        let (uv, offset) = geometry::uv_transform(self.rotation);
        let matrix = yuv_to_rgb(&self.color);
        let tone = self.tone_params().uniform_row();
        self.queue.write_buffer(&self.uniform, 0, &matrix.uniform_bytes(uv, offset, self.linearize, tone));
    }

    /// (Re)cria texturas, views e bind group quando tamanho, croma ou formato
    /// de amostra mudam.
    fn ensure_textures(&mut self, width: u32, height: u32, chroma: filmcraft_frame::Chroma, format: wgpu::TextureFormat) {
        if let Some(t) = &self.textures {
            if t.size == (width, height) && t.chroma == chroma && t.format == format {
                return;
            }
        }
        let (cw, ch) = chroma_dims(width, height, chroma);
        let plane = |w: u32, h: u32, label: &str| -> wgpu::Texture {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let y = plane(width, height, "video-y");
        let u = plane(cw, ch, "video-u");
        let v = plane(cw, ch, "video-v");
        let (y_view, u_view, v_view) = (y.create_view(&Default::default()), u.create_view(&Default::default()), v.create_view(&Default::default()));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video-yuv-bind-group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&y_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&u_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&v_view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        self.textures = Some(Textures { y, u, v, size: (width, height), chroma, format });
        self.bind_group = Some(bind_group);
    }
}

/// Estado de render do vídeo, compartilhado entre a UI e o paint callback.
#[derive(Clone)]
pub struct VideoGpu {
    inner: Arc<Mutex<Inner>>,
    /// Aspecto de exibição do frame atual (PAR e rotação aplicados).
    aspect: Arc<Mutex<f32>>,
}

/// Falha ao subir um frame: dimensão proibitiva (vira erro claro na UI) ou
/// frame/estado inválido (tratado como erro de decode).
#[derive(Debug, Clone)]
pub enum UploadError {
    /// Passa do limite de textura do device (ou do limite de 8K).
    TooLarge(String),
    /// Planos truncados ou estado interno inconsistente.
    Invalid(String),
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(d) | Self::Invalid(d) => f.write_str(d),
        }
    }
}

/// HDR → SDR ativo no frame atual (o overlay da tecla D mostra).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneMapStatus {
    pub mode: ToneMapMode,
    pub operator: Operator,
    pub peak_nits: f32,
}

impl ToneMapStatus {
    /// Linha do overlay, no inglês técnico da tecla D.
    pub fn label(self) -> String {
        format!("{} -> sdr {} ({:.0} nits)", self.mode.label(), self.operator.label(), self.peak_nits)
    }
}

/// Planos de um frame 16-bit quando o device não aceita `R16Unorm`: fica o byte
/// alto de cada amostra little-endian — o código de 8 bits que o pipeline antigo
/// já entregava (mesma convenção, ver [`crate::video::Planes`]).
fn downconvert(planes: &Planes) -> [Arc<Vec<u8>>; 3] {
    let high = |plane: usize| -> Arc<Vec<u8>> { Arc::new(planes.bytes(plane).iter().skip(1).step_by(2).copied().collect()) };
    [high(0), high(1), high(2)]
}

impl VideoGpu {
    /// Cria pipeline/texturas no device do eframe. `None` se o app não roda em wgpu.
    pub fn new(render_state: &eframe::egui_wgpu::RenderState) -> Option<Self> {
        let device = &render_state.device;
        let queue = &render_state.queue;
        let format = render_state.target_format;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video-yuv-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("yuv.wgsl"))),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video-yuv-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { has_dynamic_offset: false, min_binding_size: None, ty: wgpu::BufferBindingType::Uniform },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video-yuv-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video-yuv-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Cw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::REPLACE), write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video-yuv-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video-yuv-uniform"),
            size: 6 * 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let max_side = device.limits().max_texture_dimension_2d.min(super::MAX_DIMENSION);
        let unorm16 = device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
        Some(Self {
            inner: Arc::new(Mutex::new(Inner {
                device: device.clone(),
                queue: queue.clone(),
                pipeline,
                layout,
                sampler,
                uniform,
                bind_group: None,
                textures: None,
                color: filmcraft_color::ColorInfo::REC709,
                rotation: 0,
                size: (0, 0),
                par: (1, 1),
                linearize: format.is_srgb(),
                tone_peak: tone::env_peak_nits(),
                tone_operator: tone::env_operator(),
                frame_peak: None,
                unorm16,
                pixel: None,
                max_side,
            })),
            aspect: Arc::new(Mutex::new(1.0)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sobe o frame para as texturas e atualiza a matriz de cor/rotação.
    ///
    /// `hdr_peak_nits` é o pico sinalizado pelo container para este frame
    /// (`MaxCLL`/mastering display); `None` deixa o env/default do PQ decidir.
    pub fn upload(&self, frame: &Frame, par: (u32, u32), rotation: u8, hdr_peak_nits: Option<f32>) -> Result<(), UploadError> {
        let mut g = self.lock();
        let (w, h) = (frame.width, frame.height);
        if w == 0 || h == 0 {
            return Err(UploadError::Invalid("decoded frame has no size".into()));
        }
        if w > g.max_side || h > g.max_side {
            return Err(UploadError::TooLarge(format!("{w}×{h} exceeds the {} px limit", g.max_side)));
        }
        let (cw, ch) = chroma_dims(w, h, frame.chroma);
        let (need_y, need_c) = (w as usize * h as usize, cw as usize * ch as usize);
        if frame.planes.samples(0) < need_y || frame.planes.samples(1) < need_c || frame.planes.samples(2) < need_c {
            return Err(UploadError::Invalid("decoded frame planes are truncated".into()));
        }
        // 16 bits só quando a fonte tem >8 bits E o device aceita R16Unorm.
        let u16_source = matches!(frame.planes, Planes::U16(_));
        let unorm16 = u16_source && g.unorm16;
        let format = if unorm16 { wgpu::TextureFormat::R16Unorm } else { wgpu::TextureFormat::R8Unorm };
        g.ensure_textures(w, h, frame.chroma, format);
        let (y, u, v) = match &g.textures {
            Some(t) => (t.y.clone(), t.u.clone(), t.v.clone()),
            None => return Err(UploadError::Invalid("video textures missing".into())),
        };
        // Sem R16Unorm: 16 bits descem para 8 aqui (alloc própria, só nesse device).
        let down = (u16_source && !unorm16).then(|| downconvert(&frame.planes));
        let bytes = |plane: usize| -> &[u8] { down.as_ref().map_or_else(|| frame.planes.bytes(plane), |d| d[plane].as_slice()) };
        let bpp = if unorm16 { 2 } else { 1 };
        let write = |texture: &wgpu::Texture, data: &[u8], tw: u32, th: u32| {
            g.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(tw * bpp), rows_per_image: Some(th) },
                wgpu::Extent3d { width: tw, height: th, depth_or_array_layers: 1 },
            );
        };
        write(&y, bytes(0), w, h);
        write(&u, bytes(1), cw, ch);
        write(&v, bytes(2), cw, ch);
        g.color = frame.color;
        g.rotation = rotation % 4;
        g.size = (w, h);
        g.par = par;
        g.frame_peak = hdr_peak_nits.filter(|p| p.is_finite() && *p > 0.0);
        g.pixel = Some((frame.format_label(), frame.bits, format));
        g.write_uniform();
        drop(g);
        *self.aspect.lock().unwrap_or_else(PoisonError::into_inner) = geometry::display_aspect(w, h, par, rotation);
        Ok(())
    }

    /// Rotação mudou sem frame novo (pause): reescreve o uniform e o aspecto do
    /// letterbox (o frame seguinte do play faria o mesmo no upload).
    pub fn set_rotation(&self, rotation: u8) {
        let rotation = rotation % 4;
        let aspect = {
            let mut g = self.lock();
            if g.bind_group.is_some() && g.rotation != rotation {
                g.rotation = rotation;
                g.write_uniform();
            }
            geometry::display_aspect(g.size.0, g.size.1, g.par, rotation)
        };
        *self.aspect.lock().unwrap_or_else(PoisonError::into_inner) = aspect;
    }

    /// HDR → SDR ativo no frame atual, para o overlay da tecla D.
    pub fn tone_map(&self) -> Option<ToneMapStatus> {
        let g = self.lock();
        let p = g.tone_params();
        p.mode.is_active().then_some(ToneMapStatus { mode: p.mode, operator: p.operator, peak_nits: p.peak_nits })
    }

    /// Caminho de amostra do último upload, para o overlay da tecla D
    /// (`10-bit 4:2:0 yuv -> r16unorm`, `8-bit 4:2:0 yuv -> r8unorm`, ou a queda
    /// para 8 bits num device sem `TEXTURE_FORMAT_16BIT_NORM`).
    pub fn pipeline_label(&self) -> Option<String> {
        let g = self.lock();
        g.pixel.as_ref().map(|(origin, bits, format)| {
            let target = if *format == wgpu::TextureFormat::R16Unorm { "r16unorm" } else { "r8unorm" };
            let note = if *bits > 8 && *format != wgpu::TextureFormat::R16Unorm { " (device sem 16-bit norm)" } else { "" };
            format!("{origin} yuv -> {target}{note}")
        })
    }

    /// Shape do egui que pinta o frame dentro de `rect` (letterbox no callback).
    pub fn shape(&self, rect: egui::Rect) -> egui::Shape {
        egui::Shape::Callback(eframe::egui_wgpu::Callback::new_paint_callback(
            rect,
            VideoCallback { gpu: self.inner.clone(), aspect: self.aspect.clone() },
        ))
    }
}

/// O paint callback em si (pintado dentro do `RenderPass` do egui).
struct VideoCallback {
    gpu: Arc<Mutex<Inner>>,
    aspect: Arc<Mutex<f32>>,
}

impl CallbackTrait for VideoCallback {
    fn paint(&self, info: eframe::egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, _res: &CallbackResources) {
        if info.screen_size_px[0] == 0 || info.screen_size_px[1] == 0 {
            return;
        }
        let g = self.gpu.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(bind_group) = g.bind_group.as_ref() else { return };
        let ppp = info.pixels_per_point.max(0.01);
        let viewport = Rect::new(info.viewport.min.x * ppp, info.viewport.min.y * ppp, info.viewport.width() * ppp, info.viewport.height() * ppp);
        let aspect = *self.aspect.lock().unwrap_or_else(PoisonError::into_inner);
        let inner = geometry::letterbox(viewport, aspect.max(0.01), 1.0);
        let target = [info.screen_size_px[0] as f32, info.screen_size_px[1] as f32];
        let vp = geometry::fit_viewport(inner, target[0], target[1]);
        pass.set_viewport(vp.x, vp.y, vp.w, vp.h, 0.0, 1.0);
        pass.set_pipeline(&g.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O WGSL precisa parsear e validar **na CPU**: um erro de shader só
    /// apareceria na criação do pipeline, no arranque do app (janela), nunca
    /// na UI de um teste comum.
    #[test]
    fn wgsl_do_video_valida() {
        let module = naga::front::wgsl::parse_str(include_str!("yuv.wgsl")).expect("WGSL do vídeo precisa parsear");
        let mut validator = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        validator.validate(&module).expect("WGSL do vídeo precisa validar");
    }

    /// Sem `TEXTURE_FORMAT_16BIT_NORM` a amostra de 16 bits desce pelo byte alto
    /// (little-endian): é exatamente o código de 8 bits que o pipeline antigo
    /// entregava. 10-bit branco limitado (940) vira ~235; preto (64) vira ~16.
    #[test]
    fn queda_para_8_bits_usa_o_byte_alto() {
        let sample = |v: u16| v.to_le_bytes();
        let mut plane = Vec::new();
        for v in [4112u16, 60395, 0, 65535] {
            plane.extend_from_slice(&sample(v));
        }
        let planes = Planes::U16([Arc::new(plane), Arc::new(Vec::new()), Arc::new(Vec::new())]);
        let out = downconvert(&planes);
        assert_eq!(out[0].as_slice(), [16u8, 235, 0, 255]);
    }
}
