//! AI(MCP・言葉での編集)に渡す情報: コマンドの説明と、プロジェクトの要約。
//! 時刻はすべて秒(小数)で表す。

use crate::project::{AssetKind, Project, TrackKind, PARAMS};
use serde_json::{json, Value};

/// 使えるコマンドの一覧と引数の説明。
pub fn command_schema() -> Value {
    let time = "時刻(秒の数値、または \"00:01:02.500\" 形式の文字列)";
    json!({
        "notes": [
            "ID はプロジェクトの要約(get_project)に出ている数値を使う。",
            "時刻はすべて秒。クリップの start はタイムライン上、キーフレームの time はクリップの開始からの時刻。",
            "同じトラックのクリップは重ねられない。重なると overlap エラーになる。",
            "複数のコマンドは 1 回の Undo で戻せる 1 つの操作としてまとめて実行される。1 つでも失敗すると全て取り消される。",
            "映像トラックは、tracks の後ろのものほど手前に重なる。",
            "位置 x, y はピクセル、画面中央が原点、y は下向き。",
        ],
        "params": PARAMS.iter().map(|(n, lo, hi)| json!({"name": n, "min": lo, "max": hi})).collect::<Vec<_>>(),
        "commands": [
            {"command": "add_track", "description": "トラックを追加する", "args": {
                "kind": "\"video\" または \"audio\"", "name": "(任意)名前", "index": "(任意)挿入位置。0 が最も奥。省略で最も手前"}},
            {"command": "remove_track", "description": "トラックを中のクリップごと削除する", "args": {"track": "トラック ID"}},
            {"command": "rename_track", "args": {"track": "トラック ID", "name": "新しい名前"}},
            {"command": "move_track", "description": "トラックの並び順を変える", "args": {"track": "トラック ID", "index": "移動先の位置"}},
            {"command": "set_track_flags", "args": {"track": "トラック ID", "muted": "(任意)true/false", "locked": "(任意)true/false"}},
            {"command": "add_clip", "description": "素材をトラックに置く", "args": {
                "track": "トラック ID", "asset": "素材 ID", "start": time,
                "duration": format!("(任意){time}。省略で素材の終わりまで(画像は 5 秒)"),
                "source_in": format!("(任意){time}。素材のどこから使うか")}},
            {"command": "remove_clip", "args": {"clip": "クリップ ID"}},
            {"command": "move_clip", "args": {"clip": "クリップ ID", "start": time, "track": "(任意)移動先のトラック ID(同じ種類のみ)"}},
            {"command": "trim_clip", "description": "開始・長さ・素材の使用開始位置を直接指定する。省略した項目は変えない", "args": {
                "clip": "クリップ ID", "start": "(任意)", "duration": "(任意)", "source_in": "(任意)"}},
            {"command": "split_clip", "description": "タイムライン上の時刻でクリップを 2 つに分ける", "args": {"clip": "クリップ ID", "at": time}},
            {"command": "set_param", "description": "値を定数にする(その項目のキーフレームは消える)", "args": {
                "clip": "クリップ ID", "param": "params の name のいずれか", "value": "数値"}},
            {"command": "set_key", "description": "キーフレームを置く", "args": {
                "clip": "クリップ ID", "param": "params の name", "time": "クリップの開始からの時刻(秒)", "value": "数値",
                "interp": "(任意)次のキーまでの補間: linear / step / ease_in / ease_out / ease_in_out"}},
            {"command": "remove_key", "args": {"clip": "クリップ ID", "param": "params の name", "time": "キーの時刻(秒)"}},
            {"command": "remove_asset", "description": "素材をプロジェクトから外す(ファイルは消さない)。使われている素材は外せない", "args": {"asset": "素材 ID"}},
            {"command": "set_settings", "description": "プロジェクトの解像度などを変える", "args": {
                "width": "(任意)", "height": "(任意)", "frame_rate": "(任意){\"num\":30,\"den\":1}", "sample_rate": "(任意)44100/48000/96000"}},
        ],
        "example": [
            {"command": "add_clip", "track": 2, "asset": 5, "start": 3.0, "duration": 4.0},
            {"command": "set_key", "clip": 9, "param": "opacity", "time": 0, "value": 0, "interp": "ease_out"},
            {"command": "set_key", "clip": 9, "param": "opacity", "time": 0.5, "value": 1},
        ],
    })
}

fn r3(t: crate::time::Time) -> f64 {
    (t.secs_f64() * 1000.0).round() / 1000.0
}

