//! 再生用の音声エンジン。再生中の「現在時刻」は、音声デバイスが実際に鳴らした標本数で決める。
//!
//! - 作るスレッド: プロジェクトから少し先(約 0.3 秒)までの音をミックスしてバッファへ入れる
//! - 鳴らすスレッド(cpal のコールバック): バッファから取り出して鳴らし、鳴らした数を数える
//! 映像はこの時刻に合わせて表示する。音声デバイスが無いときは実時間の時計で代用する。

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use studio_core::time::{sample_to_time, time_to_sample};
use studio_core::{Project, Time};
use studio_render::Mixer;

const AHEAD_SECS: f64 = 0.3;
const CHUNK: usize = 1024;

struct Shared {
    rate: u32,
    buf: Mutex<VecDeque<f32>>,
    playing: AtomicBool,
    /// 再生開始位置(標本)
    start: AtomicI64,
    /// 再生開始から実際に鳴らした標本数
    played: AtomicI64,
    /// 出力の遅延(標本)
    latency: AtomicI64,
    /// シーク・再生開始のたびに増える(作るスレッドにやり直しを知らせる)
    generation: AtomicU64,
    project: Mutex<Arc<Project>>,
    quit: AtomicBool,
}

pub struct AudioEngine {
    shared: Arc<Shared>,
    _stream: Option<cpal::Stream>,
    /// 音声デバイスが無いとき用
    fallback: Option<(Instant, Time)>,
    pub device_name: String,
}

impl AudioEngine {
    pub fn new(project: Arc<Project>) -> Self {
        let host = cpal::default_host();
        let device = host.default_output_device();
        let config = device.as_ref().and_then(|d| d.default_output_config().ok());
        let rate = config.as_ref().map(|c| c.sample_rate()).unwrap_or(48_000);
        let shared = Arc::new(Shared {
            rate,
            buf: Mutex::new(VecDeque::with_capacity(rate as usize)),
            playing: AtomicBool::new(false),
            start: AtomicI64::new(0),
            played: AtomicI64::new(0),
            latency: AtomicI64::new((rate as i64) * 30 / 1000),
            generation: AtomicU64::new(0),
            project: Mutex::new(project),
            quit: AtomicBool::new(false),
        });
        let mut name = "なし(実時間で再生)".to_string();
        let stream = (|| {
            let device = device?;
            let config = config?;
            if config.sample_format() != cpal::SampleFormat::F32 {
                return None;
            }
            let channels = config.channels() as usize;
            let sh = shared.clone();
            let stream = device
                .build_output_stream(
                    config.config(),
                    move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
                        let ts = info.timestamp();
                        let lat = ts.playback.duration_since(ts.callback).as_secs_f64();
                        sh.latency.store((lat * sh.rate as f64) as i64, Ordering::Relaxed);
                        let frames = out.len() / channels;
                        let playing = sh.playing.load(Ordering::Relaxed);
                        let mut buf = match sh.buf.try_lock() {
                            Ok(b) => b,
                            Err(_) => {
                                out.fill(0.0);
                                return;
                            }
                        };
                        let mut got = 0;
                        for f in 0..frames {
                            let (l, r) = if playing && buf.len() >= 2 {
                                got += 1;
                                (buf.pop_front().unwrap(), buf.pop_front().unwrap())
                            } else {
                                (0.0, 0.0)
                            };
                            for c in 0..channels {
                                out[f * channels + c] = if c % 2 == 0 { l } else { r };
                            }
                        }
                        if playing {
                            sh.played.fetch_add(got, Ordering::Relaxed);
                        }
                    },
                    |e| eprintln!("音声エラー: {e}"),
                    None,
                )
                .ok()?;
            stream.play().ok()?;
            name = device.description().map(|d| d.to_string()).unwrap_or_default();
            Some(stream)
        })();

        // 音を作るスレッド
        let sh = shared.clone();
        std::thread::spawn(move || {
            let mut mixer = Mixer::new(sh.rate);
            let mut gen = u64::MAX;
            let mut next: i64 = 0;
            while !sh.quit.load(Ordering::Relaxed) {
                let g = sh.generation.load(Ordering::Acquire);
                if g != gen {
                    gen = g;
                    next = sh.start.load(Ordering::Acquire);
                    sh.buf.lock().unwrap().clear();
                }
                let buffered = sh.buf.lock().unwrap().len() / 2;
                if !sh.playing.load(Ordering::Relaxed) || buffered as f64 > AHEAD_SECS * sh.rate as f64 {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let project = sh.project.lock().unwrap().clone();
                let chunk = mixer.mix(&project, next, CHUNK);
                if sh.generation.load(Ordering::Acquire) == gen {
                    sh.buf.lock().unwrap().extend(chunk);
                    next += CHUNK as i64;
                }
            }
        });
        Self { shared, _stream: stream, fallback: None, device_name: name }
    }

    /// 編集されたプロジェクトを渡す(次にミックスする分から反映)。
    pub fn set_project(&self, p: Arc<Project>) {
        *self.shared.project.lock().unwrap() = p;
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    pub fn play(&mut self, from: Time) {
        let s = &self.shared;
        s.playing.store(false, Ordering::Relaxed);
        s.start.store(time_to_sample(from, s.rate), Ordering::Release);
        s.played.store(0, Ordering::Relaxed);
        s.generation.fetch_add(1, Ordering::AcqRel);
        s.playing.store(true, Ordering::Release);
        self.fallback = if self._stream.is_none() { Some((Instant::now(), from)) } else { None };
    }

    pub fn stop(&mut self) {
        self.shared.playing.store(false, Ordering::Release);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// いま耳に届いている音の時刻(映像はこれに合わせる)。
    pub fn position(&self) -> Time {
        if let Some((t0, from)) = self.fallback {
            return from + Time::from_secs_f64(t0.elapsed().as_secs_f64());
        }
        let s = &self.shared;
        let pos = s.start.load(Ordering::Relaxed) + s.played.load(Ordering::Relaxed) - s.latency.load(Ordering::Relaxed);
        sample_to_time(pos.max(s.start.load(Ordering::Relaxed)), s.rate)
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
    }
}
