//! 動きが分かっている動画を作り、追跡と追従の精度を確かめる。

use super::*;
use studio_core::command::Command::*;
use studio_core::{Editor, Origin, Rate};
use studio_media::{ExportSettings, Exporter};
use vision::{texture, Rng};

const W: u32 = 640;
const H: u32 = 360;
const FPS: u32 = 30;
const FRAMES: u32 = 90;
/// 模様が 1 フレームごとに動く量(画素)
const VX: f32 = 2.5;
const VY: f32 = -1.25;

fn s(x: f64) -> Time {
    Time::from_secs_f64(x)
}

/// 模様が一定の速さで右上へ流れる動画。
fn make_video(path: &Path) {
    let tex = texture(1024, &mut Rng(7), 1.0);
    let mut ex = Exporter::create(path, &ExportSettings { width: W, height: H, frame_rate: Rate::new(FPS, 1), video_bitrate: 8_000_000, sample_rate: None, audio_bitrate: 0 }).unwrap();
    for k in 0..FRAMES {
        let mut bgra = vec![255u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                // 画面の (x, y) には、模様の (x - VX*k, y - VY*k) が見える → 模様は (+VX, +VY) 方向へ動く
                let v = tex.sample(x as f32 - VX * k as f32 + 200.0, y as f32 - VY * k as f32 + 200.0);
                let c = (v * 255.0) as u8;
                let i = ((y * W + x) * 4) as usize;
                bgra[i..i + 3].copy_from_slice(&[c, c, c]);
            }
        }
        ex.write_frame(&bgra).unwrap();
    }
    ex.finish().unwrap();
}