/// プロジェクトの要約(AI が読む形)。キーフレームがある項目だけキーを載せる。
pub fn describe(p: &Project) -> Value {
    let assets: Vec<Value> = p
        .assets
        .values()
        .map(|a| {
            let mut v = json!({"id": a.id.0, "name": a.name, "path": a.path});
            match &a.kind {
                AssetKind::Video { width, height, frame_rate, duration, has_audio } => {
                    v["type"] = json!("video");
                    v["width"] = json!(width);
                    v["height"] = json!(height);
                    v["fps"] = json!((frame_rate.fps() * 1000.0).round() / 1000.0);
                    v["duration"] = json!(r3(*duration));
                    v["has_audio"] = json!(has_audio);
                }
                AssetKind::Audio { duration, .. } => {
                    v["type"] = json!("audio");
                    v["duration"] = json!(r3(*duration));
                }
                AssetKind::Image { width, height } => {
                    v["type"] = json!("image");
                    v["width"] = json!(width);
                    v["height"] = json!(height);
                }
            }
            v
        })
        .collect();
    let tracks: Vec<Value> = p
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let clips: Vec<Value> = t
                .clips
                .iter()
                .map(|c| {
                    let mut params = serde_json::Map::new();
                    for (name, _, _) in PARAMS {
                        let pr = c.params.get(name).unwrap();
                        let relevant = match t.kind {
                            TrackKind::Video => !matches!(name, "volume" | "pan"),
                            TrackKind::Audio => matches!(name, "volume" | "pan"),
                        };
                        if !relevant {
                            continue;
                        }
                        if pr.keys.is_empty() {
                            params.insert(name.into(), json!(pr.value));
                        } else {
                            params.insert(
                                name.into(),
                                json!({"keys": pr.keys.iter().map(|k| json!({"time": r3(k.time), "value": k.value, "interp": k.interp})).collect::<Vec<_>>()}),
                            );
                        }
                    }
                    json!({
                        "id": c.id.0, "asset": c.asset.0,
                        "start": r3(c.start), "end": r3(c.end()), "duration": r3(c.duration), "source_in": r3(c.source_in),
                        "params": params,
                    })
                })
                .collect();
            json!({"id": t.id.0, "index": i, "kind": t.kind, "name": t.name, "muted": t.muted, "locked": t.locked, "clips": clips})
        })
        .collect();
    let s = &p.settings;
    json!({
        "settings": {"width": s.width, "height": s.height, "fps": (s.frame_rate.fps() * 1000.0).round() / 1000.0,
                     "frame_rate": s.frame_rate, "sample_rate": s.sample_rate},
        "duration": r3(p.duration()),
        "assets": assets,
        "tracks": tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;

    #[test]
    fn schema_examples_parse_and_cover_all_public_commands() {
        let s = command_schema();
        for ex in s["example"].as_array().unwrap() {
            serde_json::from_value::<Command>(ex.clone()).unwrap();
        }
        let names: Vec<&str> = s["commands"].as_array().unwrap().iter().map(|c| c["command"].as_str().unwrap()).collect();
        for n in ["add_track", "remove_track", "rename_track", "move_track", "set_track_flags", "add_clip", "remove_clip", "move_clip", "trim_clip", "split_clip", "set_param", "set_key", "remove_key", "remove_asset", "set_settings"] {
            assert!(names.contains(&n), "{n} の説明がない");
        }
        assert!(!names.iter().any(|n| n.starts_with("restore")), "内部用コマンドを公開しない");
    }

    #[test]
    fn describe_uses_seconds_and_ids() {
        let mut p = Project::default();
        let mut ed = crate::Editor::new(p.clone());
        let o = ed
            .transaction(
                "t",
                vec![
                    Command::AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                    Command::ImportAsset { name: "a.png".into(), path: "C:/a.png".into(), kind: AssetKind::Image { width: 10, height: 20 }, id: None },
                ],
                crate::Origin::User,
            )
            .unwrap();
        ed.execute(
            Command::AddClip { track: o.created[0], asset: o.created[1], start: crate::Time::from_secs_f64(1.5), duration: None, source_in: None, id: None },
            crate::Origin::User,
        )
        .unwrap();
        p = ed.project.clone();
        let d = describe(&p);
        assert_eq!(d["tracks"][0]["clips"][0]["start"], json!(1.5));
        assert_eq!(d["tracks"][0]["clips"][0]["end"], json!(6.5));
        assert_eq!(d["assets"][0]["type"], json!("image"));
        assert!(d["tracks"][0]["clips"][0]["params"].get("volume").is_none());
    }
}
