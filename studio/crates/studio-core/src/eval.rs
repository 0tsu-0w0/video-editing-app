//! ある時刻に何をどう描くか・鳴らすかを決める(描画・音声エンジンへの指示)。

use crate::project::{eval_pin, Clip, Corners, Id, MaskShape, Project, TrackKind, TransitionKind};
use crate::time::Time;

/// 1 枚の映像レイヤー(奥から手前の順に並ぶ)。
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub clip: Id,
    pub asset: Id,
    /// 素材のどの時刻を表示するか
    pub source_time: Time,
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub rotation: f64,
    pub opacity: f64,
    /// 3D モデルの回転(度)
    pub rot_x: f64,
    pub rot_y: f64,
    /// クリップの開始からの時刻(3D モデルのアニメーション用)
    pub local: Time,
    /// 四隅の位置(あれば x / y / 拡大 / 回転の代わりに使う)
    pub corners: Option<Corners>,
    /// マスク
    pub mask: Option<LayerMask>,
    /// ワイプ・円形のトランジション(種類と、見えている割合 0〜1)
    pub wipe: Option<(TransitionKind, f64)>,
    /// 同じ素材を同時に 2 か所で表示するときの区別(トランジション中の前のクリップは 1)
    pub slot: u8,
    /// 色の調整とぼかし
    pub fx: LayerFx,
    /// クロマキー(色 0〜1、範囲、なめらかさ、色かぶりの抑え)
    pub key: Option<([f64; 3], f64, f64, f64)>,
}

/// 時刻を評価した色の調整とぼかし(すべて 0 で元のまま)。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayerFx {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
    /// 度
    pub hue: f64,
    /// ぼかしの半径(素材のピクセル)
    pub blur: f64,
}

/// 時刻を評価したマスク(素材のピクセル、中心は素材の中央から)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerMask {
    pub shape: MaskShape,
    pub invert: bool,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub feather: f64,
}

