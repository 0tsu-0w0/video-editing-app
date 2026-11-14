//! studio-render: タイムラインの映像を合成し、音声をミックスし、ファイルへ書き出す。
//!
//! - `Renderer`: 時刻 t の映像を GPU で合成する(素材の読み込みとキャッシュも行う)
//! - `Mixer`: 区間の音声をミックスする(音量・パンのキーフレームを反映)
//! - `export`: プロジェクト全体を MP4 に書き出す

pub mod audio_fx;
pub mod gpu;
pub mod model;
pub mod text;

use gpu::{Gpu, Placement, Texture};
use std::collections::HashMap;
use std::path::Path;
use studio_core::eval::{layers_at, track_audio_spans};
use studio_core::time::{sample_to_time, time_to_sample};
use studio_core::{AssetKind, Id, Project, Time, TrackKind};
use studio_media::{AudioReader, ExportSettings, Exporter, VideoReader};

/// 素材ごとの GPU テクスチャと、映像の読み取り。
enum Source {
    Video { reader: VideoReader, tex: Option<Texture>, shown: Option<Time> },
    Image { tex: Texture },
    /// 文字。内容が変わったら作り直す
    Text { tex: Texture, made_from: AssetKind },
    /// 3D モデル(クリップごとに描くので、素材ごとに 1 つ持つ)
    Model(Box<model::Model>),
    Broken(String),
}

pub struct Renderer {
    pub gpu: Gpu,
    sources: HashMap<(Id, u8), Source>,
    target: Option<(Texture, windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView)>,
    /// 読み込めなかった素材(画面に出すための一覧)
    pub errors: Vec<String>,
    model_pipe: Option<model::ModelPipeline>,
    /// プレビュー用: 動画にプロキシがあればそれを使う(書き出しでは使わない)
    use_proxies: bool,
}

impl Renderer {
    pub fn new(gpu: Gpu) -> Self {
        Self { gpu, sources: HashMap::new(), target: None, errors: vec![], model_pipe: None, use_proxies: false }
    }

    fn source(&mut self, project: &Project, asset: Id, slot: u8) -> &mut Source {
        let key = (asset, slot);
        // 文字の内容が変わっていたら作り直す
        if let (Some(Source::Text { made_from, .. }), Some(a)) = (self.sources.get(&key), project.assets.get(&asset)) {
            if *made_from != a.kind {
                self.sources.remove(&key);
            }
        }
        if !self.sources.contains_key(&key) {
            let s = match project.assets.get(&asset) {
                None => Source::Broken(format!("素材 {asset} がありません")),
                Some(a) => {
                    let path = Path::new(&a.path);
                    match &a.kind {
                        AssetKind::Video { .. } => match VideoReader::open(&if self.use_proxies { studio_media::proxy::existing_proxy(path).unwrap_or(path.to_path_buf()) } else { path.to_path_buf() }) {
                            Ok(reader) => Source::Video { reader, tex: None, shown: None },
                            Err(e) => Source::Broken(format!("「{}」: {e}", a.name)),
                        },
                        AssetKind::Image { .. } => match studio_media::load_image(path)
                            .map_err(|e| e.to_string())
                            .and_then(|f| self.gpu.create_texture(f.width, f.height, Some(&f.bgra), false))
                        {
                            Ok(tex) => Source::Image { tex },
                            Err(e) => Source::Broken(format!("「{}」: {e}", a.name)),
                        },
                        AssetKind::Audio { .. } => Source::Broken("音声のみの素材".into()),
                        AssetKind::Model { .. } => match model::Model::load(&self.gpu, path) {
                            Ok(m) => Source::Model(Box::new(m)),
                            Err(e) => Source::Broken(format!("「{}」: {e}", a.name)),
                        },
                        AssetKind::Text { text, style } => match text::rasterize(text, style).and_then(|(w, h, px)| self.gpu.create_texture(w, h, Some(&px), false)) {
                            Ok(tex) => Source::Text { tex, made_from: a.kind.clone() },
                            Err(e) => Source::Broken(format!("「{}」: {e}", a.name)),
                        },
                    }
                }
            };
            if let Source::Broken(m) = &s {
                if slot == 0 {
                    self.errors.push(m.clone());
                }
            }
            self.sources.insert(key, s);
        }
        self.sources.get_mut(&key).unwrap()
    }