#[test]
fn track_and_follow_known_motion() {
    let dir = std::env::temp_dir().join(format!("studio-track-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join("moving.mp4");
    make_video(&video);
    let png = dir.join("dot.png");
    image::save_buffer(&png, &[255, 0, 0, 255].repeat(16), 4, 4, image::ExtendedColorType::Rgba8).unwrap();

    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: Some(Rate::new(FPS, 1)), sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                ImportAsset { name: "moving".into(), path: video.to_string_lossy().into(), kind: studio_media::probe(&video).unwrap(), id: None },
                ImportAsset { name: "dot".into(), path: png.to_string_lossy().into(), kind: studio_media::probe(&png).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [v1, v2, mov, dot] = o.created[..] else { panic!() };
    let o = ed
        .transaction(
            "配置",
            vec![
                AddClip { track: v1, asset: mov, start: s(0.0), duration: None, source_in: None, id: None },
                AddClip { track: v2, asset: dot, start: s(0.5), duration: Some(s(2.0)), source_in: None, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [src, target] = o.created[..] else { panic!() };
    ed.execute(SetParam { clip: target, param: "x".into(), value: 10.0 }, Origin::User).unwrap();
    let p = ed.project.clone();

    // 追跡元の自動判定と、画面座標 ↔ 素材の画素の変換
    assert_eq!(source_below(&p, target, s(1.0)), Some(src));
    assert_eq!(screen_to_source(&p, src, s(0.0), (0.0, 0.0)), Some((320.0, 180.0)));

    // クリックした点を「角」に吸着させてから追跡する(0.5 秒目 = 15 フレーム目から)
    let mut reader = VideoReader::open(&video).unwrap();
    let f = reader.frame_at(s(0.5) + Time(Rate::new(FPS, 1).frame_duration().0 / 2)).unwrap().clone();
    let start = best_point_near(&f.bgra, f.width, f.height, (300.0, 200.0), 24.0);
    assert!(dist(start, (300.0, 200.0)) <= 24.0 * 1.5);
    let mut calls = 0;
    let tr = track_point(&p, src, s(0.5), s(2.5), start, |_, _| {
        calls += 1;
        true
    })
    .unwrap();
    assert_eq!(tr.len(), 60);
    assert_eq!(calls, 60);
    let mut max_err = 0.0f32;
    for (k, pt) in tr.iter().enumerate() {
        let pos = pt.pos.unwrap_or_else(|| panic!("{k} フレーム目で見失った"));
        let expect = (start.0 + VX * k as f32, start.1 + VY * k as f32);
        max_err = max_err.max(dist(pos, expect));
    }
    eprintln!("追跡の最大誤差 {max_err:.3}px");
    assert!(max_err < 1.0, "追跡の誤差が大きい: {max_err}");

    // 追従のキーフレーム: 開始時の位置(x=10, y=0)から、模様と同じだけ動く
    let cmds = follow_commands(&p, target, src, &tr).unwrap();
    ed.transaction("追従", cmds, Origin::User).unwrap();
    let c = ed.project.clip(target).unwrap().clone();
    assert_eq!(c.params.x.keys.len(), 60);
    let at = |k: usize| (c.params.x.eval(tr[k].time - c.start), c.params.y.eval(tr[k].time - c.start));
    let (x0, y0) = at(0);
    assert!((x0 - 10.0).abs() < 1e-9 && y0.abs() < 1e-9);
    let (x1, y1) = at(59);
    assert!((x1 - (10.0 + VX as f64 * 59.0)).abs() < 1.0, "x {x1}");
    assert!((y1 - VY as f64 * 59.0).abs() < 1.0, "y {y1}");
    // 1 回の Undo で追従が消える
    ed.undo();
    assert!(ed.project.clip(target).unwrap().params.x.keys.is_empty());

    // 拡大した動画の上でも、画面上の移動量に直して追従する
    ed.execute(SetParam { clip: src, param: "scale".into(), value: 2.0 }, Origin::User).unwrap();
    let cmds = follow_commands(&ed.project, target, src, &tr).unwrap();
    ed.transaction("追従", cmds, Origin::User).unwrap();
    let c = ed.project.clip(target).unwrap().clone();
    let x1 = c.params.x.eval(tr[59].time - c.start);
    assert!((x1 - (10.0 + 2.0 * VX as f64 * 59.0)).abs() < 2.0, "拡大時の x {x1}");

    // 平面の追跡: 四角形の四隅が、模様と同じだけ動く
    let quad = [(200.0, 100.0), (440.0, 110.0), (430.0, 260.0), (210.0, 250.0)];
    let pl = track_plane(&p, src, s(0.5), s(2.5), quad, |_, _| true).unwrap();
    assert_eq!(pl.len(), 60);
    let mut max_err = 0.0f32;
    for (k, key) in pl.iter().enumerate() {
        let c = key.corners.unwrap_or_else(|| panic!("平面を {k} フレーム目で見失った"));
        for (q, e) in c.iter().zip(quad) {
            max_err = max_err.max(dist(*q, (e.0 + VX * k as f32, e.1 + VY * k as f32)));
        }
    }
    eprintln!("平面の四隅の最大誤差 {max_err:.3}px");
    assert!(max_err < 1.5, "平面の追跡の誤差が大きい: {max_err}");
    let cmds = pin_commands(&p, target, src, &pl).unwrap();
    let mut ed2 = Editor::new(p.clone());
    ed2.transaction("貼り付け", cmds, Origin::User).unwrap();
    let c = ed2.project.clip(target).unwrap();
    assert_eq!(c.pin.len(), 60);
    // 画面の中央が原点(素材 640x360 を等倍で中央に表示)
    assert!((c.pin[0].corners[0][0] - (200.0 - 320.0)).abs() < 0.5 && (c.pin[0].corners[0][1] - (100.0 - 180.0)).abs() < 0.5, "{:?}", c.pin[0]);
    // 模様の無い所は追跡できない
    assert!(track_plane(&p, src, s(0.5), s(1.0), [(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (0.0, 5.0)], |_, _| true).is_err());
    assert!(pin_commands(&p, src, src, &pl).is_err());

    // 間違った使い方
    assert!(follow_commands(&p, src, src, &tr).is_err());
    assert!(track_point(&p, target, s(0.5), s(1.0), (1.0, 1.0), |_, _| true).is_err());
    std::fs::remove_dir_all(&dir).ok();
}
