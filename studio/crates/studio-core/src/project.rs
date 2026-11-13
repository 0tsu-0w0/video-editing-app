//! プロジェクトの構造(素材・トラック・クリップ・キーフレーム付きの値)。

use crate::time::{Rate, Time};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PROJECT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(pub u64);

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rate,
    pub sample_rate: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { width: 1920, height: 1080, frame_rate: Rate::new(30, 1), sample_rate: 48_000 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssetKind {
    /// 動画(音声付きのこともある)
    Video { width: u32, height: u32, frame_rate: Rate, duration: Time, has_audio: bool },
    /// 音声のみ
    Audio { duration: Time, sample_rate: u32, channels: u32 },
    /// 静止画(長さの制限なし)
    Image { width: u32, height: u32 },
    /// 文字(長さの制限なし)。ファイルは持たない
    Text {
        text: String,
        #[serde(default)]
        style: TextStyle,
    },
}

/// 文字の見た目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextStyle {
    /// フォント(Windows のフォントファイル名。例: "meiryo", "YuGothB", "msgothic")
    pub font: String,
    /// 文字の大きさ(ピクセル)
    pub size: f32,
    /// 文字の色 "#RRGGBB" または "#RRGGBBAA"
    pub color: String,
    /// 縁取りの太さ(ピクセル、0 で無し)
    pub outline: f32,
    pub outline_color: String,
    /// 行の揃え: "left" / "center" / "right"
    pub align: String,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self { font: "meiryo".into(), size: 72.0, color: "#FFFFFF".into(), outline: 0.0, outline_color: "#000000".into(), align: "center".into() }
    }
}

/// "#RRGGBB" / "#RRGGBBAA" を RGBA に。
pub fn parse_rgba(s: &str) -> Option<[u8; 4]> {
    let h = s.strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([b(0)?, b(2)?, b(4)?, if h.len() == 8 { b(6)? } else { 255 }])
}

impl TextStyle {
    pub fn check(&self) -> Result<(), String> {
        if !(4.0..=1000.0).contains(&self.size) {
            return Err("文字の大きさは 4〜1000 にしてください".into());
        }
        if !(0.0..=100.0).contains(&self.outline) {
            return Err("縁取りの太さは 0〜100 にしてください".into());
        }
        if parse_rgba(&self.color).is_none() || parse_rgba(&self.outline_color).is_none() {
            return Err("色は \"#RRGGBB\" か \"#RRGGBBAA\" で指定してください".into());
        }
        if !["left", "center", "right"].contains(&self.align.as_str()) {
            return Err("align は left / center / right のいずれかにしてください".into());
        }
        if self.font.is_empty() || self.font.len() > 64 || !self.font.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("font はフォントファイル名(英数字。例: meiryo)で指定してください".into());
        }
        Ok(())
    }
}

