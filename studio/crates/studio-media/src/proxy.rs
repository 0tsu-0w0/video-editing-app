//! プロキシ(編集中のプレビュー用の軽い代わりの動画)。
//!
//! 長い動画や高解像度の動画は、シーク(再生位置の移動)のたびに大きなフレームを復号するので重い。
//! そこで高さ 540 ピクセル・0.2 秒ごとにキーフレームの軽い動画を別に作り、プレビューだけそれを使う。
//! 書き出しは常に元の動画を使う。
//!
//! 置き場所: %LOCALAPPDATA%\ForgeStudio\proxies(環境変数 STUDIO_PROXY_DIR で変更可)。
//! ファイル名は元の動画のパス・大きさ・更新時刻から決めるので、元を差し替えると作り直しになる。

use crate::{probe, ExportSettings, Exporter, MediaError, VideoReader, R};
use std::path::{Path, PathBuf};
use studio_core::AssetKind;

/// プロキシの高さ(これ以下の動画は、大きさはそのままでキーフレームだけ増やす)
pub const PROXY_HEIGHT: u32 = 540;

pub fn proxy_dir() -> PathBuf {
    if let Ok(d) = std::env::var("STUDIO_PROXY_DIR") {
        return PathBuf::from(d);
    }
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into())).join("ForgeStudio").join("proxies")
}

/// 元の動画に対応するプロキシのパス(まだ無くてもよい)。元のファイルが無ければ None。
pub fn proxy_path(src: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(src).ok()?;
    let modified = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as u64);
    // FNV-1a(バージョンが変わっても同じ値になるよう自前で計算する)
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let key = format!("{}|{}|{}", std::fs::canonicalize(src).unwrap_or(src.to_path_buf()).to_string_lossy().to_lowercase(), meta.len(), modified);
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let stem: String = src.file_stem().map(|s| s.to_string_lossy().chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').take(40).collect()).unwrap_or_default();
    Some(proxy_dir().join(format!("{stem}_{h:016x}.mp4")))
}

/// 作成済みのプロキシ。
pub fn existing_proxy(src: &Path) -> Option<PathBuf> {
    proxy_path(src).filter(|p| p.is_file())
}

/// 元の大きさ (w, h) に対するプロキシの大きさ(縦横比を保ち、偶数)。
pub fn proxy_size(w: u32, h: u32) -> (u32, u32) {
    if h <= PROXY_HEIGHT {
        return (w / 2 * 2, h / 2 * 2);
    }
    let pw = ((w as f64 * PROXY_HEIGHT as f64 / h as f64).round() as u32 / 2 * 2).max(2);
    (pw, PROXY_HEIGHT)
}

/// 自動でプロキシを作るべき動画か(1080p を超える、または 2 分を超える)。
pub fn worth_proxy(kind: &AssetKind) -> bool {
    matches!(kind, AssetKind::Video { height, duration, .. } if *height > 1080 || duration.secs_f64() > 120.0)
}

/// 面積の平均で縮小する(BGRA)。
fn shrink(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32, out: &mut [u8]) {
    let (fx, fy) = (sw as f64 / dw as f64, sh as f64 / dh as f64);
    for y in 0..dh {
        let (y0, y1) = ((y as f64 * fy) as u32, (((y + 1) as f64 * fy).ceil() as u32).min(sh).max((y as f64 * fy) as u32 + 1));
        for x in 0..dw {
            let (x0, x1) = ((x as f64 * fx) as u32, (((x + 1) as f64 * fx).ceil() as u32).min(sw).max((x as f64 * fx) as u32 + 1));
            let mut acc = [0u32; 3];
            for yy in y0..y1 {
                let row = (yy * sw) as usize * 4;
                for xx in x0..x1 {
                    let i = row + xx as usize * 4;
                    acc[0] += src[i] as u32;
                    acc[1] += src[i + 1] as u32;
                    acc[2] += src[i + 2] as u32;
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let o = ((y * dw + x) * 4) as usize;
            out[o] = (acc[0] / n) as u8;
            out[o + 1] = (acc[1] / n) as u8;
            out[o + 2] = (acc[2] / n) as u8;
            out[o + 3] = 255;
        }
    }
}

/// プロキシを作る。`progress(済んだ割合)` が false を返したら中止する。作ったファイルのパスを返す。
pub fn make_proxy(src: &Path, mut progress: impl FnMut(f64) -> bool) -> R<PathBuf> {
    let AssetKind::Video { width, height, frame_rate, duration, .. } = probe(src)? else {
        return Err(MediaError::Unsupported("プロキシを作れるのは動画だけです".into()));
    };
    let out = proxy_path(src).ok_or_else(|| MediaError::Open(format!("{} を読めません", src.display())))?;
    std::fs::create_dir_all(proxy_dir()).map_err(|e| MediaError::Write(e.to_string()))?;
    let tmp = out.with_extension("making.mp4");
    let (pw, ph) = proxy_size(width, height);
    let total = frame_rate.time_to_frame(duration).max(1) as f64;
    let result = (|| {
        let mut reader = VideoReader::open(src)?;
        // 0.2 秒ごとにキーフレーム。画素数に合わせた軽いビットレート
        let gop = (frame_rate.fps() / 5.0).round().max(1.0) as u32;
        let bitrate = ((pw * ph) as f64 * frame_rate.fps() * 0.12).clamp(500_000.0, 6_000_000.0) as u32;
        let mut ex = Exporter::create_with_keyframes(
            &tmp,
            &ExportSettings { width: pw, height: ph, frame_rate, video_bitrate: bitrate, sample_rate: None, audio_bitrate: 0 },
            Some(gop),
        )?;
        let mut small = vec![0u8; (pw * ph * 4) as usize];
        let mut n = 0u64;
        while let Some(f) = reader.next_frame()? {
            if (f.width, f.height) == (pw, ph) {
                ex.write_frame(&f.bgra)?;
            } else {
                shrink(&f.bgra, f.width, f.height, pw, ph, &mut small);
                ex.write_frame(&small)?;
            }
            n += 1;
            if n % 15 == 0 && !progress((n as f64 / total).min(1.0)) {
                return Err(MediaError::Write("中止しました".into()));
            }
        }
        ex.finish()
    })();
    if let Err(e) = result {
        std::fs::remove_file(&tmp).ok();
        return Err(e);
    }
    std::fs::rename(&tmp, &out).map_err(|e| MediaError::Write(e.to_string()))?;
    progress(1.0);
    Ok(out)
}
