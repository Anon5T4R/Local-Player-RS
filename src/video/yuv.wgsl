// YUV planar → R'G'B' com a matriz do container e rotação em quartos.
//
// As amostras chegam normalizadas (0..1) na convenção de código do shader,
// venham de textura R8Unorm (8 bits) ou R16Unorm (10/12 bits convertidos na
// thread de decode para o código de 8 bits equivalente) — a matriz já dobra a
// faixa (limitada 16–235 / completa 0–255) nos coeficientes, então nada aqui
// muda entre 8 e 10 bits.
//
// Em faixas PQ/HLG faz HDR → SDR: EOTF, tonemap (BT.2390 default ou Hable) e
// encode para o alvo.
//
// Layout do uniform (96 bytes, 6 × vec4<f32>):
//   row_r/g/b = (kY, kCb, kCr, offset) por canal, já com a normalização da faixa
//               (16–235 limitada / 0–255 completa) dobrada nos coeficientes
//   uv        = (a, b, c, d) da transformação UV: src = (a·u + c·v + ox, b·u + d·v + oy)
//   uv_off    = (ox, oy, linearize, _)
//   tone      = (mode, peak_nits, tone_op, _) — mode: 0 = SDR, 1 = PQ, 2 = HLG
//                                              tone_op: 0 = BT.2390, 1 = Hable
//                                              ("operator" é palavra reservada no WGSL)
//
// linearize = 1 quando o alvo é *_Srgb: o shader entrega luz linear e o hardware
// codifica para sRGB na escrita. Com alvo Unorm os valores gamma-encoded saem
// direto (aí o encode sRGB é feito aqui, depois do tonemap).
//
// A matemática de HDR (EOTF PQ/HLG, BT.2390 EETF, Hable, pico) tem espelho CPU
// testável em `tone.rs`; este shader é validado pelo teste `naga` do `gpu.rs`.

struct Uniforms {
    row_r: vec4<f32>,
    row_g: vec4<f32>,
    row_b: vec4<f32>,
    uv: vec4<f32>,
    uv_off: vec4<f32>,
    tone: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex_y: texture_2d<f32>;
@group(0) @binding(2) var tex_u: texture_2d<f32>;
@group(0) @binding(3) var tex_v: texture_2d<f32>;
@group(0) @binding(4) var samp: sampler;

// Branco de referência SDR (BT.2408) e pico absoluto do PQ, em nits.
const REFERENCE_WHITE_NITS: f32 = 203.0;
const PQ_PEAK_NITS: f32 = 10000.0;
// HLG é relativo: pico fixo de 1000 nits (independe do env do PQ).
const HLG_PEAK_NITS: f32 = 1000.0;
// SMPTE ST 2084.
const PQ_M1: f32 = 0.15930176;
const PQ_M2: f32 = 78.84375;
const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.851563;
const PQ_C3: f32 = 18.6875;
// ARIB STD-B67.
const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 0.28466892;
const HLG_C: f32 = 0.5599107;

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

// Encode sRGB: luz linear → codificado (só para alvo Unorm).
fn from_linear(v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0);
    return select(1.055 * pow(x, 1.0 / 2.4) - 0.055, x * 12.92, x <= 0.0031308);
}

// SMPTE ST 2084: código 0–1 → nits absolutos (0–10000).
fn pq_eotf_nits(e: f32) -> f32 {
    let p = pow(max(e, 0.0), 1.0 / PQ_M2);
    return pow(max(p - PQ_C1, 0.0) / (PQ_C2 - PQ_C3 * p), 1.0 / PQ_M1) * PQ_PEAK_NITS;
}

// SMPTE ST 2084 inversa: nits absolutos → código 0–1 (o domínio do EETF).
fn pq_inverse_eotf(nits: f32) -> f32 {
    let y = pow(max(nits, 0.0) / PQ_PEAK_NITS, PQ_M1);
    return pow((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y), PQ_M2);
}

// ARIB STD-B67 inverse OETF: código 0–1 → luz de cena (OOTF à parte).
fn hlg_inverse_oetf(e: f32) -> f32 {
    let x = max(e, 0.0);
    if (x <= 0.5) {
        return x * x / 3.0;
    }
    return (exp((x - HLG_C) / HLG_A) + HLG_B) / 12.0;
}

