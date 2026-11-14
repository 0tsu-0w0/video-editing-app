//! 書き出した動画を読み戻して、フレーム・シーク・音声・音ズレを確かめる。

use super::*;
use std::path::PathBuf;

const W: u32 = 640;
const H: u32 = 360;
const FPS: u32 = 30;
const SECS: u32 = 6;
const BLOCK: u32 = 32;
const BITS: u32 = 8;

fn is_flash(i: u32) -> bool {
    i % FPS == 0
}

/// フレーム番号を白黒ブロックで描いた BGRA。1 秒ごとに全面が明るくなる。
fn frame(i: u32) -> Vec<u8> {
    let mut v = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let k = ((y * W + x) * 4) as usize;
            let base = if is_flash(i) { 230 } else { 40 + ((x + i * 4) % 100) as u8 };
            v[k..k + 4].copy_from_slice(&[base, base, base, 255]);
        }
    }
    for b in 0..BITS {
        let c = if (i >> b) & 1 == 1 { 255 } else { 0 };
        for y in 0..BLOCK {
            for x in 0..BLOCK {
                let k = ((y * W + b * BLOCK + x) * 4) as usize;
                v[k..k + 3].copy_from_slice(&[c, c, c]);
            }
        }
    }
    v
}

fn read_index(f: &Frame) -> u32 {
    let mut v = 0;
    for b in 0..BITS {
        let (x, y) = (b * BLOCK + BLOCK / 2, BLOCK / 2);
        if f.bgra[((y * f.width + x) * 4 + 1) as usize] > 128 {
            v |= 1 << b;
        }
    }
    v
}

/// 1 秒ごとの白フラッシュのフレームの間だけ 1kHz のビープ。
fn audio_for_frame(i: u32, rate: u32) -> Vec<f32> {
    let n = rate / FPS;
    (0..n)
        .flat_map(|k| {
            let t = (i * n + k) as f32 / rate as f32;
            let v = if is_flash(i) { (t * 1000.0 * std::f32::consts::TAU).sin() * 0.5 } else { 0.0 };
            [v, v]
        })
        .collect()
}

