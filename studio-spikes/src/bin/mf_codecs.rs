//! 検証4: この PC の Media Foundation で使えるコーデック(デコーダ/エンコーダ)の一覧。

use windows::core::GUID;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

fn count(category: GUID, input: Option<GUID>, output: Option<GUID>, major: GUID, hw: bool) -> u32 {
    let inp = input.map(|s| MFT_REGISTER_TYPE_INFO { guidMajorType: major, guidSubtype: s });
    let out = output.map(|s| MFT_REGISTER_TYPE_INFO { guidMajorType: major, guidSubtype: s });
    let flags = if hw { MFT_ENUM_FLAG_HARDWARE } else { MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT };
    let mut acts: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut n = 0u32;
    unsafe {
        if MFTEnumEx(
            category,
            flags | MFT_ENUM_FLAG_SORTANDFILTER,
            inp.as_ref().map(|x| x as *const _),
            out.as_ref().map(|x| x as *const _),
            &mut acts,
            &mut n,
        )
        .is_err()
        {
            return 0;
        }
        for i in 0..n as usize {
            drop(std::ptr::read(acts.add(i)));
        }
        CoTaskMemFree(Some(acts as _));
    }
    n
}

fn main() {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
        MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
    }
    let video = [
        ("H.264", MFVideoFormat_H264),
        ("HEVC (H.265)", MFVideoFormat_HEVC),
        ("VP9", MFVideoFormat_VP90),
        ("AV1", MFVideoFormat_AV1),
        ("MPEG-2", MFVideoFormat_MPEG2),
        ("MJPEG", MFVideoFormat_MJPG),
    ];
    let audio = [
        ("AAC", MFAudioFormat_AAC),
        ("MP3", MFAudioFormat_MP3),
        ("FLAC", MFAudioFormat_FLAC),
        ("Opus", MFAudioFormat_Opus),
        ("ALAC", MFAudioFormat_ALAC),
    ];
    println!("{:<14} {:>10} {:>10} {:>10} {:>10}", "形式", "読込(HW)", "読込(SW)", "書出(HW)", "書出(SW)");
    for (name, g) in video {
        println!(
            "{:<14} {:>10} {:>10} {:>10} {:>10}",
            name,
            count(MFT_CATEGORY_VIDEO_DECODER, Some(g), None, MFMediaType_Video, true),
            count(MFT_CATEGORY_VIDEO_DECODER, Some(g), None, MFMediaType_Video, false),
            count(MFT_CATEGORY_VIDEO_ENCODER, None, Some(g), MFMediaType_Video, true),
            count(MFT_CATEGORY_VIDEO_ENCODER, None, Some(g), MFMediaType_Video, false),
        );
    }
    for (name, g) in audio {
        println!(
            "{:<14} {:>10} {:>10} {:>10} {:>10}",
            name,
            "-",
            count(MFT_CATEGORY_AUDIO_DECODER, Some(g), None, MFMediaType_Audio, false),
            "-",
            count(MFT_CATEGORY_AUDIO_ENCODER, None, Some(g), MFMediaType_Audio, false),
        );
    }
}
