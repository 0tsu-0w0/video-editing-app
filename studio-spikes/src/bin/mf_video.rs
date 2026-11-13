//! 検証3: Windows 標準の Media Foundation だけで、動画の書き出し・読み込み・シーク・音ズレ確認ができるか。
//!
//! 1. 1920x1080 / 30fps / 20 秒の H.264 + AAC の MP4 を書き出す(ハードウェアエンコーダを優先)。
//!    各フレームの左上に「フレーム番号」を白黒のブロックで描き、1 秒ごとに白フラッシュと
//!    同時に 1kHz のビープ音を入れる(音ズレ測定用)。
//! 2. D3D11 のハードウェアデコードで全フレームを読み、速度を測る。
//! 3. ランダムな位置へのシークを繰り返し、最初のフレームが出るまでの時間を測る。
//! 4. CPU で RGB に変換しながら読み、フレーム番号が正しいか(取りこぼし・重複がないか)を確かめる。
//! 5. 音声を読み、ビープの開始時刻と白フラッシュの時刻の差(音ズレ)を測る。

use std::time::Instant;
use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 30;
const SECONDS: u32 = 20;
const RATE: u32 = 48000;
const FRAME_HNS: i64 = 10_000_000 / FPS as i64;
const BLOCK: u32 = 40; // フレーム番号を表すブロックの大きさ
const BITS: u32 = 12;

type R<T> = windows::core::Result<T>;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn is_flash(i: u32) -> bool {
    i % FPS == 0
}

/// フレーム i の NV12 画像を作る。
fn nv12_frame(i: u32, buf: &mut [u8]) {
    let (y_plane, uv) = buf.split_at_mut((W * H) as usize);
    let base = if is_flash(i) { 235u8 } else { 60 };
    for y in 0..H {
        let row = &mut y_plane[(y * W) as usize..((y + 1) * W) as usize];
        for x in 0..W {
            row[x as usize] = if is_flash(i) { base } else { (base as u32 + (x + i * 8) % 120) as u8 };
        }
    }
    // フレーム番号(下位ビットから順に、左上へ横並び)
    for b in 0..BITS {
        let v = if (i >> b) & 1 == 1 { 235 } else { 16 };
        for y in 0..BLOCK {
            for x in 0..BLOCK {
                y_plane[(y * W + b * BLOCK + x) as usize] = v;
            }
        }
    }
    uv.fill(128);
}

/// 1 フレーム分(1600 標本)のステレオ PCM16。フラッシュのフレームの間だけ 1kHz のビープ。
fn pcm_frame(i: u32, out: &mut Vec<u8>) {
    out.clear();
    let n = RATE / FPS;
    for k in 0..n {
        let t = (i * n + k) as f32 / RATE as f32;
        let v = if is_flash(i) { (t * 1000.0 * std::f32::consts::TAU).sin() * 0.5 } else { 0.0 };
        let s = (v * 32767.0) as i16;
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
    }
}

fn video_type(subtype: &GUID) -> R<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(W, H))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(FPS, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(t)
    }
}

fn audio_type(subtype: &GUID, avg_bytes: u32) -> R<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, avg_bytes)?;
        if *subtype == MFAudioFormat_PCM {
            t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
        }
        Ok(t)
    }
}

fn sample_from(data: &[u8], time: i64, dur: i64) -> R<IMFSample> {
    unsafe {
        let buf = MFCreateMemoryBuffer(data.len() as u32)?;
        let mut p = std::ptr::null_mut();
        buf.Lock(&mut p, None, None)?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        buf.Unlock()?;
        buf.SetCurrentLength(data.len() as u32)?;
        let s = MFCreateSample()?;
        s.AddBuffer(&buf)?;
        s.SetSampleTime(time)?;
        s.SetSampleDuration(dur)?;
        Ok(s)
    }
}

fn encode(path: &str) -> R<f64> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs = attrs.unwrap();
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        let p = wide(path);
        let w = MFCreateSinkWriterFromURL(PCWSTR(p.as_ptr()), None, &attrs)?;

        let vout = video_type(&MFVideoFormat_H264)?;
        vout.SetUINT32(&MF_MT_AVG_BITRATE, 12_000_000)?;
        let vs = w.AddStream(&vout)?;
        w.SetInputMediaType(vs, &video_type(&MFVideoFormat_NV12)?, None)?;
        let aout = audio_type(&MFAudioFormat_AAC, 24_000)?;
        let as_ = w.AddStream(&aout)?;
        w.SetInputMediaType(as_, &audio_type(&MFAudioFormat_PCM, RATE * 4)?, None)?;
        w.BeginWriting()?;

        let mut frame = vec![0u8; (W * H * 3 / 2) as usize];
        let mut pcm = Vec::new();
        let t = Instant::now();
        for i in 0..FPS * SECONDS {
            nv12_frame(i, &mut frame);
            w.WriteSample(vs, &sample_from(&frame, i as i64 * FRAME_HNS, FRAME_HNS)?)?;
            pcm_frame(i, &mut pcm);
            w.WriteSample(as_, &sample_from(&pcm, i as i64 * FRAME_HNS, FRAME_HNS)?)?;
        }
        w.Finalize()?;
        Ok(t.elapsed().as_secs_f64())
    }
}

