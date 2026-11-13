//! 外部(MCP の中継プログラム)からの操作の受付口。
//!
//! - 127.0.0.1 の空いているポートだけで待ち受ける(外部のネットワークからは届かない)
//! - 起動ごとに作る合言葉(トークン)が一致しない要求は拒否する
//! - 接続先と合言葉は `%LOCALAPPDATA%\ForgeStudio\control.json` に書き、終了時に消す
//! - 要求は UI スレッドへ渡し、UI スレッドが処理して返す(編集は必ずエディタを通る)
//!
//! 通信: 1 行 1 個の JSON。要求 `{"token", "id", "method", "params"}`、応答 `{"id", "ok", "result" | "error"}`。

use serde_json::{json, Value};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

pub struct Request {
    pub method: String,
    pub params: Value,
    pub reply: Sender<Result<Value, String>>,
}

pub struct ControlServer {
    pub rx: Receiver<Request>,
    pub port: u16,
    info_path: PathBuf,
}

/// OS の乱数で初期化されるハッシュの鍵を使って、推測できない合言葉を作る。
fn make_token() -> String {
    let mut s = String::new();
    for i in 0..4u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        s.push_str(&format!("{:016x}", h.finish()));
    }
    s
}

/// 長さと内容が一致するか(比較時間が内容に依存しない)。
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn info_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("ForgeStudio").join("control.json")
}

fn handle(stream: TcpStream, token: String, tx: Sender<Request>, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(600)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.len() > 8 * 1024 * 1024 {
            break;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(writer, "{}", json!({"id": null, "ok": false, "error": format!("JSON を読めません: {e}")}));
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        if !req.get("token").and_then(|t| t.as_str()).is_some_and(|t| same(t, &token)) {
            let _ = writeln!(writer, "{}", json!({"id": id, "ok": false, "error": "合言葉(トークン)が違います"}));
            break; // 合言葉が違う接続は切る
        }
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let (rtx, rrx) = channel();
        if tx.send(Request { method, params, reply: rtx }).is_err() {
            break;
        }
        wake();
        // 承認待ちがあるので、長めに待つ
        let resp = match rrx.recv_timeout(Duration::from_secs(600)) {
            Ok(Ok(v)) => json!({"id": id, "ok": true, "result": v}),
            Ok(Err(e)) => json!({"id": id, "ok": false, "error": e}),
            Err(_) => json!({"id": id, "ok": false, "error": "アプリからの応答がありません(承認待ちのまま時間切れ、または終了)"}),
        };
        if writeln!(writer, "{resp}").is_err() {
            break;
        }
    }
}

/// 受付口を開く。`wake` は要求が来たときに UI を起こす関数。
pub fn start(wake: impl Fn() + Send + Sync + 'static) -> std::io::Result<ControlServer> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let token = make_token();
    let info_path = info_path();
    if let Some(dir) = info_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&info_path, json!({"port": port, "token": token, "pid": std::process::id()}).to_string())?;
    let (tx, rx) = channel();
    let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(wake);
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(s) = s else { continue };
            // 念のため、ローカル以外からの接続は切る
            if !s.peer_addr().map(|a| a.ip().is_loopback()).unwrap_or(false) {
                continue;
            }
            let (token, tx, wake) = (token.clone(), tx.clone(), wake.clone());
            std::thread::spawn(move || handle(s, token, tx, wake));
        }
    });
    Ok(ControlServer { rx, port, info_path })
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        // 自分が書いたものだけ消す(別のインスタンスが上書きしていたら残す)
        if let Ok(s) = std::fs::read_to_string(&self.info_path) {
            if serde_json::from_str::<Value>(&s).ok().and_then(|v| v["pid"].as_u64()) == Some(std::process::id() as u64) {
                let _ = std::fs::remove_file(&self.info_path);
            }
        }
    }
}

/// PNG などを base64 にする(MCP で画像を返すため)。
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn tokens_are_long_and_differ() {
        let (a, b) = (make_token(), make_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(same(&a, &a.clone()) && !same(&a, &b) && !same(&a, &a[..10]));
    }
}
