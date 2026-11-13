//! 編集コマンド。画面操作・AI・MCP は、すべてこのコマンドを通してプロジェクトを変更する。
//!
//! 各コマンドは適用時に「元に戻すためのコマンド列」を返す。Undo はそれを実行するだけ。
//! 新しい ID を作るコマンドは、作った ID を記録した形(`id` 付き)で履歴に残し、
//! Redo で同じ ID が再現されるようにする。

use crate::project::*;
use crate::time::{parse_time, Rate, Time};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// コマンド中の時刻: JSON では秒の数値、または `"00:01:02.500"` / `"62.5s"` の文字列。
pub mod secs {
    use super::*;
    pub fn serialize<S: Serializer>(t: &Time, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(t.secs_f64())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Time, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum T {
            N(f64),
            S(String),
        }
        match T::deserialize(d)? {
            T::N(n) if n.is_finite() => Ok(Time::from_secs_f64(n)),
            T::N(_) => Err(serde::de::Error::custom("時刻が数値ではありません")),
            T::S(s) => parse_time(&s).ok_or_else(|| serde::de::Error::custom(format!("時刻を読めません: {s}"))),
        }
    }
}

pub mod opt_secs {
    use super::*;
    pub fn serialize<S: Serializer>(t: &Option<Time>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(t) => s.serialize_f64(t.secs_f64()),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Time>, D::Error> {
        #[derive(Deserialize)]
        struct W(#[serde(with = "secs")] Time);
        Ok(Option::<W>::deserialize(d)?.map(|w| w.0))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    // ---- トラック
    AddTrack {
        kind: TrackKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// 挿入位置(0 = 最も奥)。省略時は最も手前
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Id>,
    },
    RemoveTrack { track: Id },
    RenameTrack { track: Id, name: String },
    MoveTrack { track: Id, index: usize },
    SetTrackFlags {
        track: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        muted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locked: Option<bool>,
    },
    /// 音声トラックの音量(dB)・左右のバランス・ソロ(省略した項目はそのまま)
    SetTrackAudio {
        track: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gain_db: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pan: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        solo: Option<bool>,
    },
    /// 音声トラックのエフェクトを、この並びに置き換える
    SetTrackEffects {
        track: Id,
        #[serde(default)]
        effects: Vec<AudioEffect>,
    },
    /// 全体の音量(dB)
    SetMasterGain { gain_db: f64 },

    // ---- 素材
    ImportAsset {
        name: String,
        path: String,
        /// 素材の種類と情報(例: {"type": "text", "text": "...", "style": {...}})
        kind: AssetKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Id>,
    },
    RemoveAsset { asset: Id },

    // ---- クリップ
    AddClip {
        track: Id,
        asset: Id,
        #[serde(with = "secs")]
        start: Time,
        /// 省略時は素材の終わりまで(静止画は 5 秒)
        #[serde(default, with = "opt_secs", skip_serializing_if = "Option::is_none")]
        duration: Option<Time>,
        #[serde(default, with = "opt_secs", skip_serializing_if = "Option::is_none")]
        source_in: Option<Time>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Id>,
    },
    RemoveClip { clip: Id },
    MoveClip {
        clip: Id,
        #[serde(with = "secs")]
        start: Time,
        /// 別のトラックへ移す場合
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<Id>,
    },
    /// 開始・長さ・素材の使用開始位置を直接指定する(省略した項目はそのまま)
    TrimClip {
        clip: Id,
        #[serde(default, with = "opt_secs", skip_serializing_if = "Option::is_none")]
        start: Option<Time>,
        #[serde(default, with = "opt_secs", skip_serializing_if = "Option::is_none")]
        duration: Option<Time>,
        #[serde(default, with = "opt_secs", skip_serializing_if = "Option::is_none")]
        source_in: Option<Time>,
    },
    /// タイムライン上の時刻 `at` で2つに分ける
    SplitClip {
        clip: Id,
        #[serde(with = "secs")]
        at: Time,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_id: Option<Id>,
    },

    /// フェードイン・アウトの長さ(省略した方はそのまま。0 で無し)
    SetFade {
        clip: Id,
        #[serde(default, with = "opt_secs", skip_serializing_if = "Option::is_none")]
        fade_in: Option<Time>,
        #[serde(default, with = "opt_secs", skip_serializing_if = "Option::is_none")]
        fade_out: Option<Time>,
    },
    /// 文字素材の内容や見た目を変える(省略した項目はそのまま)
    SetText {
        asset: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outline: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outline_color: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<String>,
    },

    /// 四隅の位置のキーフレームを置き換える(平面への貼り付け)。keys が空なら貼り付けを外す
    SetPin {
        clip: Id,
        #[serde(default)]
        keys: Vec<PinKey>,
    },

    /// マスクを付ける・変える(mask が無ければ外す)。初めて付けるときは、素材の中央に半分の大きさで置く
    SetMask {
        clip: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mask: Option<Mask>,
    },
    /// トランジションを付ける・外す(edge: "in" は始まり、"out" は終わり。transition が無ければ外す)
    SetTransition {
        clip: Id,
        edge: Edge,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transition: Option<Transition>,
    },

    // ---- 値・キーフレーム
    /// 値を定数にする(キーフレームは消える)
    SetParam { clip: Id, param: String, value: f64 },
    /// キーフレームを置く(時刻はクリップの開始から)
    SetKey {
        clip: Id,
        param: String,
        #[serde(with = "secs")]
        time: Time,
        value: f64,
        #[serde(default)]
        interp: Interp,
    },
    RemoveKey {
        clip: Id,
        param: String,
        #[serde(with = "secs")]
        time: Time,
    },

    // ---- プロジェクト設定
    SetSettings {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_rate: Option<Rate>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample_rate: Option<u32>,
    },

    // ---- 内部用(Undo のための復元。AI には公開しない)
    #[doc(hidden)]
    RestoreTrack { index: usize, track: Track },
    #[doc(hidden)]
    RestoreClip { track: Id, clip: Clip },
    #[doc(hidden)]
    RestoreAsset { asset: Asset },
    #[doc(hidden)]
    RestoreParam { clip: Id, param: String, state: Param },
}

impl Command {
    /// Undo 専用の内部コマンドか。外部(AI・MCP)からは受け付けない。
    pub fn is_internal(&self) -> bool {
        matches!(
            self,
            Command::RestoreTrack { .. } | Command::RestoreClip { .. } | Command::RestoreAsset { .. } | Command::RestoreParam { .. }
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Command::AddTrack { .. } => "add_track",
            Command::RemoveTrack { .. } => "remove_track",
            Command::RenameTrack { .. } => "rename_track",
            Command::MoveTrack { .. } => "move_track",
            Command::SetTrackFlags { .. } => "set_track_flags",
            Command::SetTrackAudio { .. } => "set_track_audio",
            Command::SetTrackEffects { .. } => "set_track_effects",
            Command::SetMasterGain { .. } => "set_master_gain",
            Command::ImportAsset { .. } => "import_asset",
            Command::RemoveAsset { .. } => "remove_asset",
            Command::AddClip { .. } => "add_clip",
            Command::RemoveClip { .. } => "remove_clip",
            Command::MoveClip { .. } => "move_clip",
            Command::TrimClip { .. } => "trim_clip",
            Command::SplitClip { .. } => "split_clip",
            Command::SetFade { .. } => "set_fade",
            Command::SetText { .. } => "set_text",
            Command::SetPin { .. } => "set_pin",
            Command::SetMask { .. } => "set_mask",
            Command::SetTransition { .. } => "set_transition",
            Command::SetParam { .. } => "set_param",
            Command::SetKey { .. } => "set_key",
            Command::RemoveKey { .. } => "remove_key",
            Command::SetSettings { .. } => "set_settings",
            Command::RestoreTrack { .. } => "restore_track",
            Command::RestoreClip { .. } => "restore_clip",
            Command::RestoreAsset { .. } => "restore_asset",
            Command::RestoreParam { .. } => "restore_param",
        }
    }
}

/// クリップの端(始まり・終わり)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    In,
    Out,
}

/// 失敗の理由。AI へそのまま返して修正させられるよう、具体的に書く。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("[{code}] {message}")]
pub struct EditError {
    pub code: &'static str,
    pub message: String,
}

fn err<T>(code: &'static str, message: impl Into<String>) -> Result<T, EditError> {
    Err(EditError { code, message: message.into() })
}

/// 1 つのコマンドを適用した結果。
pub struct Applied {
    /// 元に戻すためのコマンド(この順に実行する)
    pub inverse: Vec<Command>,
    /// 履歴に残す形(新しい ID を記録したもの)
    pub resolved: Command,
    /// 新しく作られたもの
    pub created: Option<Id>,
    /// 人が読む変更の要約
    pub summary: String,
}

fn track_mut(p: &mut Project, id: Id) -> Result<&mut Track, EditError> {
    p.tracks.iter_mut().find(|t| t.id == id).map_or_else(|| err("not_found", format!("トラック {id} はありません")), Ok)
}

fn unlocked(t: &Track) -> Result<(), EditError> {
    if t.locked {
        return err("locked", format!("トラック「{}」({})はロックされています", t.name, t.id));
    }
    Ok(())
}

/// クリップを、重なりが無ければトラックに挿入する(開始時刻の順を保つ)。
fn insert_clip(t: &mut Track, clip: Clip) -> Result<(), EditError> {
    let i = t.clips.partition_point(|c| c.start < clip.start);
    if let Some(prev) = i.checked_sub(1).map(|j| &t.clips[j]) {
        if prev.end() > clip.start {
            return err(
                "overlap",
                format!("{}〜{} はトラック {} のクリップ {}({}〜{})と重なります", clip.start, clip.end(), t.id, prev.id, prev.start, prev.end()),
            );
        }
    }
    if let Some(next) = t.clips.get(i) {
        if clip.end() > next.start {
            return err(
                "overlap",
                format!("{}〜{} はトラック {} のクリップ {}({}〜{})と重なります", clip.start, clip.end(), t.id, next.id, next.start, next.end()),
            );
        }
    }
    t.clips.insert(i, clip);
    Ok(())
}

/// クリップの時間が素材と合っているか。
fn check_clip_time(p: &Project, c: &Clip) -> Result<(), EditError> {
    if c.start < Time::ZERO {
        return err("invalid", format!("開始時刻 {} が負です", c.start));
    }
    if c.duration <= Time::ZERO {
        return err("invalid", format!("長さ {} は 0 より大きくしてください", c.duration));
    }
    if c.source_in < Time::ZERO {
        return err("invalid", "素材の使用開始位置が負です");
    }
    let a = p.assets.get(&c.asset).ok_or(EditError { code: "not_found", message: format!("素材 {} はありません", c.asset) })?;
    if let Some(d) = a.kind.duration() {
        if c.source_in + c.duration > d {
            return err(
                "out_of_range",
                format!("素材「{}」の長さは {} です。使用開始 {} + 長さ {} が超えています", a.name, d, c.source_in, c.duration),
            );
        }
    }
    Ok(())
}

fn check_param(name: &str, value: f64) -> Result<(), EditError> {
    let Some((_, lo, hi)) = PARAMS.iter().find(|(n, _, _)| *n == name) else {
        let names: Vec<&str> = PARAMS.iter().map(|p| p.0).collect();
        return err("invalid", format!("項目 {name:?} はありません。使える項目: {}", names.join(", ")));
    };
    if !value.is_finite() || value < *lo || value > *hi {
        return err("out_of_range", format!("{name} は {lo}〜{hi} の範囲で指定してください({value})"));
    }
    Ok(())
}

fn clip_mut(p: &mut Project, id: Id) -> Result<(&mut Track, usize), EditError> {
    let (ti, ci) = p.find_clip(id).ok_or(EditError { code: "not_found", message: format!("クリップ {id} はありません") })?;
    Ok((&mut p.tracks[ti], ci))
}

/// 新しい ID を決める(履歴の再実行では記録済みの ID を使う)。
fn take_id(p: &mut Project, given: Option<Id>) -> Result<Id, EditError> {
    match given {
        Some(id) => {
            if p.assets.contains_key(&id) || p.track(id).is_some() || p.find_clip(id).is_some() {
                return err("invalid", format!("ID {id} は使用中です"));
            }
            p.next_id = p.next_id.max(id.0 + 1);
            Ok(id)
        }
        None => Ok(p.alloc_id()),
    }
}

pub fn apply(p: &mut Project, cmd: &Command) -> Result<Applied, EditError> {
    use Command::*;
    let done = |inverse, resolved, created, summary| Ok(Applied { inverse, resolved, created, summary });
    match cmd.clone() {
        AddTrack { kind, name, index, id } => {
            let index = index.unwrap_or(p.tracks.len());
            if index > p.tracks.len() {
                return err("out_of_range", format!("index は 0〜{} で指定してください", p.tracks.len()));
            }
            let id = take_id(p, id)?;
            let n = p.tracks.iter().filter(|t| t.kind == kind).count() + 1;
            let name = name.unwrap_or_else(|| format!("{}{}", if kind == TrackKind::Video { "映像" } else { "音声" }, n));
            p.tracks.insert(index, Track { id, kind, name: name.clone(), muted: false, locked: false, clips: vec![], audio: Default::default() });
            done(vec![RemoveTrack { track: id }], AddTrack { kind, name: Some(name.clone()), index: Some(index), id: Some(id) }, Some(id), format!("トラック「{name}」({id})を追加"))
        }
        RemoveTrack { track } => {
            let index = p.track_index(track).ok_or(EditError { code: "not_found", message: format!("トラック {track} はありません") })?;
            unlocked(&p.tracks[index])?;
            let t = p.tracks.remove(index);
            let s = format!("トラック「{}」({})とクリップ {} 個を削除", t.name, t.id, t.clips.len());
            done(vec![RestoreTrack { index, track: t }], cmd.clone(), None, s)
        }
        RenameTrack { track, name } => {
            if name.trim().is_empty() || name.chars().count() > 64 {
                return err("invalid", "名前は 1〜64 文字にしてください");
            }
            let t = track_mut(p, track)?;
            let old = std::mem::replace(&mut t.name, name.clone());
            done(vec![RenameTrack { track, name: old.clone() }], cmd.clone(), None, format!("トラック「{old}」を「{name}」に変更"))
        }
        MoveTrack { track, index } => {
            let from = p.track_index(track).ok_or(EditError { code: "not_found", message: format!("トラック {track} はありません") })?;
            if index >= p.tracks.len() {
                return err("out_of_range", format!("index は 0〜{} で指定してください", p.tracks.len() - 1));
            }
            let t = p.tracks.remove(from);
            p.tracks.insert(index, t);
            done(vec![MoveTrack { track, index: from }], cmd.clone(), None, format!("トラック {track} を {from} 番目から {index} 番目へ移動"))
        }
        SetTrackFlags { track, muted, locked } => {
            let t = track_mut(p, track)?;
            let inv = SetTrackFlags { track, muted: muted.map(|_| t.muted), locked: locked.map(|_| t.locked) };
            if let Some(m) = muted {
                t.muted = m;
            }
            if let Some(l) = locked {
                t.locked = l;
            }
            done(vec![inv], cmd.clone(), None, format!("トラック {track} の設定を変更"))
        }
        ImportAsset { name, path, kind, id } => {
            let is_text = matches!(kind, AssetKind::Text { .. });
            if name.trim().is_empty() || (path.trim().is_empty() && !is_text) {
                return err("invalid", "素材の名前とパスを指定してください");
            }
            if let AssetKind::Text { text, style } = &kind {
                if text.chars().count() > 10_000 {
                    return err("invalid", "文字が長すぎます(1 万文字まで)");
                }
                style.check().or_else(|m| err("invalid", m))?;
            }
            if let AssetKind::Model { animation } = &kind {
                if !animation.is_finite() || *animation < 0.0 {
                    return err("invalid", "アニメーションの長さが不正です");
                }
            }
            if let Some(d) = kind.duration() {
                if d <= Time::ZERO {
                    return err("invalid", "素材の長さが 0 です");
                }
            }
            let id = take_id(p, id)?;
            p.assets.insert(id, Asset { id, name: name.clone(), path: path.clone(), kind: kind.clone() });
            done(vec![RemoveAsset { asset: id }], ImportAsset { name: name.clone(), path, kind, id: Some(id) }, Some(id), format!("素材「{name}」({id})を読み込み"))
        }
        RemoveAsset { asset } => {
            if p.tracks.iter().any(|t| t.clips.iter().any(|c| c.asset == asset)) {
                return err("in_use", format!("素材 {asset} はクリップで使われています。先にクリップを削除してください"));
            }
            let a = p.assets.remove(&asset).ok_or(EditError { code: "not_found", message: format!("素材 {asset} はありません") })?;
            let s = format!("素材「{}」({})を削除", a.name, a.id);
            done(vec![RestoreAsset { asset: a }], cmd.clone(), None, s)
        }
        AddClip { track, asset, start, duration, source_in, id } => {
            let a = p.assets.get(&asset).ok_or(EditError { code: "not_found", message: format!("素材 {asset} はありません") })?.clone();
            let t = p.track(track).ok_or(EditError { code: "not_found", message: format!("トラック {track} はありません") })?;
            unlocked(t)?;
            let ok = match t.kind {
                TrackKind::Video => a.kind.has_video(),
                TrackKind::Audio => a.kind.has_audio(),
            };
            if !ok {
                return err("invalid", format!("素材「{}」は{}トラックに置けません", a.name, if t.kind == TrackKind::Video { "映像" } else { "音声" }));
            }
            let source_in = source_in.unwrap_or(Time::ZERO);
            let duration = match (duration, a.kind.duration()) {
                (Some(d), _) => d,
                (None, Some(d)) => d - source_in,
                (None, None) => Time::from_secs_f64(5.0),
            };
            let id = take_id(p, id)?;
            let clip = Clip { id, asset, start, duration, source_in, params: ClipParams::default(), fade_in: Time::ZERO, fade_out: Time::ZERO, pin: vec![], mask: None, transition_in: None, transition_out: None };
            check_clip_time(p, &clip)?;
            insert_clip(track_mut(p, track)?, clip)?;
            done(
                vec![RemoveClip { clip: id }],
                AddClip { track, asset, start, duration: Some(duration), source_in: Some(source_in), id: Some(id) },
                Some(id),
                format!("クリップ {id}(素材「{}」)をトラック {track} の {start}〜{} に追加", a.name, start + duration),
            )
        }
        RemoveClip { clip } => {
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            let c = t.clips.remove(ci);
            let s = format!("クリップ {clip}({}〜{})を削除", c.start, c.end());
            let tid = t.id;
            done(vec![RestoreClip { track: tid, clip: c }], cmd.clone(), None, s)
        }
        MoveClip { clip, start, track } => {
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            let from = t.id;
            let mut c = t.clips.remove(ci);
            let old_start = c.start;
            let to = track.unwrap_or(from);
            c.start = start;
            let result = (|| {
                let tt = p.track(to).ok_or(EditError { code: "not_found", message: format!("トラック {to} はありません") })?;
                unlocked(tt)?;
                let a = &p.assets[&c.asset];
                let ok = match tt.kind {
                    TrackKind::Video => a.kind.has_video(),
                    TrackKind::Audio => a.kind.has_audio(),
                };
                if !ok {
                    return err("invalid", "素材の種類がトラックと合いません");
                }
                check_clip_time(p, &c)?;
                insert_clip(track_mut(p, to)?, c.clone())
            })();
            if let Err(e) = result {
                c.start = old_start;
                insert_clip(track_mut(p, from)?, c).expect("元の位置に戻せない");
                return Err(e);
            }
            done(
                vec![MoveClip { clip, start: old_start, track: (to != from).then_some(from) }],
                cmd.clone(),
                None,
                format!("クリップ {clip} を {old_start} から {start} へ移動{}", if to != from { format!("(トラック {from} → {to})") } else { String::new() }),
            )
        }
        TrimClip { clip, start, duration, source_in } => {
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            let tid = t.id;
            let old = t.clips.remove(ci);
            let mut c = old.clone();
            if let Some(s) = start {
                c.start = s;
            }
            if let Some(d) = duration {
                c.duration = d;
            }
            if let Some(s) = source_in {
                c.source_in = s;
            }
            // フェードが長さに収まらなければ縮める
            c.fade_in = c.fade_in.min(c.duration);
            c.fade_out = c.fade_out.min(c.duration - c.fade_in);
            // 収まらないトランジションは外す
            if c.transition_in.is_some_and(|t| t.duration > c.duration) {
                c.transition_in = None;
            }
            let ti = c.transition_in.map_or(Time::ZERO, |t| t.duration);
            if c.transition_out.is_some_and(|t| ti + t.duration > c.duration) {
                c.transition_out = None;
            }
            let r = check_clip_time(p, &c).and_then(|_| insert_clip(track_mut(p, tid)?, c.clone()));
            if let Err(e) = r {
                insert_clip(track_mut(p, tid)?, old).expect("元に戻せない");
                return Err(e);
            }
            done(
                vec![RemoveClip { clip }, RestoreClip { track: tid, clip: old.clone() }],
                cmd.clone(),
                None,
                format!("クリップ {clip} を {}〜{} に変更", c.start, c.end()),
            )
        }
        SplitClip { clip, at, new_id } => {
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            let tid = t.id;
            let c = t.clips[ci].clone();
            if at <= c.start || at >= c.end() {
                return err("out_of_range", format!("分割位置 {at} はクリップ {clip}({}〜{})の内側にしてください", c.start, c.end()));
            }
            let new_id = take_id(p, new_id)?;
            let (t, ci) = clip_mut(p, clip)?;
            let left = at - c.start;
            t.clips[ci].duration = left;
            // 右側のキーフレームは、新しいクリップの開始基準に付け直す
            let mut right = Clip { id: new_id, start: at, duration: c.duration - left, source_in: c.source_in + left, fade_in: Time::ZERO, ..c.clone() };
            right.fade_out = right.fade_out.min(right.duration);
            right.transition_in = None;
            if right.transition_out.is_some_and(|t| t.duration > right.duration) {
                right.transition_out = None;
            }
            t.clips[ci].transition_out = None;
            if t.clips[ci].transition_in.is_some_and(|t| t.duration > left) {
                t.clips[ci].transition_in = None;
            }
            t.clips[ci].fade_out = Time::ZERO;
            t.clips[ci].fade_in = t.clips[ci].fade_in.min(left);
            for (name, _, _) in PARAMS {
                let (lp, rp) = (t.clips[ci].params.get_mut(name).unwrap(), right.params.get_mut(name).unwrap());
                if !lp.keys.is_empty() {
                    let v_at = c.params.get(name).unwrap().eval(left);
                    rp.keys = lp.keys.iter().filter(|k| k.time > left).map(|k| Key { time: k.time - left, ..*k }).collect();
                    rp.keys.insert(0, Key { time: Time::ZERO, value: v_at, interp: Interp::Linear });
                    lp.keys.retain(|k| k.time <= left);
                    if lp.keys.last().map_or(true, |k| k.time < left) {
                        lp.keys.push(Key { time: left, value: v_at, interp: Interp::Linear });
                    }
                }
            }
            if !c.pin.is_empty() {
                let at_left = eval_pin(&c.pin, left).unwrap();
                let lp = &mut t.clips[ci].pin;
                right.pin = lp.iter().filter(|k| k.time > left).map(|k| PinKey { time: k.time - left, ..*k }).collect();
                right.pin.insert(0, PinKey { time: Time::ZERO, corners: at_left });
                lp.retain(|k| k.time <= left);
                if lp.last().map_or(true, |k| k.time < left) {
                    lp.push(PinKey { time: left, corners: at_left });
                }
            }
            insert_clip(t, right)?;
            done(
                // 取り消し: 右側を消し、変更した左側を消して、分割前のクリップを丸ごと戻す
                vec![RemoveClip { clip: new_id }, RemoveClip { clip }, RestoreClip { track: tid, clip: c.clone() }],
                SplitClip { clip, at, new_id: Some(new_id) },
                Some(new_id),
                format!("クリップ {clip} を {at} で分割(右側はクリップ {new_id})"),
            )
        }
        SetFade { clip, fade_in, fade_out } => {
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            let tid = t.id;
            let old = t.clips[ci].clone();
            let (fi, fo) = (fade_in.unwrap_or(old.fade_in), fade_out.unwrap_or(old.fade_out));
            if fi < Time::ZERO || fo < Time::ZERO {
                return err("invalid", "フェードの長さが負です");
            }
            if fi + fo > old.duration {
                return err("out_of_range", format!("フェードイン {fi} + フェードアウト {fo} が、クリップの長さ {} を超えています", old.duration));
            }
            t.clips[ci].fade_in = fi;
            t.clips[ci].fade_out = fo;
            done(vec![RemoveClip { clip }, RestoreClip { track: tid, clip: old }], cmd.clone(), None, format!("クリップ {clip} のフェードをイン {fi}・アウト {fo} に設定"))
        }
        SetMask { clip, mask } => {
            let size = {
                let c = p.clip(clip).ok_or(EditError { code: "not_found", message: format!("クリップ {clip} はありません") })?;
                match p.assets.get(&c.asset).map(|a| &a.kind) {
                    Some(AssetKind::Video { width, height, .. }) | Some(AssetKind::Image { width, height }) => Some((*width as f64, *height as f64)),
                    _ => None,
                }
            };
            let fallback = (p.settings.width as f64, p.settings.height as f64);
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            if t.kind != TrackKind::Video {
                return err("invalid", "マスクは映像トラックのクリップだけです");
            }
            let tid = t.id;
            let old = t.clips[ci].clone();
            let c = &mut t.clips[ci];
            // 初めて付けるときは、大きさが 0 なら素材の半分にする
            if mask.is_some() && c.params.mask_w.value == 0.0 && c.params.mask_w.keys.is_empty() && c.params.mask_h.keys.is_empty() {
                let (w, h) = size.unwrap_or(fallback);
                c.params.mask_w = Param::constant((w / 2.0).round());
                c.params.mask_h = Param::constant((h / 2.0).round());
            }
            c.mask = mask;
            let s = match mask {
                Some(m) => format!("クリップ {clip} に{}のマスク{}", if m.shape == MaskShape::Rect { "四角" } else { "楕円" }, if m.invert { "(反転)" } else { "" }),
                None => format!("クリップ {clip} のマスクを外す"),
            };
            done(vec![RemoveClip { clip }, RestoreClip { track: tid, clip: old }], cmd.clone(), None, s)
        }
        SetTransition { clip, edge, transition } => {
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            if t.kind != TrackKind::Video {
                return err("invalid", "トランジションは映像トラックのクリップだけです(音声はフェードを使ってください)");
            }
            let tid = t.id;
            let old = t.clips[ci].clone();
            let c = &mut t.clips[ci];
            if let Some(tr) = transition {
                if tr.duration <= Time::ZERO {
                    return err("invalid", "トランジションの長さは 0 より大きくしてください");
                }
                let other = match edge {
                    Edge::In => c.transition_out,
                    Edge::Out => c.transition_in,
                }
                .map_or(Time::ZERO, |t| t.duration);
                if tr.duration + other > c.duration {
                    return err("out_of_range", format!("トランジションの長さ {} が、クリップの長さ {} に収まりません(反対側のトランジション {other} を含む)", tr.duration, c.duration));
                }
            }
            match edge {
                Edge::In => c.transition_in = transition,
                Edge::Out => c.transition_out = transition,
            }
            let side = if edge == Edge::In { "始まり" } else { "終わり" };
            let s = match transition {
                Some(tr) => format!("クリップ {clip} の{side}に {:?} のトランジション({})", tr.kind, tr.duration),
                None => format!("クリップ {clip} の{side}のトランジションを外す"),
            };
            done(vec![RemoveClip { clip }, RestoreClip { track: tid, clip: old }], cmd.clone(), None, s)
        }
        SetPin { clip, keys } => {
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            if t.kind != TrackKind::Video {
                return err("invalid", "四隅の貼り付けは映像トラックのクリップだけです");
            }
            check_pin(&keys).map_err(|m| EditError { code: "invalid", message: m })?;
            let tid = t.id;
            let old = t.clips[ci].clone();
            let n = keys.len();
            t.clips[ci].pin = keys;
            let s = if n == 0 { format!("クリップ {clip} の四隅の貼り付けを外す") } else { format!("クリップ {clip} の四隅をキー {n} 個で設定") };
            done(vec![RemoveClip { clip }, RestoreClip { track: tid, clip: old }], cmd.clone(), None, s)
        }
        SetText { asset, text, font, size, color, outline, outline_color, align } => {
            let a = p.assets.get_mut(&asset).ok_or(EditError { code: "not_found", message: format!("素材 {asset} はありません") })?;
            let old = a.clone();
            let AssetKind::Text { text: t, style } = &mut a.kind else {
                return err("invalid", format!("素材 {asset} は文字ではありません"));
            };
            if let Some(v) = text {
                if v.chars().count() > 10_000 {
                    return err("invalid", "文字が長すぎます(1 万文字まで)");
                }
                *t = v;
            }
            let mut st = style.clone();
            if let Some(v) = font { st.font = v; }
            if let Some(v) = size { st.size = v; }
            if let Some(v) = color { st.color = v; }
            if let Some(v) = outline { st.outline = v; }
            if let Some(v) = outline_color { st.outline_color = v; }
            if let Some(v) = align { st.align = v; }
            if let Err(m) = st.check() {
                *a = old;
                return err("invalid", m);
            }
            *style = st;
            if !t.is_empty() && a.name.starts_with("文字") {
                a.name = format!("文字「{}」", t.chars().take(12).collect::<String>().replace('\n', " "));
            }
            let s = format!("文字素材 {asset} を変更");
            done(vec![RestoreAsset { asset: old }], cmd.clone(), None, s)
        }
        SetParam { clip, param, value } => {
            check_param(&param, value)?;
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            let slot = t.clips[ci].params.get_mut(&param).unwrap();
            let old = std::mem::replace(slot, Param::constant(value));
            done(vec![RestoreParam { clip, param: param.clone(), state: old }], cmd.clone(), None, format!("クリップ {clip} の {param} を {value} に設定"))
        }
        SetKey { clip, param, time, value, interp } => {
            check_param(&param, value)?;
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            let dur = t.clips[ci].duration;
            if time < Time::ZERO || time > dur {
                return err("out_of_range", format!("キーの時刻 {time} はクリップの長さ {dur} の範囲内にしてください(クリップの開始からの時刻)"));
            }
            let slot = t.clips[ci].params.get_mut(&param).unwrap();
            let old = slot.clone();
            slot.set_key(Key { time, value, interp });
            done(vec![RestoreParam { clip, param: param.clone(), state: old }], cmd.clone(), None, format!("クリップ {clip} の {param} に {time} = {value} のキーを設定"))
        }
        RemoveKey { clip, param, time } => {
            check_param(&param, PARAMS.iter().find(|x| x.0 == param).map_or(0.0, |x| x.1))?;
            let (t, ci) = clip_mut(p, clip)?;
            unlocked(t)?;
            let slot = t.clips[ci].params.get_mut(&param).unwrap();
            let old = slot.clone();
            let Some(i) = slot.keys.iter().position(|k| k.time == time) else {
                return err("not_found", format!("クリップ {clip} の {param} に時刻 {time} のキーはありません"));
            };
            let removed = slot.keys.remove(i);
            if slot.keys.is_empty() {
                slot.value = removed.value;
            }
            done(vec![RestoreParam { clip, param: param.clone(), state: old }], cmd.clone(), None, format!("クリップ {clip} の {param} のキー({time})を削除"))
        }
        SetTrackAudio { track, gain_db, pan, solo } => {
            let t = track_mut(p, track)?;
            if t.kind != TrackKind::Audio {
                return err("invalid", format!("トラック {track} は音声トラックではありません"));
            }
            if gain_db.is_some_and(|g| !g.is_finite() || !(-60.0..=12.0).contains(&g)) {
                return err("out_of_range", "gain_db は -60〜12 にしてください");
            }
            if pan.is_some_and(|v| !v.is_finite() || !(-1.0..=1.0).contains(&v)) {
                return err("out_of_range", "pan は -1〜1 にしてください");
            }
            let a = &mut t.audio;
            let inv = SetTrackAudio { track, gain_db: gain_db.map(|_| a.gain_db), pan: pan.map(|_| a.pan), solo: solo.map(|_| a.solo) };
            if let Some(v) = gain_db {
                a.gain_db = v;
            }
            if let Some(v) = pan {
                a.pan = v;
            }
            if let Some(v) = solo {
                a.solo = v;
            }
            done(vec![inv], cmd.clone(), None, format!("トラック {track} の音量 {:.1}dB・バランス {:.2}{}", a.gain_db, a.pan, if a.solo { "・ソロ" } else { "" }))
        }
        SetTrackEffects { track, effects } => {
            let audio_tracks: Vec<Id> = p.tracks.iter().filter(|t| t.kind == TrackKind::Audio).map(|t| t.id).collect();
            if effects.len() > 16 {
                return err("invalid", "エフェクトは 16 個までです");
            }
            for (i, e) in effects.iter().enumerate() {
                e.check().map_err(|m| EditError { code: "out_of_range", message: format!("エフェクト {i}: {m}") })?;
                if let AudioEffect::Duck { source, .. } = e {
                    if *source == track || !audio_tracks.contains(source) {
                        return err("invalid", format!("エフェクト {i}: ダッキングの source には、別の音声トラックの ID を指定してください"));
                    }
                }
            }
            let t = track_mut(p, track)?;
            if t.kind != TrackKind::Audio {
                return err("invalid", format!("トラック {track} は音声トラックではありません"));
            }
            let old = std::mem::replace(&mut t.audio.effects, effects);
            let n = t.audio.effects.len();
            done(vec![SetTrackEffects { track, effects: old }], cmd.clone(), None, format!("トラック {track} のエフェクトを {n} 個に設定"))
        }
        SetMasterGain { gain_db } => {
            if !gain_db.is_finite() || !(-60.0..=12.0).contains(&gain_db) {
                return err("out_of_range", "gain_db は -60〜12 にしてください");
            }
            let old = std::mem::replace(&mut p.settings.master_gain_db, gain_db);
            done(vec![SetMasterGain { gain_db: old }], cmd.clone(), None, format!("全体の音量を {gain_db:.1}dB に設定"))
        }
        SetSettings { width, height, frame_rate, sample_rate } => {
            let s = &mut p.settings;
            if width.is_some_and(|w| !(16..=8192).contains(&w)) || height.is_some_and(|h| !(16..=8192).contains(&h)) {
                return err("out_of_range", "幅と高さは 16〜8192 にしてください");
            }
            if frame_rate.is_some_and(|r| r.num == 0 || r.den == 0 || r.fps() > 240.0) {
                return err("out_of_range", "フレームレートが不正です");
            }
            if sample_rate.is_some_and(|r| ![44_100, 48_000, 96_000].contains(&r)) {
                return err("out_of_range", "サンプルレートは 44100 / 48000 / 96000 のいずれかにしてください");
            }
            let inv = SetSettings {
                width: width.map(|_| s.width),
                height: height.map(|_| s.height),
                frame_rate: frame_rate.map(|_| s.frame_rate),
                sample_rate: sample_rate.map(|_| s.sample_rate),
            };
            if let Some(v) = width {
                s.width = v;
            }
            if let Some(v) = height {
                s.height = v;
            }
            if let Some(v) = frame_rate {
                s.frame_rate = v;
            }
            if let Some(v) = sample_rate {
                s.sample_rate = v;
            }
            done(vec![inv], cmd.clone(), None, "プロジェクト設定を変更".into())
        }
        RestoreTrack { index, track } => {
            let id = track.id;
            if index > p.tracks.len() {
                return err("internal", "復元位置が不正です");
            }
            p.tracks.insert(index, track);
            done(vec![RemoveTrack { track: id }], cmd.clone(), None, format!("トラック {id} を復元"))
        }
        RestoreClip { track, clip } => {
            let id = clip.id;
            insert_clip(track_mut(p, track)?, clip)?;
            done(vec![RemoveClip { clip: id }], cmd.clone(), None, format!("クリップ {id} を復元"))
        }
        RestoreAsset { asset } => {
            let id = asset.id;
            // 既にあれば置き換え(文字の変更の取り消し)、無ければ追加(削除の取り消し)
            let inverse = match p.assets.insert(id, asset) {
                Some(old) => RestoreAsset { asset: old },
                None => RemoveAsset { asset: id },
            };
            done(vec![inverse], cmd.clone(), None, format!("素材 {id} を復元"))
        }
        RestoreParam { clip, param, state } => {
            let (t, ci) = clip_mut(p, clip)?;
            let slot = t.clips[ci].params.get_mut(&param).ok_or(EditError { code: "internal", message: param.clone() })?;
            let old = std::mem::replace(slot, state);
            done(vec![RestoreParam { clip, param: param.clone(), state: old }], cmd.clone(), None, format!("クリップ {clip} の {param} を復元"))
        }
    }
}
