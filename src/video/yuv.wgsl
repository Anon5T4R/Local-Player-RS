// YUV planar (8 bits) → R'G'B' com a matriz do container e rotação em quartos.
//
// Layout do uniform (80 bytes, 5 × vec4<f32>):
//   row_r/g/b = (kY, kCb, kCr, offset) por canal, já com a normalização da faixa
//               (16–235 limitada / 0–255 completa) dobrada nos coeficientes
//   uv        = (a, b, c, d) da transformação UV: src = (a·u + c·v + ox, b·u + d·v + oy)
//   uv_off    = (ox, oy, linearize, _)
//
// linearize = 1 quando o alvo é *_Srgb: o shader entrega luz linear e o hardware
// codifica para sRGB na escrita; com alvo Unorm (o caso do egui) os valores
// gamma-encoded saem direto.

struct Uniforms {
    row_r: vec4<f32>,
    row_g: vec4<f32>,
    row_b: vec4<f32>,
    uv: vec4<f32>,
    uv_off: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex_y: texture_2d<f32>;
@group(0) @binding(2) var tex_u: texture_2d<f32>;
@group(0) @binding(3) var tex_v: texture_2d<f32>;
@group(0) @binding(4) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Triângulo que cobre a tela toda (sem vertex buffer).
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let xy = corners[index];
    var out: VsOut;
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    // UV com origem no canto superior esquerdo.
    out.uv = vec2<f32>((xy.x + 1.0) * 0.5, (1.0 - xy.y) * 0.5);
    return out;
}

fn channel(row: vec4<f32>, y: f32, cb: f32, cr: f32) -> f32 {
    return row.x * y + row.y * cb + row.z * cr + row.w;
}

// EOTF do sRGB: valor codificado → luz linear.
fn to_linear(v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0);
    return select(pow((x + 0.055) / 1.055, 2.4), x / 12.92, x <= 0.04045);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = vec2<f32>(
        u.uv.x * in.uv.x + u.uv.z * in.uv.y + u.uv_off.x,
        u.uv.y * in.uv.x + u.uv.w * in.uv.y + u.uv_off.y,
    );
    let y = textureSample(tex_y, samp, uv).r;
    let cb = textureSample(tex_u, samp, uv).r;
    let cr = textureSample(tex_v, samp, uv).r;
    var rgb = vec3<f32>(
        channel(u.row_r, y, cb, cr),
        channel(u.row_g, y, cb, cr),
        channel(u.row_b, y, cb, cr),
    );
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if (u.uv_off.z > 0.5) {
        rgb = vec3<f32>(to_linear(rgb.r), to_linear(rgb.g), to_linear(rgb.b));
    }
    return vec4<f32>(rgb, 1.0);
}
