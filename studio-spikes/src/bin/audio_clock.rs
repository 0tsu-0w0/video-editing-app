//! 検証2: 音声デバイスのクロックを再生の時間基準にできるか。
//! 既定の出力デバイスを開き、無音を流しながら、コールバックの間隔・バッファの大きさ・
//! デバイスが示す再生時刻(タイムスタンプ)を記録する。
//! 「出した標本数 ÷ サンプルレート」と、実時間・デバイスの時刻のずれを測る。

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let host = cpal::default_host();
    println!("ホスト: {:?}", host.id());
    let device = host.default_output_device().expect("出力デバイスがありません");
    let name = device.description().map(|d| d.to_string()).unwrap_or_default();
    let config = device.default_output_config().expect("既定の設定を取れません");
    println!("デバイス: {name}");
    println!("既定設定: {:?}", config);
    let rate = config.sample_rate() as f64;
    let channels = config.channels() as u64;

    let frames = Arc::new(AtomicU64::new(0));
    // (実時間, 累計フレーム, コールバック時刻→再生時刻の差[ms], バッファフレーム数)
    let log: Arc<Mutex<Vec<(Instant, u64, f64, usize)>>> = Arc::new(Mutex::new(Vec::with_capacity(4096)));
    let (f2, l2) = (frames.clone(), log.clone());
    let stream = device
        .build_output_stream(
            config.config(),
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                data.fill(0.0); // 無音
                let n = data.len() as u64 / channels;
                let total = f2.fetch_add(n, Ordering::Relaxed) + n;
                let ts = info.timestamp();
                let lead = ts.playback.duration_since(ts.callback).as_secs_f64() * 1000.0;
                if let Ok(mut l) = l2.try_lock() {
                    l.push((Instant::now(), total, lead, n as usize));
                }
            },
            |e| eprintln!("音声エラー: {e}"),
            None,
        )
        .expect("ストリームを作れません(f32 以外の形式の可能性)");
    stream.play().unwrap();
    let t0 = Instant::now();
    std::thread::sleep(Duration::from_secs(secs));
    drop(stream);

    let log = log.lock().unwrap();
    if log.len() < 10 {
        println!("コールバックが少なすぎます: {}", log.len());
        return;
    }
    let intervals: Vec<f64> = log.windows(2).map(|w| (w[1].0 - w[0].0).as_secs_f64() * 1000.0).collect();
    let mean = intervals.iter().sum::<f64>() / intervals.len() as f64;
    let max = intervals.iter().cloned().fold(0.0, f64::max);
    let sizes: std::collections::BTreeSet<usize> = log.iter().map(|x| x.3).collect();
    let leads: Vec<f64> = log.iter().map(|x| x.2).filter(|v| v.is_finite()).collect();
    let lead_avg = leads.iter().sum::<f64>() / leads.len().max(1) as f64;

    // 立ち上がり 1 秒を除き、実時間と「標本数から計算した時間」の進み方を比べる
    let base = log.iter().find(|x| (x.0 - t0) > Duration::from_secs(1)).unwrap();
    let last = log.last().unwrap();
    let wall = (last.0 - base.0).as_secs_f64();
    let audio = (last.1 - base.1) as f64 / rate;
    println!("コールバック回数: {}", log.len());
    println!("バッファ(フレーム数): {:?} = 約 {:.1} ms", sizes, *sizes.iter().next().unwrap() as f64 / rate * 1000.0);
    println!("コールバック間隔: 平均 {:.2} ms / 最大 {:.2} ms", mean, max);
    println!("出力遅延(コールバック→実際の再生): 平均 {:.1} ms", lead_avg);
    println!(
        "{:.2} 秒の実時間に対し、音声の標本は {:.4} 秒分(差 {:+.2} ms、{:+.1} ppm)",
        wall, audio, (audio - wall) * 1000.0, (audio / wall - 1.0) * 1e6
    );
}
