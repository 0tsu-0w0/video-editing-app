//! studio-media: 動画・音声・画像の読み込みと、動画の書き出し(Windows の Media Foundation)。
//!
//! - `probe`: 素材の情報(大きさ・フレームレート・長さ・音声の有無)
//! - `VideoReader`: 指定時刻のフレームを BGRA で取り出す(連続再生は前へ読み進め、離れた位置はシーク)
//! - `AudioReader`: 指定時刻から、48kHz などのステレオ f32 で音声を取り出す
//! - `Exporter`: H.264 + AAC の MP4 を書き出す(ハードウェアエンコーダを優先)
//!
//! Phase 1 では、扱いやすさを優先して CPU のメモリで受け渡す。GPU 上での受け渡しは後で追加する。

use std::path::Path;
use std::sync::OnceLock;
use studio_core::{AssetKind, Rate, Time};
use windows::core::{GUID, PCWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("ファイルを開けません: {0}")]
    Open(String),
    #[error("この形式には対応していません: {0}")]
    Unsupported(String),
    #[error("読み込みに失敗しました: {0}")]
    Read(String),
    #[error("書き出しに失敗しました: {0}")]
    Write(String),
}

type R<T> = Result<T, MediaError>;

fn win<E: std::fmt::Display>(f: fn(String) -> MediaError) -> impl Fn(E) -> MediaError {
    move |e| f(e.to_string())
}

/// Media Foundation を(スレッドごと・プロセスごとに)初期化する。
pub fn init() {
    static MF: OnceLock<()> = OnceLock::new();
    unsafe {
        // スレッドごとに必要。既に初期化済みなら失敗するが問題ない。
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    MF.get_or_init(|| unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_FULL).expect("Media Foundation を初期化できません");
    });
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const FIRST_AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
const MEDIA_SOURCE: u32 = MF_SOURCE_READER_MEDIASOURCE.0 as u32;
const END_OF_STREAM: u32 = MF_SOURCE_READERF_ENDOFSTREAM.0 as u32;

const IMAGE_EXT: [&str; 6] = ["png", "jpg", "jpeg", "bmp", "gif", "webp"];

fn is_image(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

fn open_reader(path: &Path, video_processing: bool) -> R<IMFSourceReader> {
    init();
    let p = wide(&path.to_string_lossy());
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2).map_err(win(MediaError::Open))?;
        let attrs = attrs.unwrap();
        if video_processing {
            // 色変換(NV12 → BGRA)を Media Foundation に任せる
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1).map_err(win(MediaError::Open))?;
        }
        MFCreateSourceReaderFromURL(PCWSTR(p.as_ptr()), &attrs).map_err(|e| MediaError::Open(format!("{}: {e}", path.display())))
    }
}

fn duration_of(r: &IMFSourceReader) -> Option<Time> {
    unsafe {
        let pv: PROPVARIANT = r.GetPresentationAttribute(MEDIA_SOURCE, &MF_PD_DURATION).ok()?;
        let hns = u64::try_from(&pv).ok()?;
        Some(Time::from_hns(hns as i64))
    }
}

/// 素材の種類と情報を調べる。
pub fn probe(path: &Path) -> R<AssetKind> {
    if is_image(path) {
        let (w, h) = image::image_dimensions(path).map_err(|e| MediaError::Open(format!("{}: {e}", path.display())))?;
        return Ok(AssetKind::Image { width: w, height: h });
    }
    let r = open_reader(path, false)?;
    let duration = duration_of(&r).ok_or_else(|| MediaError::Unsupported(format!("{}: 長さが分かりません", path.display())))?;
    unsafe {
        let video = r.GetNativeMediaType(FIRST_VIDEO, 0).ok();
        let audio = r.GetNativeMediaType(FIRST_AUDIO, 0).ok();
        match (video, audio) {
            (Some(v), a) => {
                let size = v.GetUINT64(&MF_MT_FRAME_SIZE).map_err(win(MediaError::Unsupported))?;
                let rate = v.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(pack(30, 1));
                let (num, den) = ((rate >> 32) as u32, rate as u32);
                Ok(AssetKind::Video {
                    width: (size >> 32) as u32,
                    height: size as u32,
                    frame_rate: if num > 0 && den > 0 { Rate::new(num, den) } else { Rate::new(30, 1) },
                    duration,
                    has_audio: a.is_some(),
                })
            }
            (None, Some(a)) => Ok(AssetKind::Audio {
                duration,
                sample_rate: a.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48_000),
                channels: a.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2),
            }),
            (None, None) => Err(MediaError::Unsupported(format!("{}: 映像も音声もありません", path.display()))),
        }
    }
}

