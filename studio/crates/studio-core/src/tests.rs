use crate::command::Command::*;
use crate::eval::{audio_spans, layers_at};
use crate::project::*;
use crate::time::*;
use crate::{Command, Editor, Origin};

fn s(x: f64) -> Time {
    Time::from_secs_f64(x)
}

/// 映像トラック 2 本・音声トラック 1 本と、10 秒の動画素材・静止画素材があるプロジェクト。
fn setup() -> (Editor, Id, Id, Id, Id, Id) {
    let mut ed = Editor::new(Project::default());
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                AddTrack { kind: TrackKind::Audio, name: None, index: None, id: None },
                ImportAsset {
                    name: "movie.mp4".into(),
                    path: "C:/movie.mp4".into(),
                    kind: AssetKind::Video { width: 1920, height: 1080, frame_rate: Rate::new(30, 1), duration: s(10.0), has_audio: true },
                    id: None,
                },
                ImportAsset { name: "chara.png".into(), path: "C:/chara.png".into(), kind: AssetKind::Image { width: 800, height: 1200 }, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let c = &o.created;
    (ed, c[0], c[1], c[2], c[3], c[4])
}

#[test]
fn time_is_exact_for_common_rates() {
    for (num, den) in [(24, 1), (25, 1), (30, 1), (60, 1), (30000, 1001), (60000, 1001), (120, 1)] {
        let r = Rate::new(num, den);
        for f in [0i64, 1, 29, 1000, 107_892] {
            let t = r.frame_to_time(f);
            assert_eq!(r.time_to_frame(t), f, "{num}/{den} frame {f}");
            assert_eq!(r.time_to_frame(t + Time(1)), f);
            assert_eq!(r.time_to_frame(t - Time(1)), f - 1);
        }
        // 1 フレームの長さが整数ちょうど
        assert_eq!(r.frame_to_time(1).0 * num as i64, FLICKS_PER_SECOND * den as i64, "{num}/{den}");
    }
    for rate in [44_100, 48_000, 96_000] {
        assert_eq!(sample_to_time(rate as i64, rate), Time(FLICKS_PER_SECOND));
        assert_eq!(time_to_sample(sample_to_time(123_457, rate), rate), 123_457);
    }
    assert_eq!(Time::from_hns(10_000_000), Time(FLICKS_PER_SECOND));
    assert_eq!(Time(FLICKS_PER_SECOND * 3).hns(), 30_000_000);
}

#[test]
fn time_parse_and_display() {
    assert_eq!(parse_time("00:01:02.500"), Some(s(62.5)));
    assert_eq!(parse_time("1:02.5"), Some(s(62.5)));
    assert_eq!(parse_time("62.5s"), Some(s(62.5)));
    assert_eq!(parse_time("-1.5"), Some(-s(1.5)));
    assert_eq!(parse_time("abc"), None);
    assert_eq!(s(3723.25).to_string(), "01:02:03.250");
}

#[test]
fn add_move_trim_split_and_undo_all() {
    let (mut ed, v1, v2, _a1, movie, img) = setup();
    let before = ed.project.clone();
    let base = ed.history().len();
    let c1 = ed.execute(AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(4.0)), source_in: Some(s(2.0)), id: None }, Origin::User).unwrap().created[0];
    let c2 = ed.execute(AddClip { track: v2, asset: img, start: s(1.0), duration: None, source_in: None, id: None }, Origin::User).unwrap().created[0];
    assert_eq!(ed.project.clip(c2).unwrap().duration, s(5.0)); // 静止画の既定は 5 秒
    ed.execute(MoveClip { clip: c1, start: s(3.0), track: None }, Origin::User).unwrap();
    ed.execute(TrimClip { clip: c1, start: None, duration: Some(s(6.0)), source_in: None }, Origin::User).unwrap();
    let r = ed.execute(SplitClip { clip: c1, at: s(5.0), new_id: None }, Origin::User).unwrap();
    let right = r.created[0];
    let (l, rr) = (ed.project.clip(c1).unwrap().clone(), ed.project.clip(right).unwrap().clone());
    assert_eq!((l.start, l.duration, l.source_in), (s(3.0), s(2.0), s(2.0)));
    assert_eq!((rr.start, rr.duration, rr.source_in), (s(5.0), s(4.0), s(4.0)));
    ed.project.check().unwrap();
    let after = ed.project.clone();

    // 全て取り消すと最初に戻り、全てやり直すと同じ状態(同じ ID)になる
    while ed.history().len() > base {
        ed.undo();
    }
    assert_eq!(ed.project.tracks, before.tracks);
    assert_eq!(ed.project.assets, before.assets);
    while ed.redo().is_some() {}
    assert_eq!(ed.project.tracks, after.tracks);
    ed.project.check().unwrap();
}

