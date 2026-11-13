//! 人物の写真を横に動かした動画で、追跡と部位の取り出しを確かめる。
//! モデル(STUDIO_POSE_MODELS か %LOCALAPPDATA%\ForgeStudio\models)と写真(STUDIO_POSE_IMAGE)が無ければ飛ばす。

use super::*;
use studio_core::command::Command::*;
use studio_core::{Editor, Origin, Rate, TrackKind};
use studio_media::{ExportSettings, Exporter};

#[test]
fn track_person_in_panning_video() {
    let dir = models_dir();
    let Some(photo) = std::env::var("STUDIO_POSE_IMAGE").ok().filter(|_| models_ready(&dir)) else {
        eprintln!("モデルか写真が無いので飛ばします({})", dir.display());
        return;
    };
    let img = image::open(&photo).unwrap().to_rgba8();
    let (w, h) = (img.width() / 2 * 2, img.height() / 2 * 2);
    let tmp = std::env::temp_dir().join(format!("studio-pose-test-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let video = tmp.join("pan.mp4");
    // 1 フレームに 2px ずつ、写真が右へ動く(左の空いた所は灰色)
    const FRAMES: i64 = 45;
    const V: f32 = 2.0;
    let mut ex = Exporter::create(&video, &ExportSettings { width: w, height: h, frame_rate: Rate::new(30, 1), video_bitrate: 8_000_000, sample_rate: None, audio_bitrate: 0 }).unwrap();
    for k in 0..FRAMES {
        let mut bgra = vec![128u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let sx = x as i64 - (V as i64) * k;
                if sx >= 0 {
                    let p = img.get_pixel(sx as u32, y).0;
                    let i = ((y * w + x) * 4) as usize;
                    bgra[i..i + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
                }
            }
        }
        ex.write_frame(&bgra).unwrap();
    }
    ex.finish().unwrap();

    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(w), height: Some(h), frame_rate: Some(Rate::new(30, 1)), sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                ImportAsset { name: "pan".into(), path: video.to_string_lossy().into(), kind: studio_media::probe(&video).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let c = ed.execute(AddClip { track: o.created[0], asset: o.created[1], start: Time::ZERO, duration: None, source_in: None, id: None }, Origin::User).unwrap().created[0];
    let mut models = PoseModels::load(&dir).unwrap();
    // 最初のフレームで人物を探し、その枠の中心をクリックしたことにする
    let mut reader = VideoReader::open(&video).unwrap();
    let f = reader.frame_at(Time::ZERO).unwrap().clone();
    let dets = models.detect(&Frame { px: &f.bgra, w: f.width, h: f.height }).unwrap();
    assert!(!dets.is_empty(), "人物が見つからない");
    let d = dets.iter().max_by(|a, b| a[4].total_cmp(&b[4])).unwrap();
    let pick = ((d[0] + d[2]) / 2.0, (d[1] + d[3]) / 2.0);
    let t0 = std::time::Instant::now();
    let keys = track_person(&mut models, &ed.project, c, Time::ZERO, Time::from_secs_f64(10.0), pick, |_, _| true).unwrap();
    let ms = t0.elapsed().as_secs_f64() * 1000.0 / keys.len() as f64;
    assert_eq!(keys.len() as i64, FRAMES);
    assert!(keys.iter().all(|k| k.keypoints.is_some()), "途中で見失った");
    // 頭・胴体の動きが、写真の動き(1 フレーム 2px)に合う
    for part in ["head", "body"] {
        let tr = part_track(&keys, part).unwrap();
        let p0 = tr[0].pos.unwrap();
        let mut worst = 0.0f32;
        for (k, p) in tr.iter().enumerate() {
            let p = p.pos.unwrap();
            let e = ((p.0 - p0.0 - V * k as f32).powi(2) + (p.1 - p0.1).powi(2)).sqrt();
            worst = worst.max(e);
        }
        eprintln!("{part}: 最大のずれ {worst:.2}px(1 フレーム {ms:.0}ms)");
        assert!(worst < 6.0, "{part} のずれが大きい: {worst}");
    }
    assert!(part_track(&keys, "tail").is_err());
    // 追従のキーフレームにできる
    let png = tmp.join("dot.png");
    image::save_buffer(&png, &[255, 0, 0, 255].repeat(16), 4, 4, image::ExtendedColorType::Rgba8).unwrap();
    let o = ed
        .transaction(
            "印",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                ImportAsset { name: "dot".into(), path: png.to_string_lossy().into(), kind: studio_media::probe(&png).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let dot = ed.execute(AddClip { track: o.created[0], asset: o.created[1], start: Time::ZERO, duration: Some(Time::from_secs_f64(1.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    let cmds = studio_track::follow_commands(&ed.project, dot, c, &part_track(&keys, "head").unwrap()).unwrap();
    ed.transaction("追従", cmds, Origin::User).unwrap();
    assert_eq!(ed.project.clip(dot).unwrap().params.x.keys.len(), 30);
    std::fs::remove_dir_all(&tmp).ok();
}