/// BGRA(ストレートアルファ、8bit)の画像。
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    /// このフレームの素材内の時刻と長さ
    pub time: Time,
    pub duration: Time,
}

/// 画像ファイルを BGRA で読む。
pub fn load_image(path: &Path) -> R<Frame> {
    let img = image::open(path).map_err(|e| MediaError::Read(format!("{}: {e}", path.display())))?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut bgra = img.into_raw();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok(Frame { width: w, height: h, bgra, time: Time::ZERO, duration: Time(i64::MAX / 4) })
}

fn seek(r: &IMFSourceReader, t: Time) -> R<()> {
    let pv = PROPVARIANT::from(t.hns().max(0));
    unsafe { r.SetCurrentPosition(&GUID::zeroed(), &pv) }.map_err(win(MediaError::Read))
}

/// 動画のフレームを時刻指定で取り出す。
pub struct VideoReader {
    reader: IMFSourceReader,
    width: u32,
    height: u32,
    stride: i32,
    frame_dur: Time,
    current: Option<Frame>,
    /// 次に読むサンプルの時刻の目安(前へ読み進めるか、シークするかの判断用)
    cursor: Time,
    eof: bool,
    pub duration: Time,
}

impl VideoReader {
    pub fn open(path: &Path) -> R<Self> {
        let reader = open_reader(path, true)?;
        let duration = duration_of(&reader).unwrap_or(Time::ZERO);
        unsafe {
            let t = MFCreateMediaType().map_err(win(MediaError::Open))?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(win(MediaError::Open))?;
            t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(win(MediaError::Open))?;
            reader.SetCurrentMediaType(FIRST_VIDEO, None, &t).map_err(|e| MediaError::Unsupported(format!("映像を変換できません: {e}")))?;
            reader.SetStreamSelection(FIRST_AUDIO, false).ok();
            let cur = reader.GetCurrentMediaType(FIRST_VIDEO).map_err(win(MediaError::Open))?;
            let size = cur.GetUINT64(&MF_MT_FRAME_SIZE).map_err(win(MediaError::Open))?;
            let (w, h) = ((size >> 32) as u32, size as u32);
            let stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or((w * 4) as i32);
            let rate = cur.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(pack(30, 1));
            let rate = Rate::new(((rate >> 32) as u32).max(1), (rate as u32).max(1));
            Ok(Self { reader, width: w, height: h, stride, frame_dur: rate.frame_duration(), current: None, cursor: Time::ZERO, eof: false, duration })
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn read_next(&mut self) -> R<Option<Frame>> {
        unsafe {
            loop {
                let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
                self.reader
                    .ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))
                    .map_err(win(MediaError::Read))?;
                if flags & END_OF_STREAM != 0 {
                    self.eof = true;
                    return Ok(None);
                }
                let Some(s) = sample else { continue };
                let dur = s.GetSampleDuration().map(Time::from_hns).unwrap_or(self.frame_dur);
                let buf = s.ConvertToContiguousBuffer().map_err(win(MediaError::Read))?;
                let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
                buf.Lock(&mut p, None, Some(&mut len)).map_err(win(MediaError::Read))?;
                let src = std::slice::from_raw_parts(p, len as usize);
                let (w, h) = (self.width as usize, self.height as usize);
                let abs = self.stride.unsigned_abs() as usize;
                let mut bgra = vec![0u8; w * h * 4];
                for y in 0..h {
                    let sy = if self.stride >= 0 { y } else { h - 1 - y };
                    let row = &src[sy * abs..sy * abs + w * 4];
                    let dst = &mut bgra[y * w * 4..(y + 1) * w * 4];
                    dst.copy_from_slice(row);
                    for a in dst.iter_mut().skip(3).step_by(4) {
                        *a = 255; // RGB32 の 4 バイト目は未定義なので不透明にする
                    }
                }
                buf.Unlock().ok();
                let t = Time::from_hns(ts);
                self.cursor = t + dur;
                return Ok(Some(Frame { width: self.width, height: self.height, bgra, time: t, duration: dur }));
            }
        }
    }