fn make_test_video() -> PathBuf {
    let dir = std::env::temp_dir().join("studio-media-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("test_{}.mp4", std::process::id()));
    let mut ex = Exporter::create(
        &path,
        &ExportSettings { width: W, height: H, frame_rate: Rate::new(FPS, 1), video_bitrate: 4_000_000, sample_rate: Some(48_000), audio_bitrate: 192_000 },
    )
    .unwrap();
    for i in 0..FPS * SECS {
        ex.write_frame(&frame(i)).unwrap();
        ex.write_audio(&audio_for_frame(i, 48_000)).unwrap();
    }
    ex.finish().unwrap();
    path
}

#[test]
fn export_probe_read_seek_and_sync() {
    let path = make_test_video();

    // 情報の取得
    match probe(&path).unwrap() {
        AssetKind::Video { width, height, frame_rate, duration, has_audio } => {
            assert_eq!((width, height, frame_rate), (W, H, Rate::new(FPS, 1)));
            assert!(has_audio);
            assert!((duration.secs_f64() - SECS as f64).abs() < 0.1, "{duration}");
        }
        k => panic!("動画として認識されない: {k:?}"),
    }

    // 先頭から順に読む: 全フレームの番号が合う
    let mut vr = VideoReader::open(&path).unwrap();
    assert_eq!(vr.size(), (W, H));
    let rate = Rate::new(FPS, 1);
    for i in 0..FPS * SECS {
        let t = rate.frame_to_time(i as i64) + Time(rate.frame_duration().0 / 2);
        let f = vr.frame_at(t).unwrap();
        assert_eq!(read_index(f), i, "順再生 {i}");
    }
    // ばらばらの位置へのシーク(後ろ・前・同じ位置)
    for i in [150u32, 3, 90, 91, 89, 0, 179, 45] {
        let t = rate.frame_to_time(i as i64) + Time(rate.frame_duration().0 / 2);
        let f = vr.frame_at(t).unwrap();
        assert_eq!(read_index(f), i, "シーク {i}");
    }

    // 音声: 1 秒ごとのビープの立ち上がりが、フラッシュの時刻と一致する(1 フレーム未満のずれ)
    let mut ar = AudioReader::open(&path, 48_000).unwrap();
    let all = ar.read(0, 48_000 * SECS as usize).unwrap();
    let mut onsets = vec![];
    let mut quiet = 100;
    for (i, ch) in all.chunks(2 * 48).enumerate() {
        let peak = ch.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        if peak > 0.1 {
            if quiet >= 10 {
                onsets.push(i as f64 * 48.0 / 48_000.0);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    assert_eq!(onsets.len(), SECS as usize, "{onsets:?}");
    for (k, o) in onsets.iter().enumerate() {
        let d = (o - k as f64) * 1000.0;
        assert!(d.abs() < 1000.0 / FPS as f64, "{k} 秒目のビープが {d:.1}ms ずれている");
    }
    // 途中からの読み出し(シーク)も同じ内容になる
    let part = ar.read(48_000 * 3 - 480, 4800).unwrap();
    let base = ((48_000 * 3 - 480) * 2) as usize;
    let diff = part.iter().zip(&all[base..base + 9600]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(diff < 0.02, "シーク後の音声が違う({diff})");

    std::fs::remove_file(&path).ok();
}

#[test]
fn nv12_conversion_matches_bt709() {
    // 4x2 の画像: 上の行が白、下の行が赤
    let mut bgra = vec![0u8; 4 * 2 * 4];
    for i in 0..8 {
        let c: [u8; 4] = if i < 4 { [255, 255, 255, 255] } else { [0, 0, 255, 255] };
        bgra[i * 4..i * 4 + 4].copy_from_slice(&c);
    }
    let mut out = vec![0u8; 4 * 2 * 3 / 2];
    bgra_to_nv12(&bgra, 4, 2, &mut out);
    assert_eq!(&out[0..4], &[235; 4]); // 白の Y
    assert_eq!(&out[4..8], &[63; 4]); // 赤の Y(16 + 0.1826*255)
    // 色差は白と赤の平均: U は 128 より小さく、V は大きい
    assert!(out[8] < 128 && out[9] > 128, "{:?}", &out[8..]);
}

#[test]
fn image_probe_and_load() {
    let dir = std::env::temp_dir().join("studio-media-test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("img.png");
    image::save_buffer(&p, &[255, 0, 0, 128, 0, 255, 0, 255], 2, 1, image::ExtendedColorType::Rgba8).unwrap();
    assert_eq!(probe(&p).unwrap(), AssetKind::Image { width: 2, height: 1 });
    let f = load_image(&p).unwrap();
    assert_eq!(&f.bgra[..4], &[0, 0, 255, 128]); // BGRA に並べ替え、アルファは保持
    assert!(probe(&dir.join("no_such_file.mp4")).is_err());
}

#[test]
fn proxy_is_small_frame_exact_and_seeks_fast() {
    let dir = std::env::temp_dir().join(format!("studio-proxy-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: このテストの中だけで使う(ほかのテストはこの変数を読まない)
    unsafe { std::env::set_var("STUDIO_PROXY_DIR", dir.join("proxies")) };
    // 1280x720・10 秒・フレーム番号入り(キーフレームの間隔は既定のまま = 長い)
    let (w, h, n) = (1280u32, 720u32, 300u32);
    let src = dir.join("source.mp4");
    let mut ex = Exporter::create(&src, &ExportSettings { width: w, height: h, frame_rate: Rate::new(30, 1), video_bitrate: 8_000_000, sample_rate: None, audio_bitrate: 0 }).unwrap();
    for i in 0..n {
        let mut v = vec![0u8; (w * h * 4) as usize];
        for (k, px) in v.chunks_exact_mut(4).enumerate() {
            let (x, y) = (k as u32 % w, k as u32 / w);
            let base = (40 + (x + y + i * 7) % 120) as u8;
            px.copy_from_slice(&[base, base, base, 255]);
            // フレーム番号: 64x64 のブロック 9 個
            if y < 64 && x < 64 * 9 && (i >> (x / 64)) & 1 == 1 {
                px.copy_from_slice(&[255, 255, 255, 255]);
            } else if y < 64 && x < 64 * 9 {
                px.copy_from_slice(&[0, 0, 0, 255]);
            }
        }
        ex.write_frame(&v).unwrap();
    }
    ex.finish().unwrap();

    assert!(proxy::existing_proxy(&src).is_none());
    let mut calls = 0;
    let p = proxy::make_proxy(&src, |_| {
        calls += 1;
        true
    })
    .unwrap();
    assert!(calls > 3);
    assert_eq!(proxy::existing_proxy(&src).as_deref(), Some(p.as_path()));
    let AssetKind::Video { width, height, duration, .. } = probe(&p).unwrap() else { panic!() };
    assert_eq!((width, height), (960, 540));
    let AssetKind::Video { duration: d0, .. } = probe(&src).unwrap() else { panic!() };
    assert!((duration.secs_f64() - d0.secs_f64()).abs() < 0.05, "{duration} / {d0}");
    let read_index = |f: &Frame| {
        let s = f.width as f64 / 1280.0;
        (0..9).fold(0u32, |a, b| {
            let (x, y) = (((b * 64 + 32) as f64 * s) as u32, (32.0 * s) as u32);
            let i = ((y * f.width + x) * 4) as usize;
            a | (((f.bgra[i] > 128) as u32) << b)
        })
    };
    // 飛び飛びの位置へシーク: どちらも同じフレームが出る。時間を比べる
    let order = [250u32, 30, 199, 7, 280, 120, 61, 299, 5, 175, 90, 240];
    let time = |path: &Path| {
        let mut r = VideoReader::open(path).unwrap();
        let t0 = std::time::Instant::now();
        for &k in &order {
            let f = r.frame_at(Rate::new(30, 1).frame_to_time(k as i64) + Time(1000)).unwrap();
            assert_eq!(read_index(f), k, "{} のフレーム {k}", path.display());
        }
        t0.elapsed().as_secs_f64() * 1000.0 / order.len() as f64
    };
    let (orig, prox) = (time(&src), time(&p));
    eprintln!("シーク 1 回あたり: 元 {orig:.1}ms / プロキシ {prox:.1}ms");
    assert!(prox < orig, "プロキシの方が遅い: {prox} / {orig}");
    // 元のファイルが変わったら別のプロキシ扱い
    std::thread::sleep(std::time::Duration::from_millis(20));
    let f = std::fs::File::options().write(true).open(&src).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
    drop(f);
    assert!(proxy::existing_proxy(&src).is_none());
    assert_eq!(proxy::proxy_size(3840, 2160), (960, 540));
    assert_eq!(proxy::proxy_size(1080, 1920), (304, 540));
    std::fs::remove_dir_all(&dir).ok();
}
