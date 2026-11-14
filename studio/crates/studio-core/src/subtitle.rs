//! 字幕ファイル(SRT・WebVTT)の読み書きと、文字のクリップへの変換。

use crate::command::Command;
use crate::project::{AssetKind, Id, Project, TextStyle, TrackKind};
use crate::time::Time;

/// 字幕 1 つ分。
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: Time,
    pub end: Time,
    pub text: String,
}

/// "00:01:02,345" / "00:01:02.345" / "01:02.345"(WebVTT の省略形)を読む。
fn parse_stamp(s: &str) -> Option<Time> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let (h, m, sec) = match parts[..] {
        [h, m, sec] => (h.parse::<f64>().ok()?, m.parse::<f64>().ok()?, sec.parse::<f64>().ok()?),
        [m, sec] => (0.0, m.parse::<f64>().ok()?, sec.parse::<f64>().ok()?),
        _ => return None,
    };
    if !(0.0..60.0).contains(&m) || !(0.0..60.0).contains(&sec) || h < 0.0 {
        return None;
    }
    Some(Time::from_secs_f64(h * 3600.0 + m * 60.0 + sec))
}

/// "<i>" などのタグを取り除く。
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ")
}

/// SRT か WebVTT を読む。読めない時刻の行があれば、その行番号を返す。
pub fn parse(src: &str) -> Result<Vec<Cue>, String> {
    let src = src.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let mut cues = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some((a, b)) = line.split_once("-->") {
            // WebVTT は時刻の後ろに配置の指定が付くことがある
            let b = b.split_whitespace().next().unwrap_or("");
            let (Some(start), Some(end)) = (parse_stamp(a), parse_stamp(b)) else {
                return Err(format!("{} 行目の時刻が読めません: {line}", i + 1));
            };
            i += 1;
            let mut text = vec![];
            while i < lines.len() && !lines[i].trim().is_empty() {
                text.push(strip_tags(lines[i].trim()));
                i += 1;
            }
            if end > start && !text.is_empty() {
                cues.push(Cue { start, end, text: text.join("\n") });
            }
        } else {
            i += 1;
        }
    }
    if cues.is_empty() {
        return Err("字幕が 1 つも見つかりません(SRT か WebVTT の形式か確認してください)".into());
    }
    cues.sort_by_key(|c| c.start);
    Ok(cues)
}

fn stamp(t: Time) -> String {
    let ms = (t.secs_f64() * 1000.0).round().max(0.0) as i64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// SRT の文字列にする。
pub fn to_srt(cues: &[Cue]) -> String {
    let mut s = String::new();
    for (i, c) in cues.iter().enumerate() {
        s.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, stamp(c.start), stamp(c.end), c.text));
    }
    s
}

/// プロジェクトの文字のクリップを字幕にする(`track` を指定すればそのトラックだけ)。時刻の順に並べる。
pub fn cues_of(p: &Project, track: Option<Id>) -> Vec<Cue> {
    let mut cues: Vec<Cue> = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video && track.is_none_or(|id| id == t.id))
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| match p.assets.get(&c.asset).map(|a| &a.kind) {
            Some(AssetKind::Text { text, .. }) => Some(Cue { start: c.start, end: c.end(), text: text.clone() }),
            _ => None,
        })
        .collect();
    cues.sort_by_key(|c| (c.start, c.end));
    cues
}

/// 字幕を、新しいトラックの文字のクリップとして置くコマンド(画面の下寄せ、白文字に黒の縁取り)。
/// 時間が重なる字幕は、重ならないように別のトラックに分ける。
pub fn import_commands(p: &Project, cues: &[Cue]) -> Vec<Command> {
    let (w, h) = (p.settings.width as f64, p.settings.height as f64);
    let size = (h / 15.0).round().clamp(12.0, 400.0);
    let style = TextStyle { size: size as f32, outline: (size as f32 / 12.0).round().max(2.0), ..Default::default() };
    let y = (h / 2.0 - size * 1.6).round();
    let mut next = p.next_id;
    let mut id = || {
        let i = Id(next);
        next += 1;
        i
    };
    let mut cmds = vec![];
    // 重ならないよう、トラック(段)ごとの最後の終わりを覚えておく
    let mut lanes: Vec<(Id, Time)> = vec![];
    for c in cues {
        let lane = match lanes.iter().position(|(_, end)| *end <= c.start) {
            Some(i) => i,
            None => {
                let t = id();
                let name = if lanes.is_empty() { "字幕".to_string() } else { format!("字幕{}", lanes.len() + 1) };
                cmds.push(Command::AddTrack { kind: TrackKind::Video, name: Some(name), index: None, id: Some(t) });
                lanes.push((t, Time::ZERO));
                lanes.len() - 1
            }
        };
        lanes[lane].1 = c.end;
        let (asset, clip) = (id(), id());
        let first_line: String = c.text.lines().next().unwrap_or("").chars().take(12).collect();
        cmds.push(Command::ImportAsset {
            name: format!("字幕「{first_line}」"),
            path: String::new(),
            kind: AssetKind::Text { text: c.text.clone(), style: style.clone() },
            id: Some(asset),
        });
        cmds.push(Command::AddClip { track: lanes[lane].0, asset, start: c.start, duration: Some(c.end - c.start), source_in: None, id: Some(clip) });
        // 複数行は、下の行が同じ高さに来るよう上へずらす
        let extra = (c.text.lines().count().max(1) - 1) as f64 * size * 0.6;
        cmds.push(Command::SetParam { clip, param: "y".into(), value: (y - extra).round() });
        let _ = w;
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Editor, Origin};

    const SRT: &str = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\nこんにちは\r\n<i>世界</i>\r\n\r\n2\r\n00:00:02,000 --> 00:00:03,000\r\n重なる字幕\r\n\r\n3\r\n00:00:04,250 --> 00:00:05,000\r\nA &amp; B\r\n";

    #[test]
    fn parse_srt_and_vtt() {
        let c = parse(SRT).unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].start, c[0].end, c[0].text.as_str()), (Time::from_secs_f64(1.0), Time::from_secs_f64(2.5), "こんにちは\n世界"));
        assert_eq!(c[2].text, "A & B");
        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.000 align:center\nhello\n\nnote\n01:00:00.000 --> 01:00:01.500\nlate\n";
        let v = parse(vtt).unwrap();
        assert_eq!((v[0].start, v[1].start, v[1].end), (Time::from_secs_f64(1.0), Time::from_secs_f64(3600.0), Time::from_secs_f64(3601.5)));
        assert!(parse("1\n00:00:01,000 --> 99:99\nx\n").unwrap_err().contains("2 行目"));
        assert!(parse("なにもない").is_err());
    }

    #[test]
    fn import_places_text_clips_and_export_roundtrips() {
        let mut ed = Editor::new(Project::default());
        let cues = parse(SRT).unwrap();
        let o = ed.transaction("字幕", import_commands(&ed.project, &cues), Origin::User).unwrap();
        assert!(!o.created.is_empty());
        // 重なる 2 つ目は別のトラック
        assert_eq!(ed.project.tracks.len(), 2);
        assert_eq!(ed.project.tracks[0].clips.len(), 2);
        let back = cues_of(&ed.project, None);
        assert_eq!(back, cues);
        let srt = to_srt(&back);
        assert!(srt.starts_with("1\n00:00:01,000 --> 00:00:02,500\nこんにちは\n世界\n\n2\n00:00:02,000"));
        assert_eq!(parse(&srt).unwrap(), cues);
        // 1 回の取り消しで全部消える
        ed.undo();
        assert!(ed.project.tracks.is_empty() && ed.project.assets.is_empty());
    }
}