fn d3d_manager() -> R<IMFDXGIDeviceManager> {
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device = device.unwrap();
        let mt: ID3D11Multithread = device.cast()?;
        let _ = mt.SetMultithreadProtected(true);
        let mut token = 0u32;
        let mut mgr = None;
        MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
        let mgr = mgr.unwrap();
        mgr.ResetDevice(&device, token)?;
        Ok(mgr)
    }
}

const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const FIRST_AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
const END: u32 = MF_SOURCE_READERF_ENDOFSTREAM.0 as u32;

fn reader(path: &str, hardware: bool, video_subtype: &GUID) -> R<IMFSourceReader> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 4)?;
        let attrs = attrs.unwrap();
        if hardware {
            attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &d3d_manager()?)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        } else {
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        }
        let p = wide(path);
        let r = MFCreateSourceReaderFromURL(PCWSTR(p.as_ptr()), &attrs)?;
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, video_subtype)?;
        r.SetCurrentMediaType(FIRST_VIDEO, None, &t)?;
        Ok(r)
    }
}

fn read_video(r: &IMFSourceReader) -> R<Option<(i64, IMFSample)>> {
    unsafe {
        loop {
            let (mut flags, mut ts, mut s) = (0u32, 0i64, None);
            r.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut s))?;
            if flags & END != 0 {
                return Ok(None);
            }
            if let Some(s) = s {
                return Ok(Some((ts, s)));
            }
        }
    }
}

fn seek(r: &IMFSourceReader, hns: i64) -> R<()> {
    let pv = PROPVARIANT::from(hns);
    unsafe { r.SetCurrentPosition(&GUID::zeroed(), &pv) }
}

/// RGB32(BGRA)のフレームからフレーム番号を読み取る。
/// 行の幅とフレームの高さは、読み取り側の現在の形式から取る
/// (H.264 は高さ 1088 に揃えられることがあり、負のストライド=上下反転もありうる)。
fn decode_index(s: &IMFSample, stride: i32, height: u32) -> R<u32> {
    unsafe {
        let buf = s.ConvertToContiguousBuffer()?;
        let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
        buf.Lock(&mut p, None, Some(&mut len))?;
        let abs = stride.unsigned_abs() as usize;
        let row = |y: usize| if stride >= 0 { y * abs } else { (height as usize - 1 - y) * abs };
        let mut v = 0;
        for b in 0..BITS {
            let (x, y) = ((b * BLOCK + BLOCK / 2) as usize, (BLOCK / 2) as usize);
            let i = row(y) + x * 4 + 1;
            if i < len as usize && *p.add(i) > 128 {
                v |= 1 << b;
            }
        }
        buf.Unlock()?;
        Ok(v)
    }
}

fn stride_and_height(r: &IMFSourceReader) -> R<(i32, u32)> {
    unsafe {
        let t = r.GetCurrentMediaType(FIRST_VIDEO)?;
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((size >> 32) as u32, size as u32);
        let stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|v| v as i32).unwrap_or((w * 4) as i32);
        Ok((stride, h))
    }
}

