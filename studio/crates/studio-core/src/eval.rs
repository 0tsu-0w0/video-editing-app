//! ある時刻に何をどう描くか・鳴らすかを決める(描画・音声エンジンへの指示)。

use crate::project::{Id, Project, TrackKind};
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
    pub corners: Option<crate::project::Corners>,
}

/// 時刻 t に表示する映像レイヤー。
pub fn layers_at(p: &Project, t: Time) -> Vec<Layer> {
    let mut out = Vec::new();
    for track in p.tracks.iter().filter(|tr| tr.kind == TrackKind::Video && !tr.muted) {
        let i = track.clips.partition_point(|c| c.end() <= t);
        let Some(c) = track.clips.get(i).filter(|c| c.contains(t)) else { continue };
        let local = t - c.start;
        let pr = &c.params;
        let opacity = pr.opacity.eval(local).clamp(0.0, 1.0) * c.fade_gain(local);
        if opacity <= 0.0 {
            continue;
        }
        out.push(Layer {
            clip: c.id,
            asset: c.asset,
            source_time: c.source_in + local,
            x: pr.x.eval(local),
            y: pr.y.eval(local),
            scale: pr.scale.eval(local),
            rotation: pr.rotation.eval(local),
            opacity,
            rot_x: pr.rot_x.eval(local),
            rot_y: pr.rot_y.eval(local),
            local,
            corners: crate::project::eval_pin(&c.pin, local),
        });
    }
    out
}

/// 区間 [from, to) で鳴る音声の部分。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSpan {
    pub clip: Id,
    pub asset: Id,
    /// タイムライン上の区間
    pub start: Time,
    pub end: Time,
    /// start に対応する素材の時刻
    pub source_start: Time,
    /// クリップの開始時刻(音量などのキーフレームの基準)
    pub clip_start: Time,
}

pub fn audio_spans(p: &Project, from: Time, to: Time) -> Vec<AudioSpan> {
    let mut out = Vec::new();
    for track in p.tracks.iter().filter(|tr| tr.kind == TrackKind::Audio && !tr.muted) {
        for c in &track.clips {
            if c.end() <= from || c.start >= to {
                continue;
            }
            let s = c.start.max(from);
            let e = c.end().min(to);
            out.push(AudioSpan { clip: c.id, asset: c.asset, start: s, end: e, source_start: c.source_in + (s - c.start), clip_start: c.start });
        }
    }
    out
}
