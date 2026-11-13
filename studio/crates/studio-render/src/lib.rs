//! studio-render: タイムラインの映像を合成し、音声をミックスし、ファイルへ書き出す。
//!
//! - `Renderer`: 時刻 t の映像を GPU で合成する(素材の読み込みとキャッシュも行う)
//! - `Mixer`: 区間の音声をミックスする(音量・パンのキーフレームを反映)
//! - `export`: プロジェクト全体を MP4 に書き出す

pub mod gpu;

use gpu::{Gpu, Placement, Texture};
use std::collections::HashMap;
use std::path::Path;
use studio_core::eval::{audio_spans, layers_at};
use studio_core::time::{sample_to_time, time_to_sample};
use studio_core::{AssetKind, Id, Project, Time};
use studio_media::{AudioReader, ExportSettings, Exporter, VideoReader};

/// 素材ごとの GPU テクスチャと、映像の読み取り。
enum Source {
    Video { reader: VideoReader, tex: Option<Texture>, shown: Option<Time> },
    Image { tex: Texture },
    Broken(String),
}

pub struct Renderer {
    pub gpu: Gpu,
    sources: HashMap<Id, Source>,
    target: Option<(Texture, windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView)>,
    /// 読み込めなかった素材(画面に出すための一覧)
    pub errors: Vec<String>,
}

impl Renderer {
    pub fn new(gpu: Gpu) -> Self {
        Self { gpu, sources: HashMap::new(), target: None, errors: vec![] }
    }

    fn source(&mut self, project: &Project, asset: Id) -> &mut Source {
        if !self.sources.contains_key(&asset) {
            let s = match project.assets.get(&asset) {
                None => Source::Broken(format!("素材 {asset} がありません")),
                Some(a) => {
                    let path = Path::new(&a.path);
                    match &a.kind {
                        AssetKind::Video { .. } => match VideoReader::open(path) {
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
                    }
                }
            };
            if let Source::Broken(m) = &s {
                self.errors.push(m.clone());
            }
            self.sources.insert(asset, s);
        }
        self.sources.get_mut(&asset).unwrap()
    }

    /// 素材の差し替えなどで、キャッシュを捨てる。
    pub fn forget(&mut self, asset: Id) {
        self.sources.remove(&asset);
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
            let gpu = &self.gpu as *const Gpu;
            let src = self.source(project, layer.asset);
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
                Source::Image { tex } => tex,
                Source::Broken(_) => continue,
            };
            gpu.draw(
                &rtv,
                (w, h),
                tex,
                &Placement {
                    width: tex.width as f64,
                    height: tex.height as f64,
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

/// 音声のミックス。素材ごとに読み取りを持ち、区間の音を足し合わせる。
pub struct Mixer {
    rate: u32,
    readers: HashMap<Id, Option<AudioReader>>,
}

impl Mixer {
    pub fn new(rate: u32) -> Self {
        Self { rate, readers: HashMap::new() }
    }

    /// 標本番号 start から frames 標本分(ステレオ f32)。
    pub fn mix(&mut self, project: &Project, start: i64, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        let (from, to) = (sample_to_time(start, self.rate), sample_to_time(start + frames as i64, self.rate));
        for span in audio_spans(project, from, to) {
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
            let Ok(buf) = reader.read(src_start, n) else { continue };
            let off = (s0 - start) as usize;
            // 音量とパンは 64 標本ごとに評価し、その間は直線で補間
            let step = 64;
            let gains = |i: usize| {
                let t = sample_to_time(s0 + i as i64, self.rate) - span.clip_start;
                let v = clip.params.volume.eval(t).max(0.0) as f32;
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
                    out[(off + k) * 2] += buf[k * 2] * gl;
                    out[(off + k) * 2 + 1] += buf[k * 2 + 1] * gr;
                }
                i = j;
            }
        }
        for v in &mut out {
            *v = v.clamp(-1.0, 1.0);
        }
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