#[test]
fn overlaps_and_bounds_are_rejected_without_changes() {
    let (mut ed, v1, _v2, a1, movie, img) = setup();
    ed.execute(AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(4.0)), source_in: None, id: None }, Origin::User).unwrap();
    let snapshot = ed.project.clone();
    let cases: Vec<(Command, &str)> = vec![
        (AddClip { track: v1, asset: movie, start: s(3.0), duration: Some(s(2.0)), source_in: None, id: None }, "overlap"),
        (AddClip { track: v1, asset: movie, start: s(5.0), duration: Some(s(9.0)), source_in: Some(s(2.0)), id: None }, "out_of_range"),
        (AddClip { track: a1, asset: img, start: s(0.0), duration: None, source_in: None, id: None }, "invalid"),
        (AddClip { track: Id(999), asset: movie, start: s(0.0), duration: None, source_in: None, id: None }, "not_found"),
        (AddClip { track: v1, asset: movie, start: s(-1.0), duration: Some(s(0.5)), source_in: None, id: None }, "invalid"),
        (SetParam { clip: Id(999), param: "x".into(), value: 0.0 }, "not_found"),
    ];
    for (c, code) in cases {
        let name = c.name();
        let e = ed.execute(c, Origin::Ai).unwrap_err();
        assert_eq!(e.error.code, code, "{name}: {}", e.error);
        assert_eq!(ed.project, snapshot, "{name} で状態が変わった");
    }
}

#[test]
fn transaction_is_atomic_and_one_undo() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    let snapshot = ed.project.clone();
    let hist = ed.history().len();
    // 3 番目が重なりで失敗 → 1・2 番目も巻き戻る
    let e = ed
        .transaction(
            "まとめて",
            vec![
                AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(2.0)), source_in: None, id: None },
                AddTrack { kind: TrackKind::Video, name: Some("追加".into()), index: Some(0), id: None },
                AddClip { track: v1, asset: movie, start: s(1.0), duration: Some(s(2.0)), source_in: None, id: None },
            ],
            Origin::Ai,
        )
        .unwrap_err();
    assert_eq!((e.index, e.command, e.error.code), (2, "add_clip", "overlap"));
    assert_eq!(ed.project.tracks, snapshot.tracks);
    assert_eq!(ed.history().len(), hist);

    // 成功したトランザクションは 1 回の Undo で全部戻る
    ed.transaction(
        "まとめて",
        vec![
            AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(2.0)), source_in: None, id: None },
            AddClip { track: v1, asset: movie, start: s(2.0), duration: Some(s(2.0)), source_in: None, id: None },
            AddTrack { kind: TrackKind::Video, name: None, index: Some(0), id: None },
        ],
        Origin::Ai,
    )
    .unwrap();
    assert_eq!(ed.history().last().unwrap().origin, Origin::Ai);
    ed.undo();
    assert_eq!(ed.project.tracks, snapshot.tracks);
}

#[test]
fn dry_run_does_not_change_anything() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    let snapshot = ed.project.clone();
    let o = ed.dry_run(&[AddClip { track: v1, asset: movie, start: s(0.0), duration: None, source_in: None, id: None }]).unwrap();
    assert_eq!(o.summary.len(), 1);
    assert!(o.summary[0].contains("追加"));
    assert_eq!(ed.project, snapshot);
    assert!(!ed.can_redo());
    let _ = &mut ed;
}

