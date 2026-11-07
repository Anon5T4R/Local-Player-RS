//! YUV → R'G'B' em coeficientes prontos para a GPU.
//!
//! O container/codec informa a matriz (BT.601/BT.709/BT.2020) e a faixa
//! (limitada 16–235 ou completa 0–255); a conversão é a definida em ITU-R
//! BT.601/709: com `Kr` e `Kb` da matriz,
//!
//! ```text
//! R = Y' + 2(1-Kr)·Cr'        G = Y' - 2Kb(1-Kb)/Kg·Cb' - 2Kr(1-Kr)/Kg·Cr'
//! B = Y' + 2(1-Kb)·Cb'        (Kg = 1 - Kr - Kb)
//! ```
//!
//! A normalização da faixa é dobrada nos coeficientes: cada linha da matriz já
//! multiplica a amostra crua da textura (0..1 sobre o código de 8 bits) e soma
//! o deslocamento do neutro, então o shader só faz `dot` + soma.

use filmcraft_color::{ColorInfo, Range};

/// Três linhas `(kY, kCb, kCr, offset)` — R, G e B.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YuvMatrix {
    pub rows: [[f32; 4]; 3],
}

impl YuvMatrix {
    /// Aplica a matriz a códigos de 8 bits (mesma conta do shader; usada nos testes).
    #[cfg(test)]
    pub fn apply(&self, y: f32, cb: f32, cr: f32) -> [f32; 3] {
        let (y, cb, cr) = (y / 255.0, cb / 255.0, cr / 255.0);
        self.rows.map(|r| (r[0] * y + r[1] * cb + r[2] * cr + r[3]).clamp(0.0, 1.0))
    }

    /// Bytes do uniform (20 floats, layout `vec4 × 5` do WGSL: 3 linhas + uv + uv_off).
    pub fn uniform_bytes(&self, uv: [f32; 4], uv_offset: [f32; 2], linearize: bool) -> Vec<u8> {
        let mut values = Vec::with_capacity(20);
        for row in self.rows {
            values.extend_from_slice(&row);
        }
        values.extend_from_slice(&uv);
        values.push(uv_offset[0]);
        values.push(uv_offset[1]);
        values.push(if linearize { 1.0 } else { 0.0 });
        values.push(0.0);
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
}

/// Matriz YUV→RGB do stream. Sem cor declarada, [`ColorInfo::REC709`] (BT.709
/// limitado) já é o default do próprio `ColorInfo`.
pub fn yuv_to_rgb(color: &ColorInfo) -> YuvMatrix {
    let (kr, kb) = color.matrix.kr_kb();
    let kg = 1.0 - kr - kb;
    // Códigos de 8 bits → normalizado ([0,1] luma; croma com neutro em 128).
    let (y_off, y_range, c_range) = match color.range {
        Range::Limited => (16.0f32, 219.0f32, 224.0f32),
        Range::Full => (0.0, 255.0, 255.0),
    };
    let (sy, cy) = (255.0 / y_range, -y_off / y_range);
    let (sc, cc) = (255.0 / c_range, -128.0 / c_range);
    let r_cr = 2.0 * (1.0 - kr);
    let b_cb = 2.0 * (1.0 - kb);
    // G = Y' - (2Kb(1-Kb)/Kg)·Cb' - (2Kr(1-Kr)/Kg)·Cr'
    let (g_cb, g_cr) = (2.0 * kb * (1.0 - kb) / kg, 2.0 * kr * (1.0 - kr) / kg);
    YuvMatrix {
        rows: [[sy, 0.0, r_cr * sc, cy + r_cr * cc], [sy, -g_cb * sc, -g_cr * sc, cy - (g_cb + g_cr) * cc], [sy, b_cb * sc, 0.0, cy + b_cb * cc]],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_color::{ColorInfo, Matrix};

    fn limited(matrix: Matrix) -> ColorInfo {
        ColorInfo { matrix, range: Range::Limited, ..ColorInfo::REC709 }
    }

    fn assert_rgb(got: [f32; 3], want: [f32; 3], tol: f32) {
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() <= tol, "got {got:?}, want {want:?}");
        }
    }

    /// Branco limitado: Y=235, Cb=Cr=128 → 1,1,1 (nos três padrões de matriz).
    #[test]
    fn branco_e_preto_na_faixa_limitada() {
        for m in [Matrix::Bt601, Matrix::Bt709, Matrix::Bt2020Ncl] {
            let yuv = yuv_to_rgb(&limited(m));
            assert_rgb(yuv.apply(235.0, 128.0, 128.0), [1.0, 1.0, 1.0], 1e-4);
            assert_rgb(yuv.apply(16.0, 128.0, 128.0), [0.0, 0.0, 0.0], 1e-4);
        }
    }

    /// Vermelho BT.601 (Y=81, Cb=90, Cr=240) → R≈1, G≈0, B≈0.
    #[test]
    fn vermelho_bt601() {
        let yuv = yuv_to_rgb(&limited(Matrix::Bt601));
        // Os códigos de vermelho "red" do BT.601 são arredondados (81/90/240):
        // 5e-3 cobre o arredondamento sem afrouxar o teste.
        assert_rgb(yuv.apply(81.0, 90.0, 240.0), [1.0, 0.0, 0.0], 5e-3);
    }

    /// O mesmo código em BT.709 dá outro verde (matrizes diferentes de verdade).
    #[test]
    fn matrizes_diferem() {
        let a = yuv_to_rgb(&limited(Matrix::Bt601));
        let b = yuv_to_rgb(&limited(Matrix::Bt709));
        assert!((a.rows[1][2] - b.rows[1][2]).abs() > 0.05, "601 {a:?} vs 709 {b:?}");
    }

    /// Faixa completa: 0/255 são preto/branco sem deslocamento.
    #[test]
    fn faixa_completa() {
        let yuv = yuv_to_rgb(&ColorInfo { range: Range::Full, ..ColorInfo::REC709 });
        assert_rgb(yuv.apply(255.0, 128.0, 128.0), [1.0, 1.0, 1.0], 1e-4);
        assert_rgb(yuv.apply(0.0, 128.0, 128.0), [0.0, 0.0, 0.0], 1e-4);
    }

    /// O uniform tem exatamente os 20 floats do layout WGSL.
    #[test]
    fn uniform_tem_20_floats() {
        let bytes = yuv_to_rgb(&ColorInfo::REC709).uniform_bytes([1.0, 0.0, 0.0, 1.0], [0.0, 0.0], true);
        assert_eq!(bytes.len(), 20 * 4);
        let flag = f32::from_le_bytes([bytes[72], bytes[73], bytes[74], bytes[75]]);
        assert_eq!(flag, 1.0, "flag de linearização no float 18");
    }
}
