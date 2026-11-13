//! `.au2fx`: エフェクトの保存形式(ZIP コンテナ)。
//!
//! 中身は spec.json と manifest.json だけ。Lua も .anm2 も含めない
//! (.anm2 は読み込み後に必ず Forge が再生成する)。

use crate::spec::{EffectSpec, Issue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Write};

pub const FORMAT_MAJOR: u32 = 1;
pub const FORMAT_MINOR: u32 = 0;
const MAX_ENTRIES: usize = 16;
const MAX_ENTRY_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub format_version: String,
    pub name: String,
    pub forge_version: String,
    pub prompt_version: String,
    pub spec_sha256: String,
}

fn err(m: impl Into<String>) -> Issue {
    Issue { code: "E_AU2FX", message: m.into() }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// EffectSpec を `.au2fx` のバイト列にする。
pub fn pack(spec: &EffectSpec) -> Result<Vec<u8>, Issue> {
    let spec_json = serde_json::to_vec_pretty(spec).map_err(|e| err(e.to_string()))?;
    let manifest = Manifest {
        format_version: format!("{FORMAT_MAJOR}.{FORMAT_MINOR}"),
        name: spec.meta.name.clone(),
        forge_version: env!("CARGO_PKG_VERSION").to_string(),
        prompt_version: crate::prompt::PROMPT_VERSION.to_string(),
        spec_sha256: sha256_hex(&spec_json),
    };
    let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|e| err(e.to_string()))?;
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in [("manifest.json", &manifest_json), ("spec.json", &spec_json)] {
        zw.start_file(name, opt).map_err(|e| err(e.to_string()))?;
        zw.write_all(data).map_err(|e| err(e.to_string()))?;
    }
    Ok(zw.finish().map_err(|e| err(e.to_string()))?.into_inner())
}

fn read_entry<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Vec<u8>, Issue> {
    let f = zip.by_name(name).map_err(|_| err(format!("{name} がありません")))?;
    if f.size() > MAX_ENTRY_BYTES {
        return Err(err(format!("{name} が大きすぎます")));
    }
    // 宣言サイズを信用せず、読み出し側でも上限をかける(zip bomb 対策)。
    let mut buf = Vec::new();
    f.take(MAX_ENTRY_BYTES + 1).read_to_end(&mut buf).map_err(|e| err(e.to_string()))?;
    if buf.len() as u64 > MAX_ENTRY_BYTES {
        return Err(err(format!("{name} が大きすぎます")));
    }
    Ok(buf)
}

/// `.au2fx` を読み、整合性を確かめて EffectSpec を返す。
/// ファイルは展開しない(名前で2つのエントリだけを読む)ので、zip slip は起きない。
/// 返した spec は信頼できない入力として、必ず `emit::build()` で検証すること。
pub fn unpack(data: &[u8]) -> Result<(Manifest, EffectSpec), Issue> {
    let mut zip = zip::ZipArchive::new(Cursor::new(data))
        .map_err(|_| err("ZIP として読めません(.au2fx ではない可能性があります)"))?;
    if zip.len() > MAX_ENTRIES {
        return Err(err("ファイル数が多すぎます"));
    }
    let manifest_raw = read_entry(&mut zip, "manifest.json")?;
    let manifest: Manifest = serde_json::from_slice(&manifest_raw)
        .map_err(|e| err(format!("manifest.json を読めません: {e}")))?;
    let major: u32 = manifest
        .format_version
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| err("format_version が不正です"))?;
    if major != FORMAT_MAJOR {
        return Err(err(format!(
            "この .au2fx の形式(版 {})には対応していません。Forge を更新してください",
            manifest.format_version
        )));
    }
    let spec_raw = read_entry(&mut zip, "spec.json")?;
    if sha256_hex(&spec_raw) != manifest.spec_sha256 {
        return Err(err("spec.json のハッシュが一致しません(壊れているか、改変されています)"));
    }
    let text = String::from_utf8(spec_raw).map_err(|_| err("spec.json が UTF-8 ではありません"))?;
    let spec = EffectSpec::from_json(&text)?;
    Ok((manifest, spec))
}
