//! 合成・ミックス・書き出しを、実際の素材ファイルで確かめる。

use super::*;
use std::path::PathBuf;
use studio_core::command::Command::*;
use studio_core::project::Interp;
use studio_core::{Editor, Origin, Rate, TrackKind};

const W: u32 = 640;
const H: u32 = 360;
const FPS: u32 = 30;
const SECS: u32 = 4;
const BLOCK: u32 = 32;

fn s(x: f64) -> Time {
    Time::from_secs_f64(x)
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("studio-render-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// フレーム番号のブロック入りの素材動画(1 秒ごとにビープ)。
fn make_source(path: &Path) {
    let mut ex = Exporter::create(
        path,
        &ExportSettings { width: W, height: H, frame_rate: Rate::new(FPS, 1), video_bitrate: 4_000_000, sample_rate: Some(48_000), audio_bitrate: 192_000 },
    )
    .unwrap();
    for i in 0..FPS * SECS {
        let mut v = vec![60u8; (W * H * 4) as usize];
        for b in 0..8 {
            let c = if (i >> b) & 1 == 1 { 255 } else { 0 };
            for y in 0..BLOCK {
                for x in 0..BLOCK {
                    let k = ((y * W + b * BLOCK + x) * 4) as usize;
                    v[k..k + 3].copy_from_slice(&[c, c, c]);
                }
            }
        }
        ex.write_frame(&v).unwrap();
        let n = 48_000 / FPS;
        let audio: Vec<f32> = (0..n)
            .flat_map(|k| {
                let t = (i * n + k) as f32 / 48_000.0;
                let v = if i % FPS == 0 { (t * 1000.0 * std::f32::consts::TAU).sin() * 0.5 } else { 0.0 };
                [v, v]
            })
            .collect();
        ex.write_audio(&audio).unwrap();
    }
    ex.finish().unwrap();
}

fn read_index(bgra: &[u8], width: u32) -> u32 {
    let mut v = 0;
    for b in 0..8 {
        let (x, y) = (b * BLOCK + BLOCK / 2, BLOCK / 2);
        if bgra[((y * width + x) * 4 + 1) as usize] > 128 {
            v |= 1 << b;
        }
    }
    v
}

fn px(bgra: &[u8], width: u32, x: i32, y: i32) -> [u8; 4] {
    let i = ((y as u32 * width + x as u32) * 4) as usize;
    [bgra[i], bgra[i + 1], bgra[i + 2], bgra[i + 3]]
}

fn is_red(p: [u8; 4]) -> bool {
    p[2] > 200 && p[1] < 60 && p[0] < 60
}

/// 映像 2 トラック(動画 + 横長の赤い棒の画像)と音声 1 トラックのプロジェクト。
fn project(dir: &Path) -> (Project, Id) {
    let video = dir.join("source.mp4");
    make_source(&video);
    let bar = dir.join("bar.png");
    image::save_buffer(&bar, &[255, 0, 0, 255].repeat(200 * 40), 200, 40, image::ExtendedColorType::Rgba8).unwrap();
    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: Some(Rate::new(FPS, 1)), sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                AddTrack { kind: TrackKind::Audio, name: None, index: None, id: None },
                ImportAsset { name: "source.mp4".into(), path: video.to_string_lossy().into(), kind: studio_media::probe(&video).unwrap(), id: None },
                ImportAsset { name: "bar.png".into(), path: bar.to_string_lossy().into(), kind: studio_media::probe(&bar).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [v1, v2, a1, movie, img] = o.created[..] else { panic!() };
    let o = ed
        .transaction(
            "配置",
            vec![
                AddClip { track: v1, asset: movie, start: s(0.0), duration: None, source_in: None, id: None },
                AddClip { track: v2, asset: img, start: s(1.0), duration: Some(s(2.0)), source_in: None, id: None },
                AddClip { track: a1, asset: movie, start: s(0.0), duration: None, source_in: None, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [_, bar_clip, audio_clip] = o.created[..] else { panic!() };
    ed.transaction(
        "動き",
        vec![
            SetParam { clip: bar_clip, param: "x".into(), value: 150.0 },
            // 1 秒目(クリップの 0 秒)から 2 秒目にかけて、0° → 90° に回る
            SetKey { clip: bar_clip, param: "rotation".into(), time: s(0.0), value: 0.0, interp: Interp::Linear },
            SetKey { clip: bar_clip, param: "rotation".into(), time: s(1.0), value: 90.0, interp: Interp::Linear },
            SetParam { clip: audio_clip, param: "volume".into(), value: 0.5 },
        ],
        Origin::User,
    )
    .unwrap();
    (ed.project, bar_clip)
}

#[test]
fn render_mix_and_export() {
    let d = dir();
    let (p, _bar) = project(&d);
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    let (cx, cy) = (W as i32 / 2, H as i32 / 2);

    // 0.5 秒: 動画だけ(15 フレーム目)
    let f = r.render_bgra(&p, s(0.5)).unwrap();
    assert_eq!(read_index(&f, W), 15);
    assert!(!is_red(px(&f, W, cx + 150, cy)));
    // 1.0 秒: 赤い棒が横向き(幅 200)で x=+150 に出る
    let f = r.render_bgra(&p, s(1.0)).unwrap();
    assert_eq!(read_index(&f, W), 30);
    assert!(is_red(px(&f, W, cx + 150 + 90, cy)), "棒の右端付近");
    assert!(!is_red(px(&f, W, cx + 150, cy + 60)), "縦には伸びていない");
    // 2.5 秒: 90° 回って縦向き
    let f = r.render_bgra(&p, s(2.5)).unwrap();
    assert_eq!(read_index(&f, W), 75);
    assert!(is_red(px(&f, W, cx + 150, cy + 90)), "縦に伸びた");
    assert!(!is_red(px(&f, W, cx + 150 + 90, cy)), "横には無い");
    // 後ろへ戻る(シーク)
    let f = r.render_bgra(&p, s(0.2)).unwrap();
    assert_eq!(read_index(&f, W), 6);
    assert!(r.errors.is_empty(), "{:?}", r.errors);

    // ミックス: 1 秒目のビープが音量 0.5 で鳴る(元の振幅 0.5 → 0.25)
    let mut m = Mixer::new(48_000);
    let a = m.mix(&p, 48_000, 1600);
    let peak = a.iter().fold(0.0f32, |x, v| x.max(v.abs()));
    assert!((peak - 0.25).abs() < 0.03, "ピーク {peak}");
    let quiet = m.mix(&p, 48_000 + 4800, 1600);
    assert!(quiet.iter().all(|v| v.abs() < 0.02));

    // 書き出し → 読み戻し
    let out = d.join("export.mp4");
    // 素材の長さは、音声の符号化の余白でわずかに 4 秒を超えることがある → 最後の端数もフレームにする
    let rate = Rate::new(FPS, 1);
    let expect = rate.time_to_frame(p.duration() + Time(rate.frame_duration().0 - 1));
    assert!((120..=121).contains(&expect), "{}", p.duration());
    let mut last = 0;
    export(&p, &out, |pr| {
        last = pr.frame;
        assert_eq!(pr.total, expect);
        true
    })
    .unwrap();
    assert_eq!(last, expect);
    match studio_media::probe(&out).unwrap() {
        AssetKind::Video { width, height, duration, has_audio, .. } => {
            assert_eq!((width, height), (W, H));
            assert!(has_audio);
            assert!((duration.secs_f64() - SECS as f64).abs() < 0.1);
        }
        k => panic!("{k:?}"),
    }
    let mut vr = VideoReader::open(&out).unwrap();
    let f = vr.frame_at(s(2.5) + Time(Rate::new(FPS, 1).frame_duration().0 / 2)).unwrap();
    assert_eq!(read_index(&f.bgra, W), 75);
    assert!(is_red(px(&f.bgra, W, cx + 150, cy + 90)));
    let mut ar = AudioReader::open(&out, 48_000).unwrap();
    let a = ar.read(48_000 * 2, 1600).unwrap();
    let peak = a.iter().fold(0.0f32, |x, v| x.max(v.abs()));
    assert!((peak - 0.25).abs() < 0.05, "書き出し後のピーク {peak}");

    // 中止できる
    let r2 = export(&p, &d.join("cancel.mp4"), |pr| pr.frame < 10);
    assert!(r2.is_err());
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn corner_pin_draws_perspective_quad() {
    let gpu = Gpu::new(None).unwrap();
    // 左半分が赤、右半分が緑の 64x64
    let mut px = vec![0u8; 64 * 64 * 4];
    for y in 0..64 {
        for x in 0..64 {
            let i = (y * 64 + x) * 4;
            px[i..i + 4].copy_from_slice(if x < 32 { &[0, 0, 255, 255] } else { &[0, 255, 0, 255] });
        }
    }
    let src = gpu.create_texture(64, 64, Some(&px), false).unwrap();
    let dst = gpu.create_texture(400, 400, None, true).unwrap();
    let rtv = gpu.rtv(&dst).unwrap();
    gpu.clear(&rtv, [0.0, 0.0, 0.0, 1.0]);
    // 台形(上が狭い): 画面中央原点
    let q = [[-50.0, -100.0], [50.0, -100.0], [150.0, 100.0], [-150.0, 100.0]];
    gpu.draw_pinned(&rtv, (400, 400), &src, &q, 1.0);
    let out = gpu.read(&dst).unwrap();
    let at = |x: f64, y: f64| {
        let (px, py) = ((x + 200.0) as usize, (y + 200.0) as usize);
        let i = (py * 400 + px) * 4;
        [out[i], out[i + 1], out[i + 2]]
    };
    assert_eq!(at(-100.0, 90.0), [0, 0, 255], "左下は赤");
    assert_eq!(at(100.0, 90.0), [0, 255, 0], "右下は緑");
    assert_eq!(at(-40.0, -95.0), [0, 0, 255]);
    assert_eq!(at(-120.0, -50.0), [0, 0, 0], "台形の外は描かない");
    assert_eq!(at(0.0, 150.0), [0, 0, 0]);
    // 中央の縦線は、射影変換では直線のまま(x = 0)
    assert_eq!(at(-3.0, 0.0), [0, 0, 255]);
    assert_eq!(at(3.0, 0.0), [0, 255, 0]);
    // 遠近: 上下の中点(y=0)は、素材の中央(v=0.5)より上の部分を表す。上が狭い = 奥なので、
    // 素材の上半分は画面で小さく見える。素材の横線 v=0.5 は y=0 より上(負)に来る
    let h = super::gpu::square_to_quad(&q);
    let map = |u: f64, v: f64| {
        let w = h[2][0] * u + h[2][1] * v + h[2][2];
        ((h[0][0] * u + h[0][1] * v + h[0][2]) / w, (h[1][0] * u + h[1][1] * v + h[1][2]) / w)
    };
    for (i, (u, v)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].into_iter().enumerate() {
        let p = map(u, v);
        assert!((p.0 - q[i][0]).abs() < 1e-9 && (p.1 - q[i][1]).abs() < 1e-9);
    }
    assert!(map(0.5, 0.5).1 < 0.0);
}

/// 幅 2・高さ 1・厚さ 0.2 の赤い板。1 秒で上へ 1 動くアニメーション付き。
fn write_plate(path: &Path) {
    let (x, y, z) = (1.0f32, 0.5f32, 0.1f32);
    let pos: Vec<[f32; 3]> = (0..8).map(|i| [if i & 1 == 0 { -x } else { x }, if i & 2 == 0 { -y } else { y }, if i & 4 == 0 { -z } else { z }]).collect();
    let quads = [[0, 1, 3, 2], [4, 6, 7, 5], [0, 4, 5, 1], [2, 3, 7, 6], [0, 2, 6, 4], [1, 5, 7, 3]];
    let idx: Vec<u16> = quads.iter().flat_map(|q| [q[0], q[1], q[2], q[0], q[2], q[3]]).collect();
    let mut buf: Vec<u8> = vec![];
    for p in &pos {
        for v in p {
            buf.extend(v.to_le_bytes());
        }
    }
    for i in &idx {
        buf.extend(i.to_le_bytes());
    }
    for v in [0.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        buf.extend(v.to_le_bytes());
    }
    let b64 = {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in buf.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                s.push(if k <= c.len() { T[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
            }
        }
        s
    };
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"material":0}}]}}],
"materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[1,0,0,1]}}}}],
"buffers":[{{"byteLength":{len},"uri":"data:application/octet-stream;base64,{b64}"}}],
"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":96}},{{"buffer":0,"byteOffset":96,"byteLength":72}},{{"buffer":0,"byteOffset":168,"byteLength":8}},{{"buffer":0,"byteOffset":176,"byteLength":24}}],
"accessors":[{{"bufferView":0,"componentType":5126,"count":8,"type":"VEC3","min":[-1,-0.5,-0.1],"max":[1,0.5,0.1]}},
{{"bufferView":1,"componentType":5123,"count":36,"type":"SCALAR"}},
{{"bufferView":2,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]}},
{{"bufferView":3,"componentType":5126,"count":2,"type":"VEC3"}}],
"animations":[{{"channels":[{{"sampler":0,"target":{{"node":0,"path":"translation"}}}}],"samplers":[{{"input":2,"output":3}}]}}]}}"#,
        len = buf.len()
    );
    std::fs::write(path, json).unwrap();
}