fn ease(p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

/// クリップ c の、時刻 t のレイヤー。`held` は、終わった後もトランジションの下で表示し続ける前のクリップ。
fn clip_layer(p: &Project, c: &Clip, t: Time, held: bool) -> Option<Layer> {
    let raw = t - c.start;
    let local = if held { raw.min(c.duration - Time(1)) } else { raw };
    let pr = &c.params;
    let mut opacity = pr.opacity.eval(local).clamp(0.0, 1.0) * if held { 1.0 } else { c.fade_gain(local) };
    let mut source_time = c.source_at(raw);
    if held {
        if let Some(d) = p.assets.get(&c.asset).and_then(|a| a.kind.duration()) {
            // 素材の終わりを越えたら、最後のフレームで止める
            source_time = source_time.min(d - p.settings.frame_rate.frame_duration()).max(Time::ZERO);
        }
    }
    let (w, h) = (p.settings.width as f64, p.settings.height as f64);
    let (mut dx, mut dy, mut zoom) = (0.0, 0.0, 1.0);
    let mut wipe = None;
    let mut apply = |kind: TransitionKind, q: f64, appearing: bool| {
        let s = if appearing { 1.0 } else { -1.0 };
        match kind {
            TransitionKind::Fade => opacity *= q,
            TransitionKind::Zoom => {
                zoom *= 0.3 + 0.7 * q;
                opacity *= q;
            }
            TransitionKind::SlideLeft => dx += s * (1.0 - q) * w,
            TransitionKind::SlideRight => dx -= s * (1.0 - q) * w,
            TransitionKind::SlideUp => dy += s * (1.0 - q) * h,
            TransitionKind::SlideDown => dy -= s * (1.0 - q) * h,
            k => wipe = Some((k, q)),
        }
    };
    if !held {
        if let Some(tr) = c.transition_in.filter(|tr| local < tr.duration) {
            apply(tr.kind, ease(local.0 as f64 / tr.duration.0 as f64), true);
        }
        if let Some(tr) = c.transition_out.filter(|tr| c.duration - local < tr.duration) {
            apply(tr.kind, ease((c.duration - local).0 as f64 / tr.duration.0 as f64), false);
        }
    }
    if opacity <= 0.0 {
        return None;
    }
    let corners = eval_pin(&c.pin, local).map(|mut k| {
        let (cx, cy) = (k.iter().map(|p| p[0]).sum::<f64>() / 4.0, k.iter().map(|p| p[1]).sum::<f64>() / 4.0);
        for q in &mut k {
            q[0] = cx + (q[0] - cx) * zoom + dx;
            q[1] = cy + (q[1] - cy) * zoom + dy;
        }
        k
    });
    Some(Layer {
        clip: c.id,
        asset: c.asset,
        source_time,
        x: pr.x.eval(local) + dx,
        y: pr.y.eval(local) + dy,
        scale: pr.scale.eval(local) * zoom,
        rotation: pr.rotation.eval(local),
        opacity,
        rot_x: pr.rot_x.eval(local),
        rot_y: pr.rot_y.eval(local),
        local,
        corners,
        mask: c.mask.map(|m| LayerMask {
            shape: m.shape,
            invert: m.invert,
            x: pr.mask_x.eval(local),
            y: pr.mask_y.eval(local),
            w: pr.mask_w.eval(local).max(0.0),
            h: pr.mask_h.eval(local).max(0.0),
            feather: pr.mask_feather.eval(local).max(0.0),
        }),
        wipe: if held { None } else { wipe },
        slot: held as u8,
        fx: LayerFx {
            brightness: pr.brightness.eval(local).clamp(-1.0, 1.0),
            contrast: pr.contrast.eval(local).clamp(-1.0, 1.0),
            saturation: pr.saturation.eval(local).clamp(-1.0, 1.0),
            hue: pr.hue.eval(local),
            blur: pr.blur.eval(local).clamp(0.0, 500.0),
        },
        key: c.key.as_ref().map(|k| {
            let c = k.rgb();
            ([c[0] as f64 / 255.0, c[1] as f64 / 255.0, c[2] as f64 / 255.0], k.tolerance, k.softness, k.spill)
        }),
    })
}

/// 時刻 t に表示する映像レイヤー。
pub fn layers_at(p: &Project, t: Time) -> Vec<Layer> {
    let mut out = Vec::new();
    for track in p.tracks.iter().filter(|tr| tr.kind == TrackKind::Video && !tr.muted) {
        let i = track.clips.partition_point(|c| c.end() <= t);
        let Some(c) = track.clips.get(i).filter(|c| c.contains(t)) else { continue };
        // 始まりのトランジション中は、すぐ前に接しているクリップを下に表示し続ける(クロスフェードなど)
        if let (Some(tr), Some(prev)) = (c.transition_in, i.checked_sub(1).map(|j| &track.clips[j])) {
            if t - c.start < tr.duration && prev.end() == c.start {
                out.extend(clip_layer(p, prev, t, true));
            }
        }
        out.extend(clip_layer(p, c, t, false));
    }
    out
}

/// 区間 [from, to) で鳴る音声の部分。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSpan {
    pub track: Id,
    pub clip: Id,
    pub asset: Id,
    /// タイムライン上の区間
    pub start: Time,
    pub end: Time,
    /// start に対応する素材の時刻
    pub source_start: Time,
    /// クリップの開始時刻(音量などのキーフレームの基準)
    pub clip_start: Time,
    /// 再生速度(素材を何倍の速さで読むか)
    pub speed: f64,
    /// 逆再生(source_start から素材を後ろへ向かって読む)
    pub reverse: bool,
    /// 音の高さを保つ
    pub keep_pitch: bool,
}

pub fn audio_spans(p: &Project, from: Time, to: Time) -> Vec<AudioSpan> {
    track_audio_spans(p, from, to, false)
}

/// 区間 [from, to) で鳴る音声の部分。`with_muted` なら消音中のトラックも含める(ダッキングの判定用)。
pub fn track_audio_spans(p: &Project, from: Time, to: Time, with_muted: bool) -> Vec<AudioSpan> {
    let mut out = Vec::new();
    for track in p.tracks.iter().filter(|tr| tr.kind == TrackKind::Audio && (with_muted || !tr.muted)) {
        for c in &track.clips {
            if c.end() <= from || c.start >= to {
                continue;
            }
            let s = c.start.max(from);
            let e = c.end().min(to);
            out.push(AudioSpan { track: track.id, clip: c.id, asset: c.asset, start: s, end: e, source_start: c.source_at(s - c.start), clip_start: c.start, speed: c.speed, reverse: c.reverse, keep_pitch: c.keep_pitch });
        }
    }
    out
}