#[test]
fn locked_tracks_and_internal_commands() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    ed.execute(SetTrackFlags { track: v1, muted: None, locked: Some(true) }, Origin::User).unwrap();
    let e = ed.execute(AddClip { track: v1, asset: movie, start: s(0.0), duration: None, source_in: None, id: None }, Origin::Ai).unwrap_err();
    assert_eq!(e.error.code, "locked");
    // AI から内部用コマンドは使えない
    let e = ed
        .execute(RestoreAsset { asset: Asset { id: Id(500), name: "x".into(), path: "x".into(), kind: AssetKind::Image { width: 1, height: 1 } } }, Origin::Ai)
        .unwrap_err();
    assert_eq!(e.error.code, "forbidden");
}

#[test]
fn keyframes_interpolate_and_survive_split() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    let c = ed.execute(AddClip { track: v1, asset: movie, start: s(1.0), duration: Some(s(4.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    ed.transaction(
        "キー",
        vec![
            SetKey { clip: c, param: "x".into(), time: s(0.0), value: 0.0, interp: Interp::Linear },
            SetKey { clip: c, param: "x".into(), time: s(4.0), value: 400.0, interp: Interp::Linear },
            SetKey { clip: c, param: "opacity".into(), time: s(0.0), value: 0.0, interp: Interp::EaseInOut },
            SetKey { clip: c, param: "opacity".into(), time: s(2.0), value: 1.0, interp: Interp::Linear },
        ],
        Origin::User,
    )
    .unwrap();
    let l = layers_at(&ed.project, s(3.0));
    assert_eq!(l.len(), 1);
    assert!((l[0].x - 200.0).abs() < 1e-6);
    assert!((l[0].opacity - 1.0).abs() < 1e-9);
    let mid = layers_at(&ed.project, s(2.0))[0].opacity; // イーズ(1 秒 / 2 秒 = 0.5 → 0.5)
    assert!((mid - 0.5).abs() < 1e-9);

    // 範囲外の値・時刻・存在しない項目は拒否
    for bad in [
        SetKey { clip: c, param: "opacity".into(), time: s(1.0), value: 2.0, interp: Interp::Linear },
        SetKey { clip: c, param: "x".into(), time: s(9.0), value: 0.0, interp: Interp::Linear },
        SetKey { clip: c, param: "blur".into(), time: s(1.0), value: 0.0, interp: Interp::Linear },
    ] {
        assert!(ed.execute(bad, Origin::Ai).is_err());
    }

    // 分割しても、両側で同じ動きになる(不透明度 0 のときもあるので、値を直接計算する)
    let x_at = |p: &Project, t: Time| {
        let c = p.tracks.iter().flat_map(|tr| tr.clips.iter()).find(|c| c.contains(t)).unwrap();
        c.params.x.eval(t - c.start)
    };
    let times: Vec<Time> = (0..=40).map(|i| s(1.0 + i as f64 * 0.1).min(s(4.999))).collect();
    let before: Vec<f64> = times.iter().map(|t| x_at(&ed.project, *t)).collect();
    ed.execute(SplitClip { clip: c, at: s(2.5), new_id: None }, Origin::User).unwrap();
    let after: Vec<f64> = times.iter().map(|t| x_at(&ed.project, *t)).collect();
    for (a, b) in before.iter().zip(&after) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }
    ed.undo();
    let undone: Vec<f64> = times.iter().map(|t| x_at(&ed.project, *t)).collect();
    assert_eq!(before, undone);
}

#[test]
fn layers_follow_track_order_and_mute() {
    let (mut ed, v1, v2, a1, movie, img) = setup();
    let c1 = ed.execute(AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(5.0)), source_in: Some(s(1.0)), id: None }, Origin::User).unwrap().created[0];
    let c2 = ed.execute(AddClip { track: v2, asset: img, start: s(2.0), duration: Some(s(1.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    let c3 = ed.execute(AddClip { track: a1, asset: movie, start: s(0.0), duration: Some(s(5.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    let l = layers_at(&ed.project, s(2.5));
    assert_eq!(l.iter().map(|x| x.clip).collect::<Vec<_>>(), vec![c1, c2]); // 奥 → 手前
    assert_eq!(l[0].source_time, s(3.5));
    assert_eq!(layers_at(&ed.project, s(3.0)).len(), 1); // 終わりの時刻は含まない
    ed.execute(SetTrackFlags { track: v1, muted: Some(true), locked: None }, Origin::User).unwrap();
    assert_eq!(layers_at(&ed.project, s(2.5)).len(), 1);
    let a = audio_spans(&ed.project, s(4.0), s(6.0));
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].clip, a[0].start, a[0].end, a[0].source_start), (c3, s(4.0), s(5.0), s(4.0)));
}

#[test]
fn project_json_roundtrip_and_validation() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    let c = ed.execute(AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(4.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    ed.execute(SetKey { clip: c, param: "scale".into(), time: s(1.0), value: 2.0, interp: Interp::EaseOut }, Origin::User).unwrap();
    let json = ed.project.to_json();
    assert_eq!(Project::from_json(&json).unwrap(), ed.project);
    // 重なりを持つ壊れたファイルは読み込まない
    let mut broken = ed.project.clone();
    let mut dup = broken.tracks[0].clips[0].clone();
    dup.id = Id(broken.next_id);
    broken.next_id += 1;
    broken.tracks[0].clips.push(dup);
    assert!(Project::from_json(&broken.to_json()).unwrap_err().contains("重なって"));
    let mut newer = ed.project.clone();
    newer.version = 99;
    assert!(Project::from_json(&newer.to_json()).is_err());
}

#[test]
fn commands_from_ai_style_json() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    // AI が書く形: 時刻は秒の数値か文字列
    let json = format!(
        r#"[
          {{"command":"add_clip","track":{v1},"asset":{movie},"start":"00:00:01.5","duration":2}},
          {{"command":"set_key","clip":100,"param":"opacity","time":0,"value":0}}
        ]"#
    );
    let mut cmds: Vec<Command> = serde_json::from_str(&json).unwrap();
    let o = ed.dry_run(&cmds[..1]).unwrap();
    assert!(o.summary[0].contains("00:00:01.500"));
    let id = ed.execute(cmds.remove(0), Origin::Ai).unwrap().created[0];
    assert_eq!(ed.project.clip(id).unwrap().start, s(1.5));
    // 知らない項目・知らないコマンドは読み込みで弾く
    assert!(serde_json::from_str::<Command>(r#"{"command":"add_clip","track":1,"asset":2,"start":0,"oops":1}"#).is_err());
    assert!(serde_json::from_str::<Command>(r#"{"command":"format_disk"}"#).is_err());
    // 履歴に残る形は、作られた ID を含む
    let h = serde_json::to_string(&ed.history().last().unwrap().forward).unwrap();
    assert!(h.contains(&format!("\"id\":{}", id.0)), "{h}");
}

#[test]
fn merged_edits_undo_as_one() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    let c = ed.execute(AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(4.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    let before = ed.project.clone();
    let n = ed.history().len();
    let w = std::time::Duration::from_secs(5);
    for v in [10.0, 20.0, 30.0] {
        ed.transaction_merged("x", vec![SetParam { clip: c, param: "x".into(), value: v }], Origin::User, "x:1", w).unwrap();
    }
    ed.transaction_merged("f", vec![SetFade { clip: c, fade_in: Some(s(0.5)), fade_out: Some(s(1.0)) }], Origin::User, "fade", w).unwrap();
    assert_eq!(ed.history().len(), n + 2, "同じ目印の変更は 1 つにまとまる");
    assert_eq!(ed.project.clip(c).unwrap().params.x.value, 30.0);
    ed.undo();
    assert_eq!(ed.project.clip(c).unwrap().fade_in, Time::ZERO);
    ed.undo();
    assert_eq!(ed.project, before);
    ed.redo();
    assert_eq!(ed.project.clip(c).unwrap().params.x.value, 30.0);
}

#[test]
fn fades_validate_survive_split_and_trim() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    let c = ed.execute(AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(4.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    assert!(ed.execute(SetFade { clip: c, fade_in: Some(s(3.0)), fade_out: Some(s(2.0)) }, Origin::Ai).is_err());
    ed.execute(SetFade { clip: c, fade_in: Some(s(1.0)), fade_out: Some(s(1.0)) }, Origin::User).unwrap();
    let cl = ed.project.clip(c).unwrap().clone();
    assert!((cl.fade_gain(s(0.5)) - 0.5).abs() < 1e-9 && (cl.fade_gain(s(2.0)) - 1.0).abs() < 1e-9 && (cl.fade_gain(s(3.75)) - 0.25).abs() < 1e-9);
    assert!((layers_at(&ed.project, s(0.25))[0].opacity - 0.25).abs() < 1e-9);
    // 分割: 左はフェードインだけ、右はフェードアウトだけ
    let r = ed.execute(SplitClip { clip: c, at: s(2.0), new_id: None }, Origin::User).unwrap().created[0];
    let (l, rr) = (ed.project.clip(c).unwrap().clone(), ed.project.clip(r).unwrap().clone());
    assert_eq!((l.fade_in, l.fade_out, rr.fade_in, rr.fade_out), (s(1.0), Time::ZERO, Time::ZERO, s(1.0)));
    // 短くすると、フェードも収まるよう縮む。取り消すと元に戻る
    ed.execute(TrimClip { clip: r, start: None, duration: Some(s(0.5)), source_in: None }, Origin::User).unwrap();
    assert_eq!(ed.project.clip(r).unwrap().fade_out, s(0.5));
    ed.undo();
    assert_eq!(ed.project.clip(r).unwrap().fade_out, s(1.0));
    ed.project.check().unwrap();
}

#[test]
fn text_assets_and_set_text() {
    let (mut ed, v1, _v2, _a1, _movie, _img) = setup();
    let cmds: Vec<Command> = serde_json::from_str(r#"[{"command": "import_asset", "name": "文字", "path": "", "kind": {"type": "text", "text": "こんにちは", "style": {"size": 96}}}]"#).unwrap();
    let a = ed.transaction("文字", cmds, Origin::Ai).unwrap().created[0];
    let c = ed.execute(AddClip { track: v1, asset: a, start: s(0.0), duration: None, source_in: None, id: None }, Origin::User).unwrap().created[0];
    assert_eq!(ed.project.clip(c).unwrap().duration, s(5.0));
    ed.execute(SetText { asset: a, text: Some("さようなら".into()), font: None, size: Some(40.0), color: Some("#FF000080".into()), outline: None, outline_color: None, align: None }, Origin::User).unwrap();
    match &ed.project.assets[&a].kind {
        AssetKind::Text { text, style } => assert_eq!((text.as_str(), style.size, style.color.as_str()), ("さようなら", 40.0, "#FF000080")),
        _ => panic!(),
    }
    for bad in [
        SetText { asset: a, text: None, font: None, size: Some(0.0), color: None, outline: None, outline_color: None, align: None },
        SetText { asset: a, text: None, font: None, size: None, color: Some("red".into()), outline: None, outline_color: None, align: None },
        SetText { asset: a, text: None, font: Some("../../x".into()), size: None, color: None, outline: None, outline_color: None, align: None },
    ] {
        assert!(ed.execute(bad, Origin::Ai).is_err());
    }
    ed.undo();
    match &ed.project.assets[&a].kind {
        AssetKind::Text { text, style } => assert_eq!((text.as_str(), style.size), ("こんにちは", 96.0)),
        _ => panic!(),
    }
    ed.project.check().unwrap();
    assert_eq!(Project::from_json(&ed.project.to_json()).unwrap(), ed.project);
}