    /// プロキシを使うかを切り替える(開いている動画は開き直す)。
    pub fn set_use_proxies(&mut self, on: bool) {
        if self.use_proxies != on {
            self.use_proxies = on;
            self.sources.retain(|_, s| !matches!(s, Source::Video { .. }));
        }
    }

    pub fn use_proxies(&self) -> bool {
        self.use_proxies
    }

    /// 素材の差し替えなどで、キャッシュを捨てる。
    pub fn forget(&mut self, asset: Id) {
        self.sources.retain(|k, _| k.0 != asset);
    }

    /// 時刻 t の映像を合成し、そのテクスチャを返す(プロジェクトの大きさ、背景は黒)。
    pub fn render(&mut self, project: &Project, t: Time) -> Result<&Texture, String> {
        let (w, h) = (project.settings.width, project.settings.height);
        if self.target.as_ref().map_or(true, |(tex, _)| (tex.width, tex.height) != (w, h)) {
            let tex = self.gpu.create_texture(w, h, None, true)?;
            let rtv = self.gpu.rtv(&tex)?;
            self.target = Some((tex, rtv));
        }
        let rtv = self.target.as_ref().unwrap().1.clone();
        self.gpu.clear(&rtv, [0.0, 0.0, 0.0, 1.0]);
        for layer in layers_at(project, t) {
            // 素材のテクスチャを用意(動画は表示するフレームが変わったときだけ更新)
            if self.model_pipe.is_none() && matches!(project.assets.get(&layer.asset).map(|a| &a.kind), Some(AssetKind::Model { .. })) {
                match model::ModelPipeline::new(&self.gpu) {
                    Ok(p) => self.model_pipe = Some(p),
                    Err(e) => {
                        self.errors.push(format!("3D の描画を準備できません: {e}"));
                        continue;
                    }
                }
            }
            let pipe = self.model_pipe.as_ref().map(|p| p as *const model::ModelPipeline);
            let gpu = &self.gpu as *const Gpu;
            let src = self.source(project, layer.asset, layer.slot);
            let gpu = unsafe { &*gpu };
            let tex: &Texture = match src {
                Source::Video { reader, tex, shown } => {
                    let f = match reader.frame_at(layer.source_time) {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("フレームを読めません: {e}");
                            continue;
                        }
                    };
                    if tex.as_ref().map_or(true, |t| (t.width, t.height) != (f.width, f.height)) {
                        *tex = Some(gpu.create_texture(f.width, f.height, Some(&f.bgra), false)?);
                    } else if *shown != Some(f.time) {
                        gpu.update_texture(tex.as_ref().unwrap(), &f.bgra);
                    }
                    *shown = Some(f.time);
                    tex.as_ref().unwrap()
                }
                Source::Image { tex } | Source::Text { tex, .. } => tex,
                Source::Model(m) => {
                    let pipe = unsafe { &*pipe.unwrap() };
                    match m.render(gpu, pipe, (w, h), layer.local.secs_f64(), layer.rot_x, layer.rot_y) {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("3D モデルを描けません: {e}");
                            continue;
                        }
                    }
                }
                Source::Broken(_) => continue,
            };
            // プロキシは小さいので、置き方とマスクは元の動画の大きさで計算する
            let (tw, th) = match project.assets.get(&layer.asset).map(|a| &a.kind) {
                Some(AssetKind::Video { width, height, .. }) => (*width, *height),
                _ => (tex.width, tex.height),
            };
            gpu.set_mask(&mask_cb(&layer, tw, th));
            if let Some(c) = &layer.corners {
                gpu.draw_pinned(&rtv, (w, h), tex, c, layer.opacity);
                continue;
            }
            gpu.draw(
                &rtv,
                (w, h),
                tex,
                &Placement {
                    width: tw as f64,
                    height: th as f64,
                    x: layer.x,
                    y: layer.y,
                    scale: layer.scale,
                    rotation: layer.rotation,
                    opacity: layer.opacity,
                },
            );
        }
        Ok(&self.target.as_ref().unwrap().0)
    }

    /// 時刻 t の映像を BGRA で取り出す。
    pub fn render_bgra(&mut self, project: &Project, t: Time) -> Result<Vec<u8>, String> {
        self.render(project, t)?;
        let tex = &self.target.as_ref().unwrap().0;
        self.gpu.read(tex)
    }
}