    /// 素材内の時刻 t に表示すべきフレーム。
    pub fn frame_at(&mut self, t: Time) -> R<&Frame> {
        init();
        let t = t.max(Time::ZERO);
        if let Some(f) = &self.current {
            if t >= f.time && t < f.time + f.duration {
                return Ok(self.current.as_ref().unwrap());
            }
        }
        // 少し先なら読み進める。後ろや遠い先ならシークする。
        let near_ahead = self.current.as_ref().is_some_and(|f| t >= f.time) && t < self.cursor + Time::from_secs_f64(2.0);
        if !near_ahead || self.eof {
            seek(&self.reader, t)?;
            self.eof = false;
            self.current = None;
        }
        loop {
            match self.read_next()? {
                Some(f) => {
                    let hit = t < f.time + f.duration;
                    self.current = Some(f);
                    if hit {
                        break;
                    }
                }
                None => {
                    if self.current.is_none() {
                        return Err(MediaError::Read(format!("時刻 {t} のフレームがありません")));
                    }
                    break; // 終わりを越えたら最後のフレームを出す
                }
            }
        }
        Ok(self.current.as_ref().unwrap())
    }
}

/// 音声を、指定のサンプルレートのステレオ f32 で取り出す。
pub struct AudioReader {
    reader: IMFSourceReader,
    rate: u32,
    /// 読み出し済みで、まだ渡していない標本(ステレオで交互)
    pending: Vec<f32>,
    /// pending[0] の時刻(標本番号)
    pending_start: i64,
    eof: bool,
}

impl AudioReader {
    pub fn open(path: &Path, rate: u32) -> R<Self> {
        let reader = open_reader(path, false)?;
        unsafe {
            reader.SetStreamSelection(FIRST_VIDEO, false).ok();
            reader.SetStreamSelection(FIRST_AUDIO, true).map_err(|_| MediaError::Unsupported("音声がありません".into()))?;
            let t = MFCreateMediaType().map_err(win(MediaError::Open))?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(win(MediaError::Open))?;
            t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).map_err(win(MediaError::Open))?;
            t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).map_err(win(MediaError::Open))?;
            t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate).map_err(win(MediaError::Open))?;
            t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32).map_err(win(MediaError::Open))?;
            t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 8).map_err(win(MediaError::Open))?;
            t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * 8).map_err(win(MediaError::Open))?;
            reader.SetCurrentMediaType(FIRST_AUDIO, None, &t).map_err(|e| MediaError::Unsupported(format!("音声を変換できません: {e}")))?;
        }
        Ok(Self { reader, rate, pending: vec![], pending_start: 0, eof: false })
    }

    fn fill(&mut self) -> R<bool> {
        unsafe {
            loop {
                let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
                self.reader
                    .ReadSample(FIRST_AUDIO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))
                    .map_err(win(MediaError::Read))?;
                if flags & END_OF_STREAM != 0 {
                    self.eof = true;
                    return Ok(false);
                }
                let Some(s) = sample else { continue };
                let buf = s.ConvertToContiguousBuffer().map_err(win(MediaError::Read))?;
                let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
                buf.Lock(&mut p, None, Some(&mut len)).map_err(win(MediaError::Read))?;
                let data = std::slice::from_raw_parts(p as *const f32, len as usize / 4);
                let start = studio_core::time::time_to_sample(Time::from_hns(ts), self.rate);
                let expected = self.pending_start + (self.pending.len() / 2) as i64;
                if self.pending.is_empty() {
                    self.pending_start = start;
                } else if start > expected {
                    // 途切れた分は無音で埋める
                    self.pending.extend(std::iter::repeat(0.0).take(((start - expected) * 2) as usize));
                }
                self.pending.extend_from_slice(data);
                buf.Unlock().ok();
                return Ok(true);
            }
        }
    }

    /// 標本番号 `start` から `frames` 標本分(ステレオなので長さは frames*2)。範囲外は無音。
    pub fn read(&mut self, start: i64, frames: usize) -> R<Vec<f32>> {
        init();
        let end = start + frames as i64;
        let have_end = self.pending_start + (self.pending.len() / 2) as i64;
        // 必要な位置がバッファの外(後ろ、または大きく先)ならシークし直す
        let jump = start < self.pending_start || start > have_end + self.rate as i64;
        if jump {
            seek(&self.reader, studio_core::time::sample_to_time(start, self.rate))?;
            self.pending.clear();
            self.eof = false;
            self.pending_start = start;
            // シーク直後の最初のサンプルの時刻で合わせ直す
            if self.fill()? && self.pending_start > start {
                let gap = (self.pending_start - start) as usize;
                let mut v = vec![0.0; gap * 2];
                v.extend_from_slice(&self.pending);
                self.pending = v;
                self.pending_start = start;
            }
        }
        while !self.eof && self.pending_start + ((self.pending.len() / 2) as i64) < end {
            self.fill()?;
        }
        let mut out = vec![0.0f32; frames * 2];
        let off = start - self.pending_start;
        for i in 0..frames as i64 {
            let j = off + i;
            if j >= 0 && ((j * 2 + 1) as usize) < self.pending.len() {
                out[(i * 2) as usize] = self.pending[(j * 2) as usize];
                out[(i * 2 + 1) as usize] = self.pending[(j * 2 + 1) as usize];
            }
        }
        // 使い終わった部分を捨てる(少し残して、わずかな巻き戻しに備える)
        let keep_from = (start - self.rate as i64).max(self.pending_start);
        let drop = ((keep_from - self.pending_start) * 2) as usize;
        if drop > 0 && drop <= self.pending.len() {
            self.pending.drain(..drop);
            self.pending_start = keep_from;
        }
        Ok(out)
    }
}