/// 赤い画素の (数, 左右の幅, 中心 x, 中心 y)。
fn red_stats(px: &[u8], w: usize) -> (usize, usize, f64, f64) {
    let (mut n, mut lo, mut hi, mut sx, mut sy) = (0, usize::MAX, 0, 0.0, 0.0);
    for (i, p) in px.chunks_exact(4).enumerate() {
        if p[2] > 80 && p[1] < 40 && p[0] < 40 {
            let (x, y) = (i % w, i / w);
            n += 1;
            lo = lo.min(x);
            hi = hi.max(x);
            sx += x as f64;
            sy += y as f64;
        }
    }
    (n, hi.saturating_sub(lo), sx / n.max(1) as f64, sy / n.max(1) as f64)
}

#[test]
fn model_renders_rotates_and_animates() {
    let d = dir();
    let path = d.join("plate.gltf");
    write_plate(&path);
    let kind = model::probe(&path).unwrap();
    assert_eq!(kind, AssetKind::Model { animation: 1.0 });
    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: None, sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                ImportAsset { name: "plate".into(), path: path.to_string_lossy().into(), kind, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let c = ed.execute(AddClip { track: o.created[0], asset: o.created[1], start: s(0.0), duration: None, source_in: None, id: None }, Origin::User).unwrap().created[0];
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    let still = r.render_bgra(&ed.project, s(0.0)).unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let (n0, w0, cx0, cy0) = red_stats(&still, W as usize);
    assert!(n0 > 5000, "板が描かれていない: {n0}");
    // 囲む球が画面の高さの約 85% → 幅 2 の板は、だいたい画面の高さの 0.85 * 2 / √5.04 * ... の幅で中央に描かれる
    assert!((cx0 - W as f64 / 2.0).abs() < 3.0 && (cy0 - H as f64 / 2.0).abs() < 3.0, "中央にない: {cx0},{cy0}");
    // 横に 90 度向けると、薄い側面だけが見える
    ed.execute(SetParam { clip: c, param: "rot_y".into(), value: 90.0 }, Origin::User).unwrap();
    let side = r.render_bgra(&ed.project, s(0.0)).unwrap();
    let (_, w90, _, _) = red_stats(&side, W as usize);
    assert!(w90 * 4 < w0, "向きが変わっていない: 幅 {w0} → {w90}");
    ed.execute(SetParam { clip: c, param: "rot_y".into(), value: 0.0 }, Origin::User).unwrap();
    // 位置・拡大はふつうのレイヤーと同じ
    ed.execute(SetParam { clip: c, param: "x".into(), value: 100.0 }, Origin::User).unwrap();
    ed.execute(SetParam { clip: c, param: "scale".into(), value: 0.5 }, Origin::User).unwrap();
    let moved = r.render_bgra(&ed.project, s(0.0)).unwrap();
    let (_, wm, cxm, _) = red_stats(&moved, W as usize);
    assert!((cxm - (W as f64 / 2.0 + 100.0)).abs() < 3.0, "x {cxm}");
    assert!((wm as f64 - w0 as f64 / 2.0).abs() < 4.0, "幅 {wm} / {w0}");
    // アニメーション: 0.5 秒で上へ 0.5 動く(画面では上 = y が小さい)
    let anim = r.render_bgra(&ed.project, s(0.5)).unwrap();
    let (_, _, _, cya) = red_stats(&anim, W as usize);
    let (_, _, _, cym) = red_stats(&moved, W as usize);
    assert!(cya < cym - 10.0, "動いていない: {cym} → {cya}");
    eprintln!("板の幅 {w0}px → 横向き {w90}px、アニメーションで y {cym:.1} → {cya:.1}");
}

/// 16bit ステレオの WAV。f(秒) が左右に同じ値で入る。
fn write_wav(path: &Path, secs: f64, f: impl Fn(f64) -> f32) {
    let rate = 48_000u32;
    let n = (secs * rate as f64) as u32;
    let mut d = Vec::new();
    let data_len = n * 4;
    d.extend(b"RIFF");
    d.extend((36 + data_len).to_le_bytes());
    d.extend(b"WAVEfmt ");
    d.extend(16u32.to_le_bytes());
    d.extend(1u16.to_le_bytes());
    d.extend(2u16.to_le_bytes());
    d.extend(rate.to_le_bytes());
    d.extend((rate * 4).to_le_bytes());
    d.extend(4u16.to_le_bytes());
    d.extend(16u16.to_le_bytes());
    d.extend(b"data");
    d.extend(data_len.to_le_bytes());
    for i in 0..n {
        let v = (f(i as f64 / rate as f64).clamp(-1.0, 1.0) * 32767.0) as i16;
        d.extend(v.to_le_bytes());
        d.extend(v.to_le_bytes());
    }
    std::fs::write(path, d).unwrap();
}

/// 再生と同じく 1024 標本ずつ続けてミックスした 3 秒分。
fn mix_all(p: &Project) -> Vec<f32> {
    let mut m = Mixer::new(48_000);
    let mut out = vec![];
    let mut s = 0;
    while s < 48_000 * 3 {
        out.extend(m.mix(p, s, 1024));
        s += 1024;
    }
    out
}

/// 時刻 [a, b) 秒の (左, 右) のピーク。
fn peak_in(buf: &[f32], a: f64, b: f64) -> (f32, f32) {
    let (i, j) = ((a * 48_000.0) as usize * 2, (b * 48_000.0) as usize * 2);
    super::peak(&buf[i..j])
}

#[test]
fn mixer_gain_pan_solo_eq_compressor_ducking() {
    use studio_core::project::AudioEffect;
    let d = dir();
    let (voice, bgm) = (d.join("voice.wav"), d.join("bgm.wav"));
    // 声: 1〜2 秒に 1kHz、BGM: ずっと 200Hz
    write_wav(&voice, 3.0, |t| if (1.0..2.0).contains(&t) { 0.5 * (t * 1000.0 * std::f64::consts::TAU).sin() as f32 } else { 0.0 });
    write_wav(&bgm, 3.0, |t| 0.25 * (t * 200.0 * std::f64::consts::TAU).sin() as f32);
    let mut ed = Editor::new(Project::default());
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Audio, name: Some("声".into()), index: None, id: None },
                AddTrack { kind: TrackKind::Audio, name: Some("BGM".into()), index: None, id: None },
                ImportAsset { name: "voice".into(), path: voice.to_string_lossy().into(), kind: studio_media::probe(&voice).unwrap(), id: None },
                ImportAsset { name: "bgm".into(), path: bgm.to_string_lossy().into(), kind: studio_media::probe(&bgm).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [tv, tb, av, ab] = o.created[..] else { panic!() };
    ed.transaction(
        "配置",
        vec![
            AddClip { track: tv, asset: av, start: s(0.0), duration: None, source_in: None, id: None },
            AddClip { track: tb, asset: ab, start: s(0.0), duration: None, source_in: None, id: None },
        ],
        Origin::User,
    )
    .unwrap();
    let near = |a: f32, b: f32, tol: f32| (a - b).abs() <= tol;
    let base = mix_all(&ed.project);
    let (v0, b0) = (peak_in(&base, 1.2, 1.8), peak_in(&base, 0.2, 0.8));
    assert!(near(v0.0, 0.75, 0.03), "声 + BGM の山 {v0:?}");
    assert!(near(b0.0, 0.25, 0.01), "BGM {b0:?}");

    // 音量 -6dB で約半分、左右のバランスを左いっぱいにすると右は無音
    ed.execute(SetTrackAudio { track: tb, gain_db: Some(-6.0206), pan: Some(-1.0), solo: None }, Origin::User).unwrap();
    let m = mix_all(&ed.project);
    let b = peak_in(&m, 0.2, 0.8);
    assert!(near(b.0, 0.125, 0.006) && b.1 < 1e-4, "音量・バランス {b:?}");
    ed.undo();

    // ソロ: 声だけ
    ed.execute(SetTrackAudio { track: tv, gain_db: None, pan: None, solo: Some(true) }, Origin::User).unwrap();
    let m = mix_all(&ed.project);
    assert!(peak_in(&m, 0.2, 0.8).0 < 1e-4, "ソロなのに BGM が鳴っている");
    ed.undo();

    // イコライザ: 中音(1kHz)を +12dB → 声の 1kHz は約 4 倍(BGM はソロで消して測る)
    ed.execute(SetTrackAudio { track: tv, gain_db: Some(-20.0), pan: None, solo: Some(true) }, Origin::User).unwrap();
    let before = peak_in(&mix_all(&ed.project), 1.3, 1.8).0;
    ed.execute(SetTrackEffects { track: tv, effects: vec![AudioEffect::Eq { low_db: 0.0, mid_db: 12.0, high_db: 0.0 }] }, Origin::User).unwrap();
    let after = peak_in(&mix_all(&ed.project), 1.3, 1.8).0;
    assert!(near(after / before, 3.98, 0.15), "EQ の倍率 {}", after / before);
    // コンプレッサー: -6dB の音を、しきい値 -20dB・4:1 → 超えた 14dB が 3.5dB に(約 -16.5dB = 0.15)
    ed.execute(SetTrackAudio { track: tv, gain_db: Some(0.0), pan: None, solo: None }, Origin::User).unwrap();
    ed.execute(SetTrackEffects { track: tv, effects: vec![AudioEffect::Compressor { threshold_db: -20.0, ratio: 4.0, makeup_db: 0.0 }] }, Origin::User).unwrap();
    let c = peak_in(&mix_all(&ed.project), 1.5, 1.9).0;
    assert!(near(c, 0.15, 0.02), "コンプレッサー後 {c}");
    ed.execute(SetTrackEffects { track: tv, effects: vec![] }, Origin::User).unwrap();
    ed.execute(SetTrackAudio { track: tv, gain_db: None, pan: None, solo: Some(false) }, Origin::User).unwrap();

    // ダッキング: 声が鳴る間 BGM を -12dB。声のトラックを消音しても、きっかけには使える
    ed.execute(SetTrackEffects { track: tb, effects: vec![AudioEffect::Duck { source: tv, amount_db: -12.0 }] }, Origin::User).unwrap();
    ed.execute(SetTrackFlags { track: tv, muted: Some(true), locked: None }, Origin::User).unwrap();
    let mut mx = Mixer::new(48_000);
    let mut m = vec![];
    let mut lv = vec![];
    for k in 0..(48_000 * 3 / 1024) {
        m.extend(mx.mix(&ed.project, k * 1024, 1024));
        lv.push(mx.levels.clone());
    }
    let (pre, during, post) = (peak_in(&m, 0.5, 0.9).0, peak_in(&m, 1.5, 1.9).0, peak_in(&m, 2.8, 2.95).0);
    assert!(near(pre, 0.25, 0.01), "前 {pre}");
    assert!(near(during, 0.25 * 0.251, 0.01), "声の間 {during}");
    assert!(post > 0.2, "声の後は戻る {post}");
    // メーター: 消音中の声は 0、BGM は鳴っている
    let l = &lv[70]; // 約 1.5 秒
    let get = |id: Id| l.tracks.iter().find(|t| t.0 == id).unwrap();
    assert_eq!((get(tv).1, get(tv).2), (0.0, 0.0));
    assert!(get(tb).1 > 0.03 && l.master.0 > 0.03);
    eprintln!("ダッキング: 前 {pre:.3} → 声の間 {during:.3} → 後 {post:.3} / EQ ×{:.2} / コンプ {c:.3}", after / before);
}

#[test]
fn masks_wipes_and_crossfade_render() {
    use studio_core::command::Edge;
    use studio_core::project::{Mask, MaskShape, Transition, TransitionKind};
    let d = dir();
    let (red, blue) = (d.join("red.png"), d.join("blue.png"));
    image::save_buffer(&red, &[255, 0, 0, 255].repeat(200 * 200), 200, 200, image::ExtendedColorType::Rgba8).unwrap();
    image::save_buffer(&blue, &[0, 0, 255, 255].repeat(200 * 200), 200, 200, image::ExtendedColorType::Rgba8).unwrap();
    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: None, sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                ImportAsset { name: "red".into(), path: red.to_string_lossy().into(), kind: studio_media::probe(&red).unwrap(), id: None },
                ImportAsset { name: "blue".into(), path: blue.to_string_lossy().into(), kind: studio_media::probe(&blue).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [tr, ar, ab] = o.created[..] else { panic!() };
    let o = ed
        .transaction(
            "配置",
            vec![
                AddClip { track: tr, asset: ar, start: s(0.0), duration: Some(s(2.0)), source_in: None, id: None },
                AddClip { track: tr, asset: ab, start: s(2.0), duration: Some(s(2.0)), source_in: None, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [cr, cb] = o.created[..] else { panic!() };
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    // 画面中央 (320,180) を原点にした位置の画素 (R, G, B)
    let px = |img: &[u8], x: i32, y: i32| {
        let i = (((180 + y) as u32 * W + (320 + x) as u32) * 4) as usize;
        (img[i + 2], img[i + 1], img[i])
    };
    // 楕円のマスク(幅・高さ 100): 中心は赤、半径 70 の所は黒。反転すると逆
    ed.execute(SetMask { clip: cr, mask: Some(Mask { shape: MaskShape::Ellipse, invert: false }) }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    assert_eq!(px(&img, 0, 0), (255, 0, 0));
    assert_eq!(px(&img, 45, 0), (255, 0, 0));
    assert_eq!(px(&img, 70, 0), (0, 0, 0));
    assert_eq!(px(&img, 45, 45), (0, 0, 0), "楕円の外(四角なら内側)");
    ed.execute(SetMask { clip: cr, mask: Some(Mask { shape: MaskShape::Rect, invert: true }) }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    assert_eq!(px(&img, 0, 0), (0, 0, 0));
    assert_eq!(px(&img, 70, 0), (255, 0, 0));
    // ぼかし 40: 縁の上はおよそ半分
    ed.execute(SetMask { clip: cr, mask: Some(Mask { shape: MaskShape::Rect, invert: false }) }, Origin::User).unwrap();
    ed.execute(SetParam { clip: cr, param: "mask_feather".into(), value: 40.0 }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    let edge = px(&img, 50, 0).0;
    assert!((100..=160).contains(&edge), "ぼかしの縁 {edge}");
    ed.execute(SetMask { clip: cr, mask: None }, Origin::User).unwrap();

    // クロスフェード: 青の始まりに 1 秒のフェード → 2.5 秒(半分)で赤と青が半々
    ed.execute(SetTransition { clip: cb, edge: Edge::In, transition: Some(Transition { kind: TransitionKind::Fade, duration: s(1.0) }) }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(2.5)).unwrap();
    let (rr, _, bb) = px(&img, 0, 0);
    assert!((110..=145).contains(&rr) && (110..=145).contains(&bb), "クロスフェード {rr},{bb}");
    // ワイプ(左→右): 2.5 秒では左側が青、右側はまだ赤
    ed.execute(SetTransition { clip: cb, edge: Edge::In, transition: Some(Transition { kind: TransitionKind::WipeRight, duration: s(1.0) }) }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(2.5)).unwrap();
    assert_eq!(px(&img, -80, 0), (0, 0, 255));
    assert_eq!(px(&img, 80, 0), (255, 0, 0));
    // 円形: 中央から青
    ed.execute(SetTransition { clip: cb, edge: Edge::In, transition: Some(Transition { kind: TransitionKind::Circle, duration: s(1.0) }) }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(2.4)).unwrap();
    assert_eq!(px(&img, 0, 0), (0, 0, 255));
    assert_eq!(px(&img, 95, 95), (255, 0, 0));
}

#[test]
fn preview_with_proxy_matches_original_placement() {
    let d = dir();
    // SAFETY: このテストの中だけで使う
    unsafe { std::env::set_var("STUDIO_PROXY_DIR", d.join("proxies")) };
    let src = d.join("hd.mp4");
    let (w, h) = (1280u32, 720u32);
    let mut ex = Exporter::create(&src, &ExportSettings { width: w, height: h, frame_rate: Rate::new(30, 1), video_bitrate: 8_000_000, sample_rate: None, audio_bitrate: 0 }).unwrap();
    for i in 0..30u32 {
        // 左半分が赤、右半分が青(境目がフレームごとに右へ 4px)
        let edge = 600 + i * 4;
        let v: Vec<u8> = (0..w * h).flat_map(|k| if k % w < edge { [0, 0, 255, 255] } else { [255, 0, 0, 255] }).collect();
        ex.write_frame(&v).unwrap();
    }
    ex.finish().unwrap();
    studio_media::proxy::make_proxy(&src, |_| true).unwrap();
    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: None, sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                ImportAsset { name: "hd".into(), path: src.to_string_lossy().into(), kind: studio_media::probe(&src).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let c = ed.execute(AddClip { track: o.created[0], asset: o.created[1], start: s(0.0), duration: None, source_in: None, id: None }, Origin::User).unwrap().created[0];
    // 1280x720 の素材を 640x360 の画面に、拡大率 0.25 で置く(画面で 320x180)
    ed.execute(SetParam { clip: c, param: "scale".into(), value: 0.25 }, Origin::User).unwrap();
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    let orig = r.render_bgra(&ed.project, s(0.5)).unwrap();
    r.set_use_proxies(true);
    let prox = r.render_bgra(&ed.project, s(0.5)).unwrap();
    // 描かれた範囲(黒でない所)が同じで、色の境目もほぼ同じ所
    let extent = |img: &[u8]| {
        let (mut x0, mut x1, mut y0, mut y1) = (u32::MAX, 0, u32::MAX, 0);
        for (k, p) in img.chunks_exact(4).enumerate() {
            if p[0] > 60 || p[2] > 60 {
                let (x, y) = (k as u32 % W, k as u32 / W);
                (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            }
        }
        (x0, x1, y0, y1)
    };
    assert_eq!(extent(&orig), extent(&prox));
    assert_eq!(extent(&orig), (160, 479, 90, 269));
    let edge = |img: &[u8]| (0..W).find(|&x| img[((180 * W + x) * 4) as usize] > 128).unwrap();
    let (a, b) = (edge(&orig), edge(&prox));
    assert!(a.abs_diff(b) <= 1, "境目 {a} / {b}");
}

#[test]
fn color_blur_and_chroma_key_render() {
    use studio_core::project::ChromaKey;
    let d = dir();
    // 左半分が緑、右半分が赤の画像
    let path = d.join("green_red.png");
    let mut buf = Vec::new();
    for _y in 0..200 {
        for x in 0..200 {
            buf.extend_from_slice(if x < 100 { &[0, 255, 0, 255] } else { &[255, 0, 0, 255] });
        }
    }
    image::save_buffer(&path, &buf, 200, 200, image::ExtendedColorType::Rgba8).unwrap();
    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: None, sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                ImportAsset { name: "gr".into(), path: path.to_string_lossy().into(), kind: studio_media::probe(&path).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [tr, a] = o.created[..] else { panic!() };
    let c = ed.execute(AddClip { track: tr, asset: a, start: s(0.0), duration: Some(s(2.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    let px = |img: &[u8], x: i32, y: i32| {
        let i = (((H as i32 / 2 + y) as u32 * W + (W as i32 / 2 + x) as u32) * 4) as usize;
        (img[i + 2] as i32, img[i + 1] as i32, img[i] as i32)
    };
    let near = |a: (i32, i32, i32), b: (i32, i32, i32)| (a.0 - b.0).abs() <= 3 && (a.1 - b.1).abs() <= 3 && (a.2 - b.2).abs() <= 3;
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    assert!(near(px(&img, -50, 0), (0, 255, 0)) && near(px(&img, 50, 0), (255, 0, 0)));

    // クロマキー(緑): 緑は透明になり(背景の黒)、赤は残る
    let key = ChromaKey { color: "#00FF00".into(), tolerance: 0.25, softness: 0.1, spill: 0.5 };
    ed.execute(SetChromaKey { clip: c, key: Some(key) }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    assert!(near(px(&img, -50, 0), (0, 0, 0)), "緑が残っている {:?}", px(&img, -50, 0));
    assert!(near(px(&img, 50, 0), (255, 0, 0)));
    ed.execute(SetChromaKey { clip: c, key: None }, Origin::User).unwrap();

    // 色相 120 度: 赤 → 緑、緑 → 青
    ed.execute(SetParam { clip: c, param: "hue".into(), value: 120.0 }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    assert!(near(px(&img, 50, 0), (0, 255, 0)), "色相 {:?}", px(&img, 50, 0));
    assert!(near(px(&img, -50, 0), (0, 0, 255)));
    ed.execute(SetParam { clip: c, param: "hue".into(), value: 0.0 }, Origin::User).unwrap();

    // 彩度 -1: 赤は灰色(輝度 0.299 → 76)
    ed.execute(SetParam { clip: c, param: "saturation".into(), value: -1.0 }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    assert!(near(px(&img, 50, 0), (76, 76, 76)), "彩度 {:?}", px(&img, 50, 0));
    // 明るさ +0.2 も重ねる: 76 + 51
    ed.execute(SetParam { clip: c, param: "brightness".into(), value: 0.2 }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    assert!(near(px(&img, 50, 0), (127, 127, 127)), "明るさ {:?}", px(&img, 50, 0));
    ed.execute(SetParam { clip: c, param: "saturation".into(), value: 0.0 }, Origin::User).unwrap();
    ed.execute(SetParam { clip: c, param: "brightness".into(), value: 0.0 }, Origin::User).unwrap();

    // ぼかし 20: 境目はおよそ半々、離れた所は元の色
    ed.execute(SetParam { clip: c, param: "blur".into(), value: 20.0 }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    let (rr, gg, _) = px(&img, 0, 0);
    assert!((90..=170).contains(&rr) && (90..=170).contains(&gg), "境目 {rr},{gg}");
    assert!(near(px(&img, -60, 0), (0, 255, 0)) && near(px(&img, 60, 0), (255, 0, 0)));
    // 取り消しで元に戻る
    ed.undo().unwrap();
    let img = r.render_bgra(&ed.project, s(1.0)).unwrap();
    assert!(near(px(&img, 0, 0), (255, 0, 0)) || near(px(&img, -1, 0), (0, 255, 0)));
}

#[test]
fn speed_changes_video_frame_and_audio_pitch() {
    let d = dir();
    let video = d.join("speed_src.mp4");
    make_source(&video);
    let wav = d.join("tone440.wav");
    write_wav(&wav, 4.0, |t| (t * 440.0 * std::f64::consts::TAU).sin() as f32 * 0.5);
    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: Some(Rate::new(FPS, 1)), sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                AddTrack { kind: TrackKind::Audio, name: None, index: None, id: None },
                ImportAsset { name: "v".into(), path: video.to_string_lossy().into(), kind: studio_media::probe(&video).unwrap(), id: None },
                ImportAsset { name: "a".into(), path: wav.to_string_lossy().into(), kind: studio_media::probe(&wav).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [tv, ta, av, aa] = o.created[..] else { panic!() };
    let o = ed
        .transaction(
            "配置",
            vec![
                AddClip { track: tv, asset: av, start: s(0.0), duration: None, source_in: None, id: None },
                AddClip { track: ta, asset: aa, start: s(0.0), duration: None, source_in: None, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [cv, ca] = o.created[..] else { panic!() };
    ed.execute(SetSpeed { clip: cv, speed: 2.0 }, Origin::User).unwrap();
    ed.execute(SetSpeed { clip: ca, speed: 2.0 }, Origin::User).unwrap();
    // 映像: 0.5 秒目には素材の 1 秒目(フレーム FPS)が出る
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    let img = r.render_bgra(&ed.project, s(0.5)).unwrap();
    assert_eq!(read_index(&img, W), FPS);
    let img = r.render_bgra(&ed.project, s(1.25)).unwrap();
    assert_eq!(read_index(&img, W), FPS * 5 / 2);
    // 音声(高さを保つ、既定): 440Hz のまま(0.5〜1.5 秒の、上向きのゼロ交差の数)
    let ups = |buf: &[f32]| {
        let l: Vec<f32> = buf.chunks(2).map(|c| c[0]).collect();
        l[24_000..72_000].windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    };
    let u = ups(&mix_all(&ed.project));
    assert!((435..=445).contains(&u), "高さを保つ: 上向きのゼロ交差 {u}");
    // 高さを保たない: 440Hz が 880Hz になる
    ed.execute(SetKeepPitch { clip: ca, keep_pitch: false }, Origin::User).unwrap();
    let buf = mix_all(&ed.project);
    let u = ups(&buf);
    assert!((875..=885).contains(&u), "テープ式: 上向きのゼロ交差 {u}");
    // 2 倍速で 2 秒の長さになったので、2 秒以降は無音
    let (pl, _) = peak_in(&buf, 2.1, 2.9);
    assert!(pl < 1e-3, "{pl}");
}

#[test]
fn reverse_plays_frames_and_audio_backwards() {
    let d = dir();
    let video = d.join("rev_src.mp4");
    make_source(&video);
    let wav = d.join("beep_start.wav");
    // 最初の 0.5 秒だけ鳴る音
    write_wav(&wav, 3.0, |t| if t < 0.5 { (t * 500.0 * std::f64::consts::TAU).sin() as f32 * 0.5 } else { 0.0 });
    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: Some(Rate::new(FPS, 1)), sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                AddTrack { kind: TrackKind::Audio, name: None, index: None, id: None },
                ImportAsset { name: "v".into(), path: video.to_string_lossy().into(), kind: studio_media::probe(&video).unwrap(), id: None },
                ImportAsset { name: "a".into(), path: wav.to_string_lossy().into(), kind: studio_media::probe(&wav).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [tv, ta, av, aa] = o.created[..] else { panic!() };
    let o = ed
        .transaction(
            "配置",
            vec![
                AddClip { track: tv, asset: av, start: s(0.0), duration: None, source_in: None, id: None },
                AddClip { track: ta, asset: aa, start: s(0.0), duration: None, source_in: None, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [cv, ca] = o.created[..] else { panic!() };
    ed.execute(SetReverse { clip: cv, reverse: true }, Origin::User).unwrap();
    ed.execute(SetReverse { clip: ca, reverse: true }, Origin::User).unwrap();
    // 映像: 1 フレームずつ進めると、素材のフレーム番号が 1 ずつ減る(最初は最後のフレーム)
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    let last = FPS * SECS - 1;
    let t0 = std::time::Instant::now();
    for i in 0..(FPS * 2) {
        let t = Time::from_secs_f64((i as f64 + 0.5) / FPS as f64);
        let img = r.render_bgra(&ed.project, t).unwrap();
        assert_eq!(read_index(&img, W), last - i, "{i} フレーム目");
    }
    eprintln!("逆再生 {} フレーム: {:?}", FPS * 2, t0.elapsed());
    // 音声: 鳴るのは最後の 0.5 秒(2.5〜3 秒)
    let buf = mix_all(&ed.project);
    assert!(peak_in(&buf, 0.1, 2.4).0 < 1e-3);
    assert!(peak_in(&buf, 2.55, 2.95).0 > 0.3);
}

#[test]
fn frame_blend_mixes_neighbouring_frames() {
    let d = dir();
    let video = d.join("blend_src.mp4");
    make_source(&video);
    let mut ed = Editor::new(Project::default());
    // 60fps のプロジェクトに 30fps の素材
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: Some(Rate::new(60, 1)), sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                ImportAsset { name: "v".into(), path: video.to_string_lossy().into(), kind: studio_media::probe(&video).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [tv, av] = o.created[..] else { panic!() };
    let c = ed.execute(AddClip { track: tv, asset: av, start: s(0.0), duration: None, source_in: None, id: None }, Origin::User).unwrap().created[0];
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    // ビット 0 のブロック(フレーム番号の偶奇で白黒が変わる)の明るさ
    let bit0 = |img: &[u8]| img[((BLOCK / 2 * W + BLOCK / 2) * 4 + 1) as usize] as i32;
    // 10 フレーム目と 11 フレーム目のちょうど間(10.5/30 秒)
    let mid = Time::from_secs_f64(10.5 / 30.0);
    let img = r.render_bgra(&ed.project, mid).unwrap();
    assert!(bit0(&img) < 30, "ブレンドなしは 10 フレーム目のまま {}", bit0(&img));
    ed.execute(SetFrameBlend { on: true }, Origin::User).unwrap();
    // 再生と同じく前から順に描いて、テクスチャの入れ替えも確かめる
    for i in 0..30 {
        let t = Time::from_secs_f64(i as f64 / 60.0);
        let img = r.render_bgra(&ed.project, t).unwrap();
        let v = bit0(&img);
        if i % 2 == 0 {
            // コマの始まりは混ぜない(偶数フレームは黒、奇数は白)
            let want = if (i / 2) % 2 == 0 { 0 } else { 255 };
            assert!((v - want).abs() < 30, "{i}: {v}");
        } else {
            assert!((100..=160).contains(&v), "{i}: 半々に混ざるはず {v}");
        }
    }
    // スロー(0.5 倍)では 30fps のプロジェクトでも混ぜる
    ed.execute(SetSettings { width: None, height: None, frame_rate: Some(Rate::new(30, 1)), sample_rate: None }, Origin::User).unwrap();
    ed.execute(SetSpeed { clip: c, speed: 0.5 }, Origin::User).unwrap();
    let img = r.render_bgra(&ed.project, Time::from_secs_f64(21.0 / 30.0)).unwrap();
    assert!((100..=160).contains(&bit0(&img)), "スロー {}", bit0(&img));
}
