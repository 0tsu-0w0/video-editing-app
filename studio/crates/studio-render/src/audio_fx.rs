//! 音声エフェクトの処理(イコライザ・コンプレッサー・ダッキング)。状態はトラックごとに持つ。

use studio_core::project::AudioEffect;

pub fn db_to_gain(db: f64) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        10f64.powf(db / 20.0) as f32
    }
}

pub fn gain_to_db(g: f32) -> f32 {
    if g <= 1e-6 {
        -120.0
    } else {
        20.0 * g.log10()
    }
}

/// 双二次フィルタ(RBJ の式)。
#[derive(Clone, Copy, Default)]
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    /// 左右それぞれの状態 (x1, x2, y1, y2)
    z: [[f32; 4]; 2],
}

enum Shape {
    LowShelf,
    Peak,
    HighShelf,
}

impl Biquad {
    fn set(&mut self, shape: Shape, freq: f32, db: f32, rate: f32) {
        let a = 10f32.powf(db / 40.0);
        let w = std::f32::consts::TAU * (freq / rate).min(0.49);
        let (sn, cs) = w.sin_cos();
        let (b, ac) = match shape {
            Shape::Peak => {
                let alpha = sn / (2.0 * 0.7);
                ([1.0 + alpha * a, -2.0 * cs, 1.0 - alpha * a], [1.0 + alpha / a, -2.0 * cs, 1.0 - alpha / a])
            }
            Shape::LowShelf | Shape::HighShelf => {
                let alpha = sn / 2.0 * 2f32.sqrt(); // S = 1
                let sq = 2.0 * a.sqrt() * alpha;
                if matches!(shape, Shape::LowShelf) {
                    (
                        [a * ((a + 1.0) - (a - 1.0) * cs + sq), 2.0 * a * ((a - 1.0) - (a + 1.0) * cs), a * ((a + 1.0) - (a - 1.0) * cs - sq)],
                        [(a + 1.0) + (a - 1.0) * cs + sq, -2.0 * ((a - 1.0) + (a + 1.0) * cs), (a + 1.0) + (a - 1.0) * cs - sq],
                    )
                } else {
                    (
                        [a * ((a + 1.0) + (a - 1.0) * cs + sq), -2.0 * a * ((a - 1.0) + (a + 1.0) * cs), a * ((a + 1.0) + (a - 1.0) * cs - sq)],
                        [(a + 1.0) - (a - 1.0) * cs + sq, 2.0 * ((a - 1.0) - (a + 1.0) * cs), (a + 1.0) - (a - 1.0) * cs - sq],
                    )
                }
            }
        };
        self.b = [b[0] / ac[0], b[1] / ac[0], b[2] / ac[0]];
        self.a = [ac[1] / ac[0], ac[2] / ac[0]];
    }

    fn run(&mut self, buf: &mut [f32]) {
        for f in buf.chunks_exact_mut(2) {
            for (c, x) in f.iter_mut().enumerate() {
                let z = &mut self.z[c];
                let y = self.b[0] * *x + self.b[1] * z[0] + self.b[2] * z[1] - self.a[0] * z[2] - self.a[1] * z[3];
                *z = [*x, z[0], y, z[2]];
                *x = y;
            }
        }
    }
}

/// 1 つのエフェクトの状態。
pub enum FxState {
    Eq([Biquad; 3]),
    /// 検出した音量(直線)
    Comp(f32),
    /// (相手の音量の包絡, 今の下げ幅 dB)
    Duck(f32, f32),
}

impl FxState {
    pub fn new(e: &AudioEffect) -> FxState {
        match e {
            AudioEffect::Eq { .. } => FxState::Eq([Biquad::default(); 3]),
            AudioEffect::Compressor { .. } => FxState::Comp(0.0),
            AudioEffect::Duck { .. } => FxState::Duck(0.0, 0.0),
        }
    }

    pub fn matches(&self, e: &AudioEffect) -> bool {
        matches!((self, e), (FxState::Eq(_), AudioEffect::Eq { .. }) | (FxState::Comp(_), AudioEffect::Compressor { .. }) | (FxState::Duck(..), AudioEffect::Duck { .. }))
    }

    /// buf(ステレオ交互)にエフェクトをかける。side はダッキングの相手の音(無ければ None)。
    pub fn process(&mut self, e: &AudioEffect, buf: &mut [f32], side: Option<&[f32]>, rate: f32) {
        let coef = |ms: f32| (-1.0 / (ms * 0.001 * rate)).exp();
        match (self, e) {
            (FxState::Eq(f), &AudioEffect::Eq { low_db, mid_db, high_db }) => {
                for (bq, (shape, freq, db)) in f.iter_mut().zip([(Shape::LowShelf, 200.0, low_db), (Shape::Peak, 1000.0, mid_db), (Shape::HighShelf, 5000.0, high_db)]) {
                    if db != 0.0 {
                        bq.set(shape, freq, db as f32, rate);
                        bq.run(buf);
                    }
                }
            }
            (FxState::Comp(env), &AudioEffect::Compressor { threshold_db, ratio, makeup_db }) => {
                let (att, rel) = (coef(10.0), coef(150.0));
                let makeup = db_to_gain(makeup_db);
                for f in buf.chunks_exact_mut(2) {
                    let x = f[0].abs().max(f[1].abs());
                    let c = if x > *env { att } else { rel };
                    *env = c * *env + (1.0 - c) * x;
                    let over = gain_to_db(*env) - threshold_db as f32;
                    let g = if over > 0.0 { db_to_gain((-over * (1.0 - 1.0 / ratio as f32)) as f64) } else { 1.0 } * makeup;
                    f[0] *= g;
                    f[1] *= g;
                }
            }
            (FxState::Duck(env, cut), &AudioEffect::Duck { amount_db, .. }) => {
                let Some(side) = side else { return };
                // 相手の音量の検出は速く、下げ幅の戻りはゆっくり(言葉の切れ目で BGM が揺れないように)
                let (att, rel) = (coef(5.0), coef(40.0));
                let (down, up) = (coef(60.0), coef(300.0));
                for (f, s) in buf.chunks_exact_mut(2).zip(side.chunks_exact(2)) {
                    let x = s[0].abs().max(s[1].abs());
                    let c = if x > *env { att } else { rel };
                    *env = c * *env + (1.0 - c) * x;
                    // 相手が -40dB より大きければ下げる
                    let target = if gain_to_db(*env) > -40.0 { amount_db as f32 } else { 0.0 };
                    let c = if target < *cut { down } else { up };
                    *cut = c * *cut + (1.0 - c) * target;
                    let g = 10f32.powf(*cut / 20.0);
                    f[0] *= g;
                    f[1] *= g;
                }
            }
            _ => {}
        }
    }
}
