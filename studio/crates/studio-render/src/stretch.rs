//! 音の高さを保ったまま速さを変える(WSOLA に近い重ね合わせ)。
//!
//! 出力を H 標本ごとの区切り(粒)に分け、粒 j は「クリップ内の位置 j·H に対応する素材の位置」から
//! 2H 標本を元の速さで読み、ハン窓をかけて半分ずつ重ねて足す。粒の読み始めは ±TOL の範囲で、
//! 1 つ前の粒の続きと波形が最もよく合う所にずらす(つなぎ目のうなりを減らす)。
//! 各粒の実際の読み始めは `starts` に覚えておき、次の粒を合わせるのに使う(続けて呼べば、何標本ずつ呼んでも同じ結果)。
//! 覚えがない所(シークの直後)は、1 つ前の粒をずらさずに読んだものとして始める。

/// 粒の間隔(標本)。48kHz で約 21ms
pub const HOP: i64 = 1024;
/// 読み始めをずらす範囲(標本)
const TOL: i64 = 480;
/// ずらし量を探す刻みと、波形を比べる刻み
const SEARCH_STEP: i64 = 8;
const CMP_STEP: i64 = 4;

/// クリップ内の位置 local0 から n 標本分(ステレオ)を作る。
/// `pos(l)` はクリップ内の位置 l に対応する素材の位置(標本)、`dir` は素材を読む向き(1 か -1、逆再生は -1)。
/// `read(start, frames)` は素材の start から frames 標本(ステレオ)を返す。
pub fn stretch(
    local0: i64,
    n: usize,
    pos: impl Fn(i64) -> f64,
    dir: i64,
    starts: &mut std::collections::BTreeMap<i64, i64>,
    mut read: impl FnMut(i64, usize) -> Vec<f32>,
) -> Vec<f32> {
    let first = local0.div_euclid(HOP) - 1;
    let last = (local0 + n as i64 - 1).div_euclid(HOP);
    let base = |j: i64| pos(j * HOP).round() as i64;
    // 必要な素材の範囲をまとめて読む
    let (mut lo, mut hi) = (i64::MAX, i64::MIN);
    for j in (first - 1)..=last {
        let b = base(j);
        for e in [b - TOL, b + dir * 2 * HOP - TOL, b + TOL, b + dir * 2 * HOP + TOL] {
            lo = lo.min(e);
            hi = hi.max(e);
        }
    }
    let lo = lo.max(0);
    if hi < lo {
        return vec![0.0; n * 2];
    }
    let buf = read(lo, (hi - lo + 2) as usize);
    let at = |i: i64, ch: usize| -> f32 {
        if i < lo {
            return 0.0;
        }
        buf.get(((i - lo) as usize) * 2 + ch).copied().unwrap_or(0.0)
    };
    let mono = |i: i64| at(i, 0) + at(i, 1);
    // 古い覚えは捨てる
    while starts.first_key_value().is_some_and(|(k, _)| *k < first - 2) {
        starts.pop_first();
    }
    // 粒 j の読み始め(1 つ前の粒の実際の続きに最もよく合う所)
    let mut start_of = |j: i64| -> i64 {
        if let Some(s) = starts.get(&j) {
            return *s;
        }
        let b = base(j);
        let prev = starts.get(&(j - 1)).copied().unwrap_or_else(|| base(j - 1)) + dir * HOP;
        let mut best = (f32::MIN, 0);
        let mut d = -TOL;
        while d <= TOL {
            let mut s = 0.0f32;
            let mut x = 0;
            while x < HOP {
                s += mono(b + d + dir * x) * mono(prev + dir * x);
                x += CMP_STEP;
            }
            if s > best.0 {
                best = (s, d);
            }
            d += SEARCH_STEP;
        }
        starts.insert(j, b + best.1);
        b + best.1
    };
    let mut out = vec![0.0f32; n * 2];
    for j in first..=last {
        let s = start_of(j);
        let g0 = j * HOP;
        for x in 0..2 * HOP {
            let l = g0 + x;
            if l < local0 || l >= local0 + n as i64 {
                continue;
            }
            let w = 0.5 - 0.5 * (std::f32::consts::PI * x as f32 / HOP as f32).cos();
            let k = (l - local0) as usize;
            let src = s + dir * x;
            out[k * 2] += at(src, 0) * w;
            out[k * 2 + 1] += at(src, 1) * w;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, secs: f64) -> Vec<f32> {
        let n = (secs * 48_000.0) as usize;
        (0..n).flat_map(|i| {
            let v = ((i as f64 / 48_000.0) * f * std::f64::consts::TAU).sin() as f32 * 0.5;
            [v, v]
        }).collect()
    }

    fn reader(src: &[f32]) -> impl FnMut(i64, usize) -> Vec<f32> + '_ {
        move |start, frames| (0..frames).flat_map(|k| {
            let i = (start as usize + k) * 2;
            [src.get(i).copied().unwrap_or(0.0), src.get(i + 1).copied().unwrap_or(0.0)]
        }).collect()
    }

    fn ups(buf: &[f32]) -> usize {
        let l: Vec<f32> = buf.chunks(2).map(|c| c[0]).collect();
        l.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    /// 1024 標本ずつに分けて呼んでも、まとめて呼んでも同じ
    fn run(src: &[f32], speed: f64, dir: i64, start: f64, total: usize, chunk: usize) -> Vec<f32> {
        let mut out = vec![];
        let mut r = reader(src);
        let mut starts = Default::default();
        let mut l = 0;
        while l < total {
            let n = chunk.min(total - l);
            out.extend(stretch(l as i64, n, |x| start + x as f64 * speed * dir as f64, dir, &mut starts, &mut r));
            l += n;
        }
        out
    }

    #[test]
    fn keeps_pitch_at_double_and_half_speed() {
        let src = sine(440.0, 6.0);
        for speed in [2.0, 0.5, 1.5] {
            let out = run(&src, speed, 1, 0.0, 48_000, 1024);
            let u = ups(&out[4_800 * 2..43_200 * 2]); // 0.1〜0.9 秒
            assert!((348..=356).contains(&u), "速度 {speed}: 0.8 秒で上向きのゼロ交差 {u}(440Hz なら 352)");
            // 音量がほぼ一定(粒の重ね合わせで大きく欠けない)
            let peak = |a: usize| out[a * 2..(a + 2400) * 2].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            for a in [6_000, 18_000, 30_000, 40_000] {
                assert!(peak(a) > 0.4, "速度 {speed} の {a} 付近のピーク {}", peak(a));
            }
        }
    }

    #[test]
    fn same_result_regardless_of_chunking_and_reverse_works() {
        let src = sine(300.0, 4.0);
        let a = run(&src, 2.0, 1, 1000.0, 20_000, 20_000);
        let b = run(&src, 2.0, 1, 1000.0, 20_000, 777);
        assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5));
        // 逆向き(4 秒目から戻る)でも高さは同じ
        let r = run(&src, 2.0, -1, 4.0 * 48_000.0 - 1.0, 48_000, 1024);
        let u = ups(&r[4_800 * 2..43_200 * 2]);
        assert!((236..=244).contains(&u), "逆向き {u}(300Hz なら 240)");
    }
}