// Curva filmic de Hable (Uncharted 2), sem normalização do white point.
fn hable(x: f32) -> f32 {
    let a = 0.15;
    let b = 0.50;
    let c = 0.10;
    let d = 0.20;
    let e = 0.02;
    let f = 0.30;
    return (x * (a * x + c * b) + d * e) / (x * (a * x + b) + d * f) - e / f;
}

// Sinal R'G'B' da faixa → nits absolutos de display.
fn display_nits(c: vec3<f32>, mode: u32) -> vec3<f32> {
    if (mode == 1u) {
        return vec3<f32>(pq_eotf_nits(c.r), pq_eotf_nits(c.g), pq_eotf_nits(c.b));
    }
    // HLG: inverse OETF + OOTF (gamma de sistema 1.2) com pico relativo de 1000 nits.
    let scene = vec3<f32>(hlg_inverse_oetf(c.r), hlg_inverse_oetf(c.g), hlg_inverse_oetf(c.b));
    return pow(max(scene, vec3<f32>(0.0)), vec3<f32>(1.2)) * HLG_PEAK_NITS;
}

// ITU-R BT.2390 EETF: nits (pico de origem `src_peak`) → pico de destino
// `dst_peak`, no domínio PQ e com joelho. Mesma conta do `tone::bt2390_eetf`.
fn bt2390_eetf(nits: f32, src_peak: f32, dst_peak: f32) -> f32 {
    if (dst_peak >= src_peak || nits <= 0.0) {
        return min(max(nits, 0.0), max(src_peak, dst_peak));
    }
    let smax = pq_inverse_eotf(src_peak);
    let e1 = min(pq_inverse_eotf(nits) / smax, 1.0);
    let max_lum = pq_inverse_eotf(dst_peak) / smax;
    let ks = 1.5 * max_lum - 0.5;
    if (e1 < ks) {
        return pq_eotf_nits(e1 * smax);
    }
    let t = (e1 - ks) / (1.0 - ks);
    let t2 = t * t;
    let t3 = t2 * t;
    let e2 = (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks) + (-2.0 * t3 + 3.0 * t2) * max_lum;
    return pq_eotf_nits(e2 * smax);
}

// HDR → SDR com Hable: canal a canal, normalizado pelo pico do conteúdo.
fn tonemap_hable(c: vec3<f32>, mode: u32, peak_nits: f32) -> vec3<f32> {
    let lin = display_nits(c, mode) / REFERENCE_WHITE_NITS;
    let white = max(peak_nits, 1.0) / REFERENCE_WHITE_NITS;
    let scale = 1.0 / max(hable(white), 1e-6);
    return clamp(vec3<f32>(hable(lin.r), hable(lin.g), hable(lin.b)) * scale, vec3<f32>(0.0), vec3<f32>(1.0));
}

// HDR → SDR com BT.2390: EETF sobre max(R,G,B) e um ganho único nos três
// canais (matiz preservado). Devolve luz de display linear 0–1.
fn tonemap_bt2390(c: vec3<f32>, mode: u32, peak_nits: f32) -> vec3<f32> {
    let nits = display_nits(c, mode);
    let m = max(nits.r, max(nits.g, nits.b));
    let out = bt2390_eetf(m, max(peak_nits, 1.0), REFERENCE_WHITE_NITS);
    let k = select(0.0, out / m, m > 1e-6);
    return clamp(nits * k / REFERENCE_WHITE_NITS, vec3<f32>(0.0), vec3<f32>(1.0));
}

// HDR → SDR: escolhe o operador de `u.tone.z` (0 = BT.2390, 1 = Hable).
fn tonemap(c: vec3<f32>, mode: u32, peak_nits: f32, tone_op: u32) -> vec3<f32> {
    if (tone_op == 1u) {
        return tonemap_hable(c, mode, peak_nits);
    }
    return tonemap_bt2390(c, mode, peak_nits);
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
    let linear_target = u.uv_off.z > 0.5;
    let mode = u32(max(u.tone.x, 0.0) + 0.5);
    let tone_op = u32(max(u.tone.z, 0.0) + 0.5);
    if (mode == 1u || mode == 2u) {
        let lin = tonemap(rgb, mode, u.tone.y, tone_op);
        if (linear_target) {
            rgb = lin;
        } else {
            rgb = vec3<f32>(from_linear(lin.r), from_linear(lin.g), from_linear(lin.b));
        }
    } else if (linear_target) {
        rgb = vec3<f32>(to_linear(rgb.r), to_linear(rgb.g), to_linear(rgb.b));
    }
    return vec4<f32>(rgb, 1.0);
}
