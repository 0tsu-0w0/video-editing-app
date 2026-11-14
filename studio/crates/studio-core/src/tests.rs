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
        SetKey { clip: c, param: "glow".into(), time: s(1.0), value: 0.0, interp: Interp::Linear },
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

#[test]
fn corner_pin_interpolates_splits_and_undoes() {
    let (mut ed, v1, _v2, a1, movie, img) = setup();
    let c = ed.execute(AddClip { track: v1, asset: img, start: s(1.0), duration: Some(s(4.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    // AI からの JSON(秒で指定)
    let cmds: Vec<Command> = serde_json::from_str(&format!(
        r#"[{{"command": "set_pin", "clip": {}, "keys": [
            {{"time": 0, "corners": [[-100,-100],[100,-100],[100,100],[-100,100]]}},
            {{"time": 2, "corners": [[0,-100],[200,-100],[200,100],[0,100]]}}]}}]"#,
        c.0
    ))
    .unwrap();
    ed.transaction("四隅", cmds, Origin::Ai).unwrap();
    let l = &layers_at(&ed.project, s(2.0))[0];
    let k = l.corners.unwrap();
    assert!((k[0][0] - -50.0).abs() < 1e-6 && (k[2][0] - 150.0).abs() < 1e-6, "{k:?}");
    assert_eq!(layers_at(&ed.project, s(4.5))[0].corners.unwrap()[0], [0.0, -100.0]);
    // 時刻が逆順・範囲外は拒否
    let bad = PinKey { time: s(1.0), corners: [[0.0; 2]; 4] };
    assert!(ed.execute(SetPin { clip: c, keys: vec![bad, PinKey { time: s(0.5), ..bad }] }, Origin::Ai).is_err());
    assert!(ed.execute(SetPin { clip: c, keys: vec![PinKey { corners: [[1e9, 0.0]; 4], ..bad }] }, Origin::Ai).is_err());
    // 音声トラックのクリップには置けない
    let ac = ed.execute(AddClip { track: a1, asset: movie, start: s(0.0), duration: Some(s(1.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    assert!(ed.execute(SetPin { clip: ac, keys: vec![bad] }, Origin::User).is_err());
    // 分割しても、画面上の四隅は変わらない
    let before: Vec<_> = [1.5, 2.0, 2.5, 3.5].iter().map(|&t| layers_at(&ed.project, s(t))[0].corners).collect();
    ed.execute(SplitClip { clip: c, at: s(2.0), new_id: None }, Origin::User).unwrap();
    let after: Vec<_> = [1.5, 2.0, 2.5, 3.5].iter().map(|&t| layers_at(&ed.project, s(t))[0].corners).collect();
    for (a, b) in before.iter().zip(&after) {
        for (p, q) in a.unwrap().iter().zip(b.unwrap().iter()) {
            assert!((p[0] - q[0]).abs() < 1e-6 && (p[1] - q[1]).abs() < 1e-6, "{a:?} {b:?}");
        }
    }
    ed.project.check().unwrap();
    // 保存・読み込みで保たれる
    let p2 = Project::from_json(&ed.project.to_json()).unwrap();
    assert_eq!(p2.clip(c).unwrap().pin.len(), ed.project.clip(c).unwrap().pin.len());
    ed.undo();
    ed.execute(SetPin { clip: c, keys: vec![] }, Origin::User).unwrap();
    assert!(layers_at(&ed.project, s(2.0))[0].corners.is_none());
    ed.undo();
    assert!(layers_at(&ed.project, s(2.0))[0].corners.is_some());
}

#[test]
fn track_audio_and_effects_validate_and_undo() {
    let (mut ed, v1, _v2, a1, _movie, _img) = setup();
    let a2 = ed.execute(AddTrack { kind: TrackKind::Audio, name: None, index: None, id: None }, Origin::User).unwrap().created[0];
    // 映像トラックには使えない、範囲外は拒否
    assert!(ed.execute(SetTrackAudio { track: v1, gain_db: Some(-6.0), pan: None, solo: None }, Origin::Ai).is_err());
    assert!(ed.execute(SetTrackAudio { track: a1, gain_db: Some(30.0), pan: None, solo: None }, Origin::Ai).is_err());
    ed.execute(SetTrackAudio { track: a1, gain_db: Some(-6.0), pan: Some(0.5), solo: None }, Origin::Ai).unwrap();
    // JSON(AI の書き方)で、BGM(a2)を声(a1)の裏で下げる
    let cmds: Vec<Command> = serde_json::from_str(&format!(
        r#"[{{"command": "set_track_effects", "track": {}, "effects": [{{"type": "eq", "low_db": 3, "mid_db": 0, "high_db": -2}}, {{"type": "duck", "source": {}, "amount_db": -12}}]}}]"#,
        a2.0, a1.0
    ))
    .unwrap();
    ed.transaction("ダッキング", cmds, Origin::Ai).unwrap();
    assert_eq!(ed.project.track(a2).unwrap().audio.effects.len(), 2);
    // 自分自身・映像トラック・範囲外は拒否
    let duck = |s: Id, a: f64| SetTrackEffects { track: a2, effects: vec![AudioEffect::Duck { source: s, amount_db: a }] };
    assert!(ed.execute(duck(a2, -12.0), Origin::Ai).is_err());
    assert!(ed.execute(duck(v1, -12.0), Origin::Ai).is_err());
    assert!(ed.execute(duck(a1, -80.0), Origin::Ai).is_err());
    ed.execute(SetMasterGain { gain_db: -3.0 }, Origin::Ai).unwrap();
    let p2 = Project::from_json(&ed.project.to_json()).unwrap();
    assert_eq!(p2, ed.project);
    // 取り消すと元へ
    ed.undo();
    assert_eq!(ed.project.settings.master_gain_db, 0.0);
    ed.undo();
    assert!(ed.project.track(a2).unwrap().audio.effects.is_empty());
    ed.undo();
    assert!(ed.project.track(a1).unwrap().audio.is_default());
}

#[test]
fn transitions_crossfade_slide_and_masks() {
    use crate::command::Edge;
    let (mut ed, v1, _v2, _a1, movie, img) = setup();
    let o = ed
        .transaction(
            "配置",
            vec![
                AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(4.0)), source_in: Some(s(5.0)), id: None },
                AddClip { track: v1, asset: img, start: s(4.0), duration: Some(s(4.0)), source_in: None, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [a, b] = o.created[..] else { panic!() };
    // JSON(AI の書き方)
    let cmds: Vec<Command> = serde_json::from_str(&format!(r#"[{{"command": "set_transition", "clip": {}, "edge": "in", "transition": {{"kind": "fade", "duration": 1}}}}]"#, b.0)).unwrap();
    ed.transaction("トランジション", cmds, Origin::Ai).unwrap();
    // 1.5 秒は長すぎ(反対側と合わせて 4 秒を超える)
    let tr = |k: TransitionKind, d: f64| Some(Transition { kind: k, duration: s(d) });
    assert!(ed.execute(SetTransition { clip: b, edge: Edge::Out, transition: tr(TransitionKind::Fade, 3.5) }, Origin::Ai).is_err());
    assert!(ed.execute(SetTransition { clip: b, edge: Edge::Out, transition: tr(TransitionKind::Fade, 0.0) }, Origin::Ai).is_err());
    // クロスフェード: 4.5 秒では、前のクリップ(素材の 9.5 秒 = 終わりの先)を下に、後のクリップを半分の不透明度で
    let l = layers_at(&ed.project, s(4.5));
    assert_eq!(l.len(), 2, "{l:?}");
    assert_eq!((l[0].clip, l[0].slot, l[1].clip, l[1].slot), (a, 1, b, 0));
    assert_eq!(l[0].opacity, 1.0);
    assert!((l[1].opacity - 0.5).abs() < 1e-9);
    assert_eq!(l[0].source_time, s(9.5));
    // 素材の長さを越える所では最後のフレームで止まる
    let l = layers_at(&ed.project, s(4.99));
    assert!(l[0].source_time < s(10.0));
    assert_eq!(layers_at(&ed.project, s(5.2)).len(), 1);
    // スライド: 始まりの途中では右にずれている
    ed.execute(SetTransition { clip: b, edge: Edge::In, transition: tr(TransitionKind::SlideLeft, 1.0) }, Origin::User).unwrap();
    let l = layers_at(&ed.project, s(4.5));
    assert!((l[1].x - 1920.0 * 0.5).abs() < 1e-6, "{}", l[1].x);
    // ワイプ: 終わりの途中で、見えている割合が減る
    ed.execute(SetTransition { clip: b, edge: Edge::Out, transition: tr(TransitionKind::WipeRight, 1.0) }, Origin::User).unwrap();
    let l = layers_at(&ed.project, s(7.75));
    let (k, q) = l[0].wipe.unwrap();
    assert!(k == TransitionKind::WipeRight && q < 0.5 && q > 0.0, "{q}");
    // 分割: 左は始まり、右は終わりのトランジションだけ残る
    let r = ed.execute(SplitClip { clip: b, at: s(6.0), new_id: None }, Origin::User).unwrap().created[0];
    let (lc, rc) = (ed.project.clip(b).unwrap().clone(), ed.project.clip(r).unwrap().clone());
    assert!(lc.transition_in.is_some() && lc.transition_out.is_none() && rc.transition_in.is_none() && rc.transition_out.is_some());
    // 短くして収まらなくなったら外れる
    ed.execute(TrimClip { clip: r, start: None, duration: Some(s(0.5)), source_in: None }, Origin::User).unwrap();
    assert!(ed.project.clip(r).unwrap().transition_out.is_none());
    ed.project.check().unwrap();

    // マスク: 初めて付けると素材(800x1200)の半分の大きさ。外すと消える
    ed.execute(SetMask { clip: b, mask: Some(Mask { shape: MaskShape::Ellipse, invert: true }) }, Origin::Ai).unwrap();
    let l = layers_at(&ed.project, s(5.5));
    let m = l.last().unwrap().mask.unwrap();
    assert_eq!((m.w, m.h, m.invert, m.shape), (400.0, 600.0, true, MaskShape::Ellipse));
    ed.execute(SetKey { clip: b, param: "mask_x".into(), time: s(1.0), value: 50.0, interp: Interp::Linear }, Origin::User).unwrap();
    assert_eq!(layers_at(&ed.project, s(5.5)).last().unwrap().mask.unwrap().x, 50.0);
    let p2 = Project::from_json(&ed.project.to_json()).unwrap();
    assert_eq!(p2, ed.project);
    ed.execute(SetMask { clip: b, mask: None }, Origin::User).unwrap();
    assert!(layers_at(&ed.project, s(5.5)).last().unwrap().mask.is_none());
}

#[test]
fn chroma_key_and_color_params_validate_split_and_undo() {
    let (mut ed, v1, _v2, a1, movie, _img) = setup();
    let c = ed.execute(AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(4.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    let key = ChromaKey { color: "#00FF00".into(), tolerance: 0.3, softness: 0.1, spill: 0.5 };
    // 色が読めない・範囲外は理由つきで断る
    let bad = ed.execute(SetChromaKey { clip: c, key: Some(ChromaKey { color: "green".into(), ..key.clone() }) }, Origin::Ai).unwrap_err();
    assert!(bad.to_string().contains("#00FF00"), "{bad}");
    assert!(ed.execute(SetChromaKey { clip: c, key: Some(ChromaKey { tolerance: 2.0, ..key.clone() }) }, Origin::Ai).is_err());
    // 音声トラックのクリップには付けられない
    let ac = ed.execute(AddClip { track: a1, asset: movie, start: s(0.0), duration: Some(s(1.0)), source_in: None, id: None }, Origin::User).unwrap().created[0];
    assert!(ed.execute(SetChromaKey { clip: ac, key: Some(key.clone()) }, Origin::User).is_err());

    ed.execute(SetChromaKey { clip: c, key: Some(key.clone()) }, Origin::User).unwrap();
    ed.execute(SetKey { clip: c, param: "hue".into(), time: s(0.0), value: 0.0, interp: Interp::Linear }, Origin::User).unwrap();
    ed.execute(SetKey { clip: c, param: "hue".into(), time: s(4.0), value: 180.0, interp: Interp::Linear }, Origin::User).unwrap();
    ed.execute(SetParam { clip: c, param: "blur".into(), value: 12.0 }, Origin::User).unwrap();
    assert!(ed.execute(SetParam { clip: c, param: "saturation".into(), value: 3.0 }, Origin::User).is_err(), "範囲外");
    let l = layers_at(&ed.project, s(2.0)).into_iter().find(|l| l.clip == c).unwrap();
    assert_eq!(l.key, Some(([0.0, 1.0, 0.0], 0.3, 0.1, 0.5)));
    assert!((l.fx.hue - 90.0).abs() < 1e-6 && l.fx.blur == 12.0);

    // 分割しても両方に残る
    ed.execute(SplitClip { clip: c, at: s(2.0), new_id: None }, Origin::User).unwrap();
    let keyed = ed.project.tracks.iter().flat_map(|t| &t.clips).filter(|x| x.key.is_some()).count();
    assert_eq!(keyed, 2);
    // 保存・読み込みで変わらない
    let j = serde_json::to_string(&ed.project).unwrap();
    let back: Project = serde_json::from_str(&j).unwrap();
    assert_eq!(back, ed.project);
    // 外して、取り消すと戻る
    ed.undo();
    ed.execute(SetChromaKey { clip: c, key: None }, Origin::User).unwrap();
    assert!(ed.project.clip(c).unwrap().key.is_none());
    ed.undo();
    assert_eq!(ed.project.clip(c).unwrap().key, Some(key));
}

#[test]
fn speed_rescales_clip_keys_and_source_and_undoes() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    let c = ed.execute(AddClip { track: v1, asset: movie, start: s(0.0), duration: Some(s(4.0)), source_in: Some(s(1.0)), id: None }, Origin::User).unwrap().created[0];
    ed.execute(SetKey { clip: c, param: "x".into(), time: s(2.0), value: 100.0, interp: Interp::Linear }, Origin::User).unwrap();
    ed.execute(SetFade { clip: c, fade_in: Some(s(1.0)), fade_out: None }, Origin::User).unwrap();
    let before = ed.project.clone();

    // 2 倍速: 長さは半分、キーとフェードも半分の時刻に。素材は同じ範囲(1〜5 秒)を使う
    ed.execute(SetSpeed { clip: c, speed: 2.0 }, Origin::User).unwrap();
    let cl = ed.project.clip(c).unwrap().clone();
    assert_eq!((cl.duration, cl.fade_in, cl.params.x.keys[0].time), (s(2.0), s(0.5), s(1.0)));
    let l = layers_at(&ed.project, s(1.5)).into_iter().find(|l| l.clip == c).unwrap();
    assert_eq!(l.source_time, s(4.0));
    // 0.5 倍速: 長さ 8 秒で、素材は同じ 4 秒分
    ed.execute(SetSpeed { clip: c, speed: 0.5 }, Origin::User).unwrap();
    assert_eq!((ed.project.clip(c).unwrap().duration, ed.project.clip(c).unwrap().source_len()), (s(8.0), s(4.0)));
    // 分割すると、右側の素材の位置も速度どおり
    let r = ed.execute(SplitClip { clip: c, at: s(2.0), new_id: None }, Origin::User).unwrap().created[0];
    assert_eq!(ed.project.clip(r).unwrap().source_in, s(2.0));
    assert_eq!(ed.project.clip(r).unwrap().speed, 0.5);
    ed.undo();
    // 範囲外の速度は断る。速くしたまま右端を伸ばして素材を超えるのも断る
    let e = ed.execute(SetSpeed { clip: c, speed: 20.0 }, Origin::Ai).unwrap_err().to_string();
    assert!(e.contains("速度"), "{e}");
    ed.execute(SetSpeed { clip: c, speed: 2.0 }, Origin::User).unwrap();
    let e = ed.execute(TrimClip { clip: c, start: None, duration: Some(s(5.0)), source_in: None }, Origin::Ai).unwrap_err().to_string();
    assert!(e.contains("2 倍"), "{e}");
    ed.undo();
    assert!(ed.execute(SetSpeed { clip: c, speed: 20.0 }, Origin::Ai).is_err());
    // 後ろのクリップと重なるなら断る
    ed.execute(AddClip { track: v1, asset: movie, start: s(9.0), duration: Some(s(1.0)), source_in: None, id: None }, Origin::User).unwrap();
    let e = ed.execute(SetSpeed { clip: c, speed: 0.4 }, Origin::Ai).unwrap_err().to_string();
    assert!(e.contains("重な"), "{e}");
    ed.undo();
    // 保存・読み込みで変わらない、取り消すと元どおり
    let back: Project = serde_json::from_str(&serde_json::to_string(&ed.project).unwrap()).unwrap();
    assert_eq!(back, ed.project);
    ed.undo();
    ed.undo();
    assert_eq!(ed.project.tracks, before.tracks);
    // 音の高さを保つのが既定(古いプロジェクトに項目が無くても)。切り替えて取り消せる
    assert!(ed.project.clip(c).unwrap().keep_pitch);
    ed.execute(SetKeepPitch { clip: c, keep_pitch: false }, Origin::User).unwrap();
    let j = serde_json::to_string(&ed.project).unwrap();
    assert!(j.contains("\"keep_pitch\":false"));
    ed.undo();
    assert!(!serde_json::to_string(&ed.project).unwrap().contains("keep_pitch"));
}

#[test]
fn reverse_maps_time_backwards_and_split_keeps_picture() {
    let (mut ed, v1, _v2, _a1, movie, _img) = setup();
    let c = ed.execute(AddClip { track: v1, asset: movie, start: s(1.0), duration: Some(s(4.0)), source_in: Some(s(2.0)), id: None }, Origin::User).unwrap().created[0];
    ed.execute(SetReverse { clip: c, reverse: true }, Origin::User).unwrap();
    let src = |p: &Project, t: f64| layers_at(p, s(t)).into_iter().next().unwrap().source_time;
    // 素材の 2〜6 秒を逆に: 始まりは 6 秒の直前、終わりは 2 秒付近
    assert!((src(&ed.project, 1.0).secs_f64() - 6.0).abs() < 1e-6 && src(&ed.project, 1.0) < s(6.0));
    assert!((src(&ed.project, 4.0).secs_f64() - 3.0).abs() < 1e-6);
    // 2 倍速と組み合わせ(長さ 2 秒): 1.5 秒目(クリップの 0.5 秒)は素材の 5 秒
    ed.execute(SetSpeed { clip: c, speed: 2.0 }, Origin::User).unwrap();
    assert!((src(&ed.project, 1.5).secs_f64() - 5.0).abs() < 1e-6);
    ed.undo();
    // 分割しても、どの時刻でも同じ素材の時刻が出る
    let before: Vec<Time> = (0..40).map(|i| src(&ed.project, 1.0 + i as f64 * 0.1)).collect();
    ed.execute(SplitClip { clip: c, at: s(2.5), new_id: None }, Origin::User).unwrap();
    let after: Vec<Time> = (0..40).map(|i| src(&ed.project, 1.0 + i as f64 * 0.1)).collect();
    assert_eq!(before, after);
    assert!(ed.project.tracks[0].clips.iter().all(|c| c.reverse));
    ed.undo();
    ed.execute(SetReverse { clip: c, reverse: false }, Origin::User).unwrap();
    assert_eq!(src(&ed.project, 1.0), s(2.0));
    ed.undo();
    assert!(ed.project.clip(c).unwrap().reverse);
    let back: Project = serde_json::from_str(&serde_json::to_string(&ed.project).unwrap()).unwrap();
    assert_eq!(back, ed.project);
}