/// H.264 + AAC の MP4 を書き出す。
pub struct Exporter {
    writer: IMFSinkWriter,
    video: u32,
    audio: Option<u32>,
    width: u32,
    height: u32,
    rate: Rate,
    sample_rate: u32,
    frames: i64,
    samples: i64,
    nv12: Vec<u8>,
}

pub struct ExportSettings {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rate,
    pub video_bitrate: u32,
    /// None なら音声なし
    pub sample_rate: Option<u32>,
    pub audio_bitrate: u32,
}

impl Exporter {
    pub fn create(path: &Path, s: &ExportSettings) -> R<Self> {
        init();
        if s.width % 2 != 0 || s.height % 2 != 0 {
            return Err(MediaError::Write("幅と高さは偶数にしてください".into()));
        }
        let p = wide(&path.to_string_lossy());
        let e = win(MediaError::Write);
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1).map_err(&e)?;
            let attrs = attrs.unwrap();
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).map_err(&e)?;
            let writer = MFCreateSinkWriterFromURL(PCWSTR(p.as_ptr()), None, &attrs).map_err(|x| MediaError::Write(format!("{}: {x}", path.display())))?;
            let vtype = |sub: &GUID| -> windows::core::Result<IMFMediaType> {
                let t = MFCreateMediaType()?;
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                t.SetGUID(&MF_MT_SUBTYPE, sub)?;
                t.SetUINT64(&MF_MT_FRAME_SIZE, pack(s.width, s.height))?;
                t.SetUINT64(&MF_MT_FRAME_RATE, pack(s.frame_rate.num, s.frame_rate.den))?;
                t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
                t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                Ok(t)
            };
            let out = vtype(&MFVideoFormat_H264).map_err(&e)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, s.video_bitrate).map_err(&e)?;
            let video = writer.AddStream(&out).map_err(&e)?;
            writer.SetInputMediaType(video, &vtype(&MFVideoFormat_NV12).map_err(&e)?, None).map_err(&e)?;
            let mut audio = None;
            if let Some(rate) = s.sample_rate {
                let atype = |sub: &GUID, avg: u32| -> windows::core::Result<IMFMediaType> {
                    let t = MFCreateMediaType()?;
                    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    t.SetGUID(&MF_MT_SUBTYPE, sub)?;
                    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
                    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
                    t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, avg)?;
                    if *sub == MFAudioFormat_PCM {
                        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
                    }
                    Ok(t)
                };
                let a = writer.AddStream(&atype(&MFAudioFormat_AAC, s.audio_bitrate / 8).map_err(&e)?).map_err(&e)?;
                writer.SetInputMediaType(a, &atype(&MFAudioFormat_PCM, rate * 4).map_err(&e)?, None).map_err(&e)?;
                audio = Some(a);
            }
            writer.BeginWriting().map_err(&e)?;
            Ok(Self {
                writer,
                video,
                audio,
                width: s.width,
                height: s.height,
                rate: s.frame_rate,
                sample_rate: s.sample_rate.unwrap_or(48_000),
                frames: 0,
                samples: 0,
                nv12: vec![0; (s.width * s.height * 3 / 2) as usize],
            })
        }
    }

    fn write(&self, stream: u32, data: &[u8], time: Time, dur: Time) -> R<()> {
        let e = win(MediaError::Write);
        unsafe {
            let buf = MFCreateMemoryBuffer(data.len() as u32).map_err(&e)?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).map_err(&e)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            buf.Unlock().map_err(&e)?;
            buf.SetCurrentLength(data.len() as u32).map_err(&e)?;
            let s = MFCreateSample().map_err(&e)?;
            s.AddBuffer(&buf).map_err(&e)?;
            s.SetSampleTime(time.hns()).map_err(&e)?;
            s.SetSampleDuration(dur.hns()).map_err(&e)?;
            self.writer.WriteSample(stream, &s).map_err(&e)
        }
    }

    /// 次のフレームを書く(BGRA、出力と同じ大きさ。アルファは無視)。
    pub fn write_frame(&mut self, bgra: &[u8]) -> R<()> {
        init();
        let (w, h) = (self.width as usize, self.height as usize);
        if bgra.len() != w * h * 4 {
            return Err(MediaError::Write("フレームの大きさが出力と合いません".into()));
        }
        bgra_to_nv12(bgra, w, h, &mut self.nv12);
        let t = self.rate.frame_to_time(self.frames);
        let d = self.rate.frame_to_time(self.frames + 1) - t;
        let nv12 = std::mem::take(&mut self.nv12);
        let r = self.write(self.video, &nv12, t, d);
        self.nv12 = nv12;
        self.frames += 1;
        r
    }

    /// 次の音声を書く(ステレオ f32、-1〜1)。
    pub fn write_audio(&mut self, stereo: &[f32]) -> R<()> {
        let Some(a) = self.audio else { return Ok(()) };
        let n = stereo.len() / 2;
        let pcm: Vec<u8> = stereo.iter().flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
        let t = studio_core::time::sample_to_time(self.samples, self.sample_rate);
        let d = studio_core::time::sample_to_time(self.samples + n as i64, self.sample_rate) - t;
        self.samples += n as i64;
        self.write(a, &pcm, t, d)
    }

    pub fn finish(self) -> R<()> {
        unsafe { self.writer.Finalize() }.map_err(win(MediaError::Write))
    }
}