/// レイヤーのマスクとワイプを、シェーダに渡す形にする。
fn mask_cb(l: &studio_core::eval::Layer, w: u32, h: u32) -> gpu::MaskCb {
    use studio_core::project::{MaskShape, TransitionKind as K};
    let mut m = gpu::MaskCb { size: [w as f32, h as f32, 0.0, 0.0], ..Default::default() };
    if let Some(k) = &l.mask {
        m.rect = [(w as f64 / 2.0 + k.x) as f32, (h as f64 / 2.0 + k.y) as f32, (k.w / 2.0) as f32, (k.h / 2.0) as f32];
        m.opt = [if k.shape == MaskShape::Rect { 1.0 } else { 2.0 }, k.invert as u8 as f32, k.feather as f32, 0.0];
    }
    if let Some((kind, q)) = l.wipe {
        let n = match kind {
            K::WipeRight => 1.0,
            K::WipeLeft => 2.0,
            K::WipeDown => 3.0,
            K::WipeUp => 4.0,
            _ => 5.0,
        };
        m.wipe = [n, q as f32, 0.0, 0.0];
    }
    let f = &l.fx;
    m.color = [f.brightness as f32, f.contrast as f32, f.saturation as f32, f.hue.to_radians() as f32];
    m.key2[3] = f.blur as f32;
    if let Some((c, tol, soft, spill)) = l.key {
        m.key = [c[0] as f32, c[1] as f32, c[2] as f32, 1.0];
        m.key2[0] = tol as f32;
        m.key2[1] = soft as f32;
        m.key2[2] = spill as f32;
    }
    m
}

/// 直近にミックスした区間の音量(ピーク、直線)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Levels {
    /// 区間の始まり(標本)
    pub start: i64,
    /// 音声トラックごとの (ID, 左, 右)。鳴っていないトラックは 0
    pub tracks: Vec<(Id, f32, f32)>,
    /// 全体(音量調整後、クリップ前)
    pub master: (f32, f32),
}

/// 音声のミックス。素材ごとに読み取りを持ち、トラックごとに音を集めてエフェクトをかけ、足し合わせる。
/// エフェクトの状態は続けて呼ばれる間は引き継ぎ、飛んだ(シークした)ら初めからにする。
pub struct Mixer {
    rate: u32,
    readers: HashMap<Id, Option<AudioReader>>,
    fx: HashMap<(Id, usize), audio_fx::FxState>,
    next: Option<i64>,
    pub levels: Levels,
}

fn peak(buf: &[f32]) -> (f32, f32) {
    buf.chunks_exact(2).fold((0.0f32, 0.0f32), |(l, r), f| (l.max(f[0].abs()), r.max(f[1].abs())))
}

impl Mixer {
    pub fn new(rate: u32) -> Self {
        Self { rate, readers: HashMap::new(), fx: HashMap::new(), next: None, levels: Levels::default() }
    }