impl AssetKind {
    /// 素材の長さ(静止画は None = 無制限)。
    pub fn duration(&self) -> Option<Time> {
        match self {
            AssetKind::Video { duration, .. } | AssetKind::Audio { duration, .. } => Some(*duration),
            AssetKind::Image { .. } | AssetKind::Text { .. } => None,
        }
    }
    pub fn has_video(&self) -> bool {
        !matches!(self, AssetKind::Audio { .. })
    }
    pub fn has_audio(&self) -> bool {
        matches!(self, AssetKind::Audio { .. } | AssetKind::Video { has_audio: true, .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: Id,
    pub name: String,
    pub path: String,
    #[serde(flatten)]
    pub kind: AssetKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: Id,
    pub kind: TrackKind,
    pub name: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub locked: bool,
    /// 開始時刻の順に並ぶ。重なりは無い。
    #[serde(default)]
    pub clips: Vec<Clip>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    #[default]
    Linear,
    Step,
    EaseIn,
    EaseOut,
    EaseInOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Key {
    /// クリップの開始からの時刻
    pub time: Time,
    pub value: f64,
    /// このキーから次のキーまでの補間
    #[serde(default)]
    pub interp: Interp,
}

/// キーフレームを持てる数値。キーが無ければ `value` の定数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub value: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Key>,
}

impl Param {
    pub fn constant(v: f64) -> Param {
        Param { value: v, keys: vec![] }
    }

    /// クリップ内の時刻 t での値。
    pub fn eval(&self, t: Time) -> f64 {
        let k = &self.keys;
        if k.is_empty() {
            return self.value;
        }
        if t <= k[0].time {
            return k[0].value;
        }
        let last = k.len() - 1;
        if t >= k[last].time {
            return k[last].value;
        }
        let i = k.partition_point(|x| x.time <= t) - 1;
        let (a, b) = (&k[i], &k[i + 1]);
        let f = (t - a.time).0 as f64 / (b.time - a.time).0 as f64;
        let f = match a.interp {
            Interp::Linear => f,
            Interp::Step => 0.0,
            Interp::EaseIn => f * f,
            Interp::EaseOut => 1.0 - (1.0 - f) * (1.0 - f),
            Interp::EaseInOut => f * f * (3.0 - 2.0 * f),
        };
        a.value + (b.value - a.value) * f
    }

    /// 時刻 t にキーを置く(同じ時刻のキーは置き換え)。
    pub fn set_key(&mut self, key: Key) {
        match self.keys.binary_search_by(|k| k.time.cmp(&key.time)) {
            Ok(i) => self.keys[i] = key,
            Err(i) => self.keys.insert(i, key),
        }
    }
}

/// クリップの数値項目。AI やコマンドからは文字列の名前で指定する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipParams {
    /// 画面中央からの位置(ピクセル)
    pub x: Param,
    pub y: Param,
    /// 拡大率(1.0 = 等倍)
    pub scale: Param,
    /// 回転(度、時計回り)
    pub rotation: Param,
    /// 不透明度(0〜1)
    pub opacity: Param,
    /// 音量(1.0 = 元の音量)
    pub volume: Param,
    /// 左右の定位(-1 = 左、0 = 中央、1 = 右)
    pub pan: Param,
}

impl Default for ClipParams {
    fn default() -> Self {
        Self {
            x: Param::constant(0.0),
            y: Param::constant(0.0),
            scale: Param::constant(1.0),
            rotation: Param::constant(0.0),
            opacity: Param::constant(1.0),
            volume: Param::constant(1.0),
            pan: Param::constant(0.0),
        }
    }
}

/// 項目名と、値の範囲。
pub const PARAMS: [(&str, f64, f64); 7] = [
    ("x", -100_000.0, 100_000.0),
    ("y", -100_000.0, 100_000.0),
    ("scale", 0.0, 100.0),
    ("rotation", -36_000.0, 36_000.0),
    ("opacity", 0.0, 1.0),
    ("volume", 0.0, 16.0),
    ("pan", -1.0, 1.0),
];

impl ClipParams {
    pub fn get(&self, name: &str) -> Option<&Param> {
        Some(match name {
            "x" => &self.x,
            "y" => &self.y,
            "scale" => &self.scale,
            "rotation" => &self.rotation,
            "opacity" => &self.opacity,
            "volume" => &self.volume,
            "pan" => &self.pan,
            _ => return None,
        })
    }
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Param> {
        Some(match name {
            "x" => &mut self.x,
            "y" => &mut self.y,
            "scale" => &mut self.scale,
            "rotation" => &mut self.rotation,
            "opacity" => &mut self.opacity,
            "volume" => &mut self.volume,
            "pan" => &mut self.pan,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: Id,
    pub asset: Id,
    /// タイムライン上の開始時刻
    pub start: Time,
    /// タイムライン上の長さ
    pub duration: Time,
    /// 素材のどこから使うか
    #[serde(default)]
    pub source_in: Time,
    #[serde(default)]
    pub params: ClipParams,
    /// フェードイン・フェードアウトの長さ(映像は不透明度、音声は音量に効く)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_in: Time,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_out: Time,
}

fn is_zero(t: &Time) -> bool {
    t.0 == 0
}

impl Clip {
    /// クリップ内の時刻 local でのフェードの倍率(0〜1)。
    pub fn fade_gain(&self, local: Time) -> f64 {
        let mut g = 1.0f64;
        if self.fade_in.0 > 0 && local < self.fade_in {
            g *= (local.0.max(0) as f64 / self.fade_in.0 as f64).clamp(0.0, 1.0);
        }
        let rest = self.duration - local;
        if self.fade_out.0 > 0 && rest < self.fade_out {
            g *= (rest.0.max(0) as f64 / self.fade_out.0 as f64).clamp(0.0, 1.0);
        }
        g
    }

    pub fn end(&self) -> Time {
        self.start + self.duration
    }
    pub fn contains(&self, t: Time) -> bool {
        t >= self.start && t < self.end()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub settings: Settings,
    #[serde(default)]
    pub assets: BTreeMap<Id, Asset>,
    /// 先頭が最も奥(下)のトラック。映像は後ろのトラックほど手前に重なる。
    #[serde(default)]
    pub tracks: Vec<Track>,
    pub next_id: u64,
}

impl Default for Project {
    fn default() -> Self {
        Self { version: PROJECT_VERSION, settings: Settings::default(), assets: BTreeMap::new(), tracks: vec![], next_id: 1 }
    }
}

impl Project {
    pub fn alloc_id(&mut self) -> Id {
        let id = Id(self.next_id);
        self.next_id += 1;
        id
    }
    pub fn track(&self, id: Id) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }
    pub fn track_index(&self, id: Id) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }
    /// クリップの (トラックの添字, トラック内の添字)
    pub fn find_clip(&self, id: Id) -> Option<(usize, usize)> {
        for (ti, t) in self.tracks.iter().enumerate() {
            if let Some(ci) = t.clips.iter().position(|c| c.id == id) {
                return Some((ti, ci));
            }
        }
        None
    }
    pub fn clip(&self, id: Id) -> Option<&Clip> {
        self.find_clip(id).map(|(t, c)| &self.tracks[t].clips[c])
    }
    /// 全体の長さ(最後のクリップの終わり)。
    pub fn duration(&self) -> Time {
        self.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.end())).max().unwrap_or(Time::ZERO)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("serialize")
    }
    pub fn from_json(s: &str) -> Result<Project, String> {
        let p: Project = serde_json::from_str(s).map_err(|e| format!("プロジェクトを読めません: {e}"))?;
        if p.version > PROJECT_VERSION {
            return Err(format!("新しい版({})のプロジェクトです。アプリを更新してください", p.version));
        }
        p.check().map_err(|e| format!("プロジェクトが壊れています: {e}"))?;
        Ok(p)
    }

    /// 内部の整合性(重なり・並び順・参照・ID の重複)を確かめる。
    pub fn check(&self) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        for (k, a) in &self.assets {
            if *k != a.id || !ids.insert(a.id) || a.id.0 >= self.next_id {
                return Err(format!("素材 {} の ID が不正です", a.id));
            }
        }
        for t in &self.tracks {
            if !ids.insert(t.id) || t.id.0 >= self.next_id {
                return Err(format!("トラック {} の ID が不正です", t.id));
            }
            for w in t.clips.windows(2) {
                if w[0].end() > w[1].start {
                    return Err(format!("トラック {} でクリップ {} と {} が重なっています", t.id, w[0].id, w[1].id));
                }
            }
            for c in &t.clips {
                if !ids.insert(c.id) || c.id.0 >= self.next_id {
                    return Err(format!("クリップ {} の ID が不正です", c.id));
                }
                let a = self.assets.get(&c.asset).ok_or(format!("クリップ {} の素材がありません", c.id))?;
                if c.fade_in < Time::ZERO || c.fade_out < Time::ZERO || c.fade_in + c.fade_out > c.duration {
                    return Err(format!("クリップ {} のフェードが長さを超えています", c.id));
                }
                if c.duration <= Time::ZERO || c.start < Time::ZERO || c.source_in < Time::ZERO {
                    return Err(format!("クリップ {} の時間が不正です", c.id));
                }
                if let Some(d) = a.kind.duration() {
                    if c.source_in + c.duration > d {
                        return Err(format!("クリップ {} が素材の長さを超えています", c.id));
                    }
                }
                let ok = match t.kind {
                    TrackKind::Video => a.kind.has_video(),
                    TrackKind::Audio => a.kind.has_audio(),
                };
                if !ok {
                    return Err(format!("クリップ {} の素材はトラックの種類と合いません", c.id));
                }
            }
        }
        Ok(())
    }
}