fn main() -> R<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    let dir = std::env::temp_dir().join("studio-spike");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("av_test.mp4");
    let path = path.to_str().unwrap();
    let total = FPS * SECONDS;

    // 1. 書き出し
    let enc = encode(path)?;
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    println!(
        "[書き出し] {}x{} {}fps {}秒 H.264+AAC: {:.2} 秒(実時間の {:.1} 倍速)、{:.1} MB",
        W, H, FPS, SECONDS, enc, SECONDS as f64 / enc, size as f64 / 1e6
    );

    // 2. ハードウェアデコード速度
    let r = reader(path, true, &MFVideoFormat_NV12)?;
    let t = Instant::now();
    let mut n = 0;
    while read_video(&r)?.is_some() {
        n += 1;
    }
    let dt = t.elapsed().as_secs_f64();
    println!("[読み込み・GPU] {} フレーム / {:.2} 秒 = {:.0} fps", n, dt, n as f64 / dt);

    // 3. シーク(キーフレーム位置への移動 + 目的のフレームまで読み進める)
    let mut lat = Vec::new();
    let mut s: u32 = 7;
    for _ in 0..20 {
        s = s.wrapping_mul(1103515245).wrapping_add(12345);
        let target = (s >> 8) % total;
        let t = Instant::now();
        seek(&r, target as i64 * FRAME_HNS)?;
        // 目的時刻の直前のキーフレームから始まるので、目的のフレームまで捨てる
        while let Some((ts, _)) = read_video(&r)? {
            if ts + FRAME_HNS / 2 >= target as i64 * FRAME_HNS {
                break;
            }
        }
        lat.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "[シーク・GPU] 20 回: 中央値 {:.1} ms / 最大 {:.1} ms(目的のフレームが出るまで)",
        lat[lat.len() / 2],
        lat.last().unwrap()
    );

    // 4. 中身の確認(CPU で RGB にして、フレーム番号を読む)
    let r = reader(path, false, &MFVideoFormat_RGB32)?;
    let (stride, height) = stride_and_height(&r)?;
    println!("  (RGB 出力: ストライド {stride}、高さ {height})");
    let (mut ok, mut bad, mut count) = (0, 0, 0);
    let mut flash_times = Vec::new();
    while let Some((ts, sample)) = read_video(&r)? {
        let expect = ((ts + FRAME_HNS / 2) / FRAME_HNS) as u32;
        let got = decode_index(&sample, stride, height)?;
        if got == expect {
            ok += 1;
        } else {
            bad += 1;
            if bad <= 3 {
                println!("  不一致: 時刻 {} → 期待 {} / 読み取り {}", ts, expect, got);
            }
        }
        if is_flash(got) {
            flash_times.push(ts);
        }
        count += 1;
    }
    println!("[中身] {} フレーム中 {} 一致 / {} 不一致(取りこぼし {})", count, ok, bad, total as i64 - count as i64);

    // 5. 音ズレ: 音声を PCM で読み、ビープの立ち上がり時刻をフラッシュの時刻と比べる
    let r = reader(path, false, &MFVideoFormat_RGB32)?;
    unsafe {
        r.SetStreamSelection(FIRST_VIDEO, false)?;
        r.SetStreamSelection(FIRST_AUDIO, true)?;
        r.SetCurrentMediaType(FIRST_AUDIO, None, &audio_type(&MFAudioFormat_PCM, RATE * 4)?)?;
    }
    // 1ms(48 標本)ごとの最大振幅で、無音→音ありに変わった時刻を立ち上がりとする
    let mut onsets = Vec::new();
    let mut sample_index: i64 = -1;
    let mut quiet_ms = 1000;
    let (mut win_peak, mut win_n) = (0u16, 0);
    loop {
        let (mut flags, mut ts, mut smp) = (0u32, 0i64, None);
        unsafe { r.ReadSample(FIRST_AUDIO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut smp))? };
        if flags & END != 0 {
            break;
        }
        let Some(smp) = smp else { continue };
        if sample_index < 0 {
            sample_index = ts * RATE as i64 / 10_000_000;
        }
        unsafe {
            let buf = smp.ConvertToContiguousBuffer()?;
            let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
            buf.Lock(&mut p, None, Some(&mut len))?;
            let d = std::slice::from_raw_parts(p as *const i16, len as usize / 2);
            for fr in d.chunks_exact(2) {
                win_peak = win_peak.max(fr[0].unsigned_abs());
                win_n += 1;
                sample_index += 1;
                if win_n == 48 {
                    if win_peak > 3000 {
                        if quiet_ms >= 10 {
                            // 窓の先頭の時刻
                            onsets.push((sample_index - 48) * 10_000_000 / RATE as i64);
                        }
                        quiet_ms = 0;
                    } else if win_peak < 300 {
                        quiet_ms += 1;
                    }
                    win_peak = 0;
                    win_n = 0;
                }
            }
            buf.Unlock()?;
        }
    }
    // フラッシュ1回につき最初の立ち上がりだけを残す
    let mut beeps: Vec<i64> = Vec::new();
    for o in onsets {
        if beeps.last().map_or(true, |l| o - l > 5_000_000) {
            beeps.push(o);
        }
    }
    let diffs: Vec<f64> = flash_times
        .iter()
        .zip(&beeps)
        .map(|(v, a)| (*a - *v) as f64 / 10_000.0)
        .collect();
    if diffs.is_empty() {
        println!("[音ズレ] ビープを検出できませんでした");
    } else {
        let max = diffs.iter().cloned().fold(0.0f64, |m, d| m.max(d.abs()));
        let avg = diffs.iter().sum::<f64>() / diffs.len() as f64;
        println!(
            "[音ズレ] フラッシュ {} 回 / ビープ {} 回: 音−映像 平均 {:+.1} ms、最大 {:.1} ms(1 フレーム = {:.1} ms)",
            flash_times.len(),
            beeps.len(),
            avg,
            max,
            1000.0 / FPS as f64
        );
    }
    println!("ファイル: {path}");
    Ok(())
}