    /// 標本番号 start から frames 標本分(ステレオ f32)。
    pub fn mix(&mut self, project: &Project, start: i64, frames: usize) -> Vec<f32> {
        if self.next != Some(start) {
            self.fx.clear();
        }
        self.next = Some(start + frames as i64);
        let (from, to) = (sample_to_time(start, self.rate), sample_to_time(start + frames as i64, self.rate));
        // 1. トラックごとに、クリップの音を集める(消音中のトラックも、ダッキングの判定に使うので集める)
        let mut raw: HashMap<Id, Vec<f32>> = HashMap::new();
        for span in track_audio_spans(project, from, to, true) {
            let Some(clip) = project.clip(span.clip) else { continue };
            let reader = self.readers.entry(span.asset).or_insert_with(|| {
                project.assets.get(&span.asset).and_then(|a| AudioReader::open(Path::new(&a.path), self.rate).ok())
            });
            let Some(reader) = reader else { continue };
            let s0 = time_to_sample(span.start, self.rate);
            let s1 = time_to_sample(span.end, self.rate);
            let n = (s1 - s0).max(0) as usize;
            if n == 0 {
                continue;
            }
            let src_start = time_to_sample(span.source_start, self.rate);
            let buf = if span.speed == 1.0 && !span.reverse {
                let Ok(b) = reader.read(src_start, n) else { continue };
                b
            } else {
                // 速度が違うときは、必要な長さを読んで直線補間で詰める・伸ばす(音の高さも変わる)。
                // 逆再生では source_start が範囲の終わりなので、そこから手前の範囲を読んで、最後に前後を入れ替える
                let len = n as f64 * span.speed;
                let hi = span.source_start.secs_f64() * self.rate as f64;
                let pos0 = if span.reverse { (hi - len).max(0.0) } else { hi };
                let base = pos0.floor() as i64;
                let need = ((n as f64) * span.speed).ceil() as usize + 2;
                let Ok(src) = reader.read(base, need) else { continue };
                let frac0 = pos0 - base as f64;
                let at = |i: usize, ch: usize| src.get(i * 2 + ch).copied().unwrap_or(0.0);
                let mut b = vec![0.0f32; n * 2];
                for k in 0..n {
                    let x = frac0 + k as f64 * span.speed;
                    let i = x.floor() as usize;
                    let f = (x - i as f64) as f32;
                    for ch in 0..2 {
                        b[k * 2 + ch] = at(i, ch) * (1.0 - f) + at(i + 1, ch) * f;
                    }
                }
                if span.reverse {
                    let mut r = vec![0.0f32; n * 2];
                    for k in 0..n {
                        r[k * 2..k * 2 + 2].copy_from_slice(&b[(n - 1 - k) * 2..(n - 1 - k) * 2 + 2]);
                    }
                    b = r;
                }
                b
            };
            let off = (s0 - start) as usize;
            let out = raw.entry(span.track).or_insert_with(|| vec![0.0f32; frames * 2]);
            // 音量とパンは 64 標本ごとに評価し、その間は直線で補間
            let step = 64;
            let gains = |i: usize| {
                let t = sample_to_time(s0 + i as i64, self.rate) - span.clip_start;
                let v = (clip.params.volume.eval(t).max(0.0) * clip.fade_gain(t)) as f32;
                let p = clip.params.pan.eval(t).clamp(-1.0, 1.0) as f32;
                // 等パワーのパン
                let a = (p + 1.0) * std::f32::consts::FRAC_PI_4;
                (v * a.cos() * std::f32::consts::SQRT_2, v * a.sin() * std::f32::consts::SQRT_2)
            };
            let mut i = 0;
            while i < n {
                let j = (i + step).min(n);
                let (l0, r0) = gains(i);
                let (l1, r1) = gains(j);
                for k in i..j {
                    let f = (k - i) as f32 / (j - i) as f32;
                    let (gl, gr) = (l0 + (l1 - l0) * f, r0 + (r1 - r0) * f);
                    if let (Some(o), Some(b)) = (out.get_mut((off + k) * 2..(off + k) * 2 + 2), buf.get(k * 2..k * 2 + 2)) {
                        o[0] += b[0] * gl;
                        o[1] += b[1] * gr;
                    }
                }
                i = j;
            }
        }
        // 2. トラックごとにエフェクト・音量・バランスをかけて足す
        let any_solo = project.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.audio.solo);
        let mut out = vec![0.0f32; frames * 2];
        let mut levels = Levels { start, ..Default::default() };
        let rate = self.rate as f32;
        for t in project.tracks.iter().filter(|t| t.kind == TrackKind::Audio) {
            let audible = !t.muted && (!any_solo || t.audio.solo);
            let Some(mut b) = raw.get(&t.id).cloned() else {
                levels.tracks.push((t.id, 0.0, 0.0));
                continue;
            };
            for (i, e) in t.audio.effects.iter().enumerate() {
                let st = self.fx.entry((t.id, i)).or_insert_with(|| audio_fx::FxState::new(e));
                if !st.matches(e) {
                    *st = audio_fx::FxState::new(e);
                }
                let side = match e {
                    studio_core::project::AudioEffect::Duck { source, .. } => Some(raw.get(source).map(|v| v.as_slice()).unwrap_or(&[])),
                    _ => None,
                };
                // 相手が無音の区間も、下げ幅を戻すために無音として処理する
                let silent;
                let side = match side {
                    Some(s) if s.is_empty() => {
                        silent = vec![0.0f32; frames * 2];
                        Some(silent.as_slice())
                    }
                    other => other,
                };
                st.process(e, &mut b, side, rate);
            }
            let g = audio_fx::db_to_gain(t.audio.gain_db);
            let p = t.audio.pan.clamp(-1.0, 1.0) as f32;
            let (gl, gr) = (g * (1.0 - p).min(1.0), g * (1.0 + p).min(1.0));
            for f in b.chunks_exact_mut(2) {
                f[0] *= gl;
                f[1] *= gr;
            }
            let (pl, pr) = if audible { peak(&b) } else { (0.0, 0.0) };
            levels.tracks.push((t.id, pl, pr));
            if audible {
                for (o, v) in out.iter_mut().zip(&b) {
                    *o += v;
                }
            }
        }
        let mg = audio_fx::db_to_gain(project.settings.master_gain_db);
        for v in &mut out {
            *v *= mg;
        }
        levels.master = peak(&out);
        for v in &mut out {
            *v = v.clamp(-1.0, 1.0);
        }
        self.levels = levels;
        out
    }
}

