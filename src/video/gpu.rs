//! Rasterização do frame de vídeo na GPU do eframe/wgpu.
//!
//! Um shader WGSL faz YUV → R'G'B' (a matriz vem do container, ver
//! [`crate::video::color`]) e a rotação em quartos vem na transformação de UV.
//! O frame é pintado por um **paint callback** do egui (sem roundtrip por CPU
//! dos pixels convertidos) num retângulo letterboxed.
//!
//! Um único [`VideoGpu`] é compartilhado entre a UI (upload) e o callback
//! (paint) por `Arc<Mutex<…>>`; as texturas só são recriadas quando o tamanho
//! ou o formato de croma muda.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use eframe::egui_wgpu::wgpu;
use eframe::egui_wgpu::{CallbackResources, CallbackTrait};

use super::color::yuv_to_rgb;
use super::geometry::{self, Rect};
use super::{chroma_dims, Frame};

/// Texturas planas do frame atual (Y, U, V).
struct Textures {
    y: wgpu::Texture,
    u: wgpu::Texture,
    v: wgpu::Texture,
    size: (u32, u32),
    chroma: filmcraft_frame::Chroma,
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
    /// Maior lado que o device aceita (≤ 8K).
    max_side: u32,
}

impl Inner {
    fn write_uniform(&self) {
        let (uv, offset) = geometry::uv_transform(self.rotation);
        let matrix = yuv_to_rgb(&self.color);
        self.queue.write_buffer(&self.uniform, 0, &matrix.uniform_bytes(uv, offset, self.linearize));
    }

    /// (Re)cria texturas, views e bind group quando o frame muda de tamanho/croma.
    fn ensure_textures(&mut self, width: u32, height: u32, chroma: filmcraft_frame::Chroma) {
        if let Some(t) = &self.textures {
            if t.size == (width, height) && t.chroma == chroma {
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
                format: wgpu::TextureFormat::R8Unorm,
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
        self.textures = Some(Textures { y, u, v, size: (width, height), chroma });
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
            size: 5 * 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let max_side = device.limits().max_texture_dimension_2d.min(super::MAX_DIMENSION);
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
                max_side,
            })),
            aspect: Arc::new(Mutex::new(1.0)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sobe o frame para as texturas e atualiza a matriz de cor/rotação.
    pub fn upload(&self, frame: &Frame, par: (u32, u32), rotation: u8) -> Result<(), UploadError> {
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
        if frame.planes[0].len() < need_y || frame.planes[1].len() < need_c || frame.planes[2].len() < need_c {
            return Err(UploadError::Invalid("decoded frame planes are truncated".into()));
        }
        g.ensure_textures(w, h, frame.chroma);
        let (y, u, v) = match &g.textures {
            Some(t) => (t.y.clone(), t.u.clone(), t.v.clone()),
            None => return Err(UploadError::Invalid("video textures missing".into())),
        };
        let write = |texture: &wgpu::Texture, data: &[u8], tw: u32, th: u32| {
            g.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(tw), rows_per_image: Some(th) },
                wgpu::Extent3d { width: tw, height: th, depth_or_array_layers: 1 },
            );
        };
        write(&y, &frame.planes[0], w, h);
        write(&u, &frame.planes[1], cw, ch);
        write(&v, &frame.planes[2], cw, ch);
        g.color = frame.color;
        g.rotation = rotation % 4;
        g.size = (w, h);
        g.par = par;
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
    /// O WGSL precisa parsear e validar **na CPU**: um erro de shader só
    /// apareceria na criação do pipeline, no arranque do app (janela), nunca
    /// na UI de um teste comum.
    #[test]
    fn wgsl_do_video_valida() {
        let module = naga::front::wgsl::parse_str(include_str!("yuv.wgsl")).expect("WGSL do vídeo precisa parsear");
        let mut validator = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        validator.validate(&module).expect("WGSL do vídeo precisa validar");
    }
}