/// BGRA → NV12(BT.709、テレビ用の範囲 16〜235)。
pub fn bgra_to_nv12(bgra: &[u8], w: usize, h: usize, out: &mut [u8]) {
    let (y_plane, uv) = out.split_at_mut(w * h);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let (b, g, r) = (bgra[i] as f32, bgra[i + 1] as f32, bgra[i + 2] as f32);
            y_plane[y * w + x] = (16.0 + (0.1826 * r + 0.6142 * g + 0.0620 * b)).round().clamp(0.0, 255.0) as u8;
        }
    }
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let i = ((y + dy) * w + x + dx) * 4;
                b += bgra[i] as f32 / 4.0;
                g += bgra[i + 1] as f32 / 4.0;
                r += bgra[i + 2] as f32 / 4.0;
            }
            let u = 128.0 - 0.1006 * r - 0.3386 * g + 0.4392 * b;
            let v = 128.0 + 0.4392 * r - 0.3989 * g - 0.0403 * b;
            let j = (y / 2) * w + x;
            uv[j] = u.round().clamp(0.0, 255.0) as u8;
            uv[j + 1] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
}

// 読み取り・書き出しのオブジェクトは、MTA(マルチスレッド)で作った COM オブジェクトなので、
// 別のスレッドへ移して使える。同時に複数のスレッドから使うことはしない(&mut で守る)。
// 移した先のスレッドでも、各メソッドの先頭で init() を呼んで COM を初期化する。
unsafe impl Send for VideoReader {}
unsafe impl Send for AudioReader {}
unsafe impl Send for Exporter {}

#[cfg(test)]
mod tests;