pub struct Progress {
    pub frame: i64,
    pub total: i64,
}

/// プロジェクトを MP4(H.264 + AAC)に書き出す。`progress` が false を返したら中止する。
pub fn export(project: &Project, path: &Path, mut progress: impl FnMut(Progress) -> bool) -> Result<(), String> {
    let s = &project.settings;
    let total = s.frame_rate.time_to_frame(project.duration() + Time(s.frame_rate.frame_duration().0 - 1));
    if total <= 0 {
        return Err("書き出す内容がありません".into());
    }
    let mut renderer = Renderer::new(Gpu::new(None)?);
    let mut mixer = Mixer::new(s.sample_rate);
    let bitrate = ((s.width as u64 * s.height as u64 * 6) as f64 * s.frame_rate.fps() / 30.0).clamp(2e6, 60e6) as u32;
    let mut ex = Exporter::create(
        path,
        &ExportSettings { width: s.width, height: s.height, frame_rate: s.frame_rate, video_bitrate: bitrate, sample_rate: Some(s.sample_rate), audio_bitrate: 192_000 },
    )
    .map_err(|e| e.to_string())?;
    for n in 0..total {
        let t = s.frame_rate.frame_to_time(n);
        let bgra = renderer.render_bgra(project, t)?;
        ex.write_frame(&bgra).map_err(|e| e.to_string())?;
        // このフレームの区間に入る標本(端数はフレームの境界で正確に分ける)
        let a = time_to_sample(t, s.sample_rate);
        let b = time_to_sample(s.frame_rate.frame_to_time(n + 1), s.sample_rate);
        ex.write_audio(&mixer.mix(project, a, (b - a) as usize)).map_err(|e| e.to_string())?;
        if !progress(Progress { frame: n + 1, total }) {
            return Err("中止しました".into());
        }
    }
    ex.finish().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
