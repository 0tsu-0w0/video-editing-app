//! forge-studio-mcp: Claude(Claude Code / Claude Desktop)から Forge Studio を操作するための MCP サーバー。
//!
//! Claude がこのプログラムを起動し、標準入出力で MCP(JSON-RPC 2.0、1 行 1 メッセージ)をやり取りする。
//! 実際の操作は、起動中の Forge Studio の受付口(127.0.0.1、合言葉つき)へ取り次ぐ。
//! 接続先は `%LOCALAPPDATA%\ForgeStudio\control.json` から毎回読む(アプリを再起動しても使える)。

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

const SERVER_NAME: &str = "forge-studio";
const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Forge Studio(動画編集ソフト)を操作します。\
まず get_project でプロジェクトの素材・トラック・クリップ(ID と時刻)を確認し、\
get_command_schema でコマンドの書式を確認してから execute_commands で編集してください。\
編集は 1 回の呼び出しが 1 回の Undo で戻せる単位になります。dry_run: true で、実行せずに結果を確認できます。\
利用者の設定によっては、編集のたびにアプリ側で承認が求められます。\
render_frame で任意の時刻の画面を画像で確認できます。時刻はすべて秒です。";

fn tools() -> Value {
    let time = json!({"description": "時刻(秒の数値、または \"00:01:02.500\" 形式)", "anyOf": [{"type": "number"}, {"type": "string"}]});
    json!([
        {"name": "get_project", "description": "プロジェクトの要約(設定・素材・トラック・クリップ・各項目の値やキーフレーム)を返す。ID はここで確認する",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "get_state", "description": "再生位置・再生中か・選択中のクリップ・未保存の変更・操作の権限、再生中なら音声トラックごとと全体の音量(ピーク dB)を返す",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "get_command_schema", "description": "execute_commands で使えるコマンドの一覧・引数・注意点・例を返す",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "execute_commands", "description": "編集コマンドの配列を、1 回の Undo で戻せる 1 つの操作として実行する。1 つでも失敗すると全て取り消され、何番目がなぜ失敗したかを返す",
         "inputSchema": {"type": "object", "required": ["commands"], "properties": {
             "commands": {"type": "array", "items": {"type": "object"}, "description": "コマンドの配列(書式は get_command_schema)"},
             "label": {"type": "string", "description": "操作履歴に表示する名前(例: 「キャラを右から登場させる」)"},
             "dry_run": {"type": "boolean", "description": "true なら実行せず、成功するかと変更内容だけを返す"}}}},
        {"name": "import_file", "description": "この PC にある動画・音声・画像・3D モデル(.gltf / .glb / .vrm)のファイルを素材として読み込む(配置は execute_commands の add_clip で行う)",
         "inputSchema": {"type": "object", "required": ["path"], "properties": {"path": {"type": "string", "description": "ファイルの絶対パス"}}}},
        {"name": "render_frame", "description": "指定した時刻の画面を PNG 画像で返す(編集結果の確認用)",
         "inputSchema": {"type": "object", "properties": {"time": time, "width": {"type": "integer", "description": "画像の幅(既定 640、最大 1920)"}}}},
        {"name": "select_clip", "description": "クリップを選択して、アプリの調整欄に表示する(利用者に見せたいクリップを示すときに使う)。clip を省略すると選択を外す",
         "inputSchema": {"type": "object", "properties": {"clip": {"type": "integer"}}}},
        {"name": "seek", "description": "再生位置を移動する", "inputSchema": {"type": "object", "required": ["time"], "properties": {"time": time}}},
        {"name": "play", "description": "再生を始める", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "stop", "description": "再生を止める", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "undo", "description": "直前の操作を元に戻す", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "redo", "description": "元に戻した操作をやり直す", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "export_video", "description": "MP4(H.264 + AAC)に書き出す。必ず利用者の承認が必要。開始後は export_status で進み具合を確認する",
         "inputSchema": {"type": "object", "required": ["path"], "properties": {"path": {"type": "string", "description": "書き出し先の .mp4 の絶対パス"}}}},
        {"name": "export_status", "description": "書き出しの進み具合", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "track_follow", "description": "クリップ(clip)を、その下のトラックの動画上の点の動きに追従させる。点は画面上の位置(画面中央が原点の画素、y は下向き)で、time の時点の画面で指定する。time からクリップの終わりまで追跡し、x / y のキーフレームを設定する。解析は裏で進むので track_status で結果を確認する。render_frame で画面を見て、模様のはっきりした点を選ぶとよい",
         "inputSchema": {"type": "object", "required": ["clip", "x", "y"], "properties": {
             "clip": {"type": "integer", "description": "追従させるクリップの ID"},
             "x": {"type": "number"}, "y": {"type": "number"}, "time": time}}},
        {"name": "track_plane", "description": "クリップ(clip)を、その下のトラックの動画に写っている平面(壁・机・看板・画面など)に貼り付け、平面を追跡して遠近ごと動かす。corners は time の時点の画面上での平面の四隅(左上・右上・右下・左下の順、画面中央が原点の画素、y は下向き)。clip の素材の四隅がそこに合わさる。解析は裏で進むので track_status で結果を確認する。貼り付けだけなら execute_commands の set_pin を使う",
         "inputSchema": {"type": "object", "required": ["clip", "corners"], "properties": {
             "clip": {"type": "integer", "description": "貼り付けるクリップの ID"},
             "corners": {"type": "array", "minItems": 4, "maxItems": 4, "items": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number"}}},
             "time": time}}},
        {"name": "track_person", "description": "クリップ(clip)を、その下のトラックの動画に写っている人物の部位(part)の動きに追従させる。x, y は time の時点の画面で、その人物の体の上の点(画面中央が原点の画素、y は下向き)。複数人いても、選んだ人を追い続ける。人物の推定モデルが必要(無ければエラーで置き場所を返す)。解析は裏で進むので track_status で結果を確認する",
         "inputSchema": {"type": "object", "required": ["clip", "x", "y"], "properties": {
             "clip": {"type": "integer", "description": "追従させるクリップの ID"},
             "x": {"type": "number"}, "y": {"type": "number"},
             "part": {"type": "string", "enum": ["head", "neck", "body", "hips", "left_hand", "right_hand", "left_foot", "right_foot"], "description": "部位(既定は head)。左右は写っている本人から見た左右"},
             "time": time}}},
        {"name": "track_status", "description": "トラッキングの進み具合と、最後の結果", "inputSchema": {"type": "object", "properties": {}}},
    ])
}

/// ツール名 → アプリ側の操作名
fn method_of(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "get_project" => "get_project",
        "get_state" => "get_state",
        "get_command_schema" => "get_command_schema",
        "execute_commands" => "execute",
        "import_file" => "import_file",
        "render_frame" => "render_frame",
        "select_clip" => "select",
        "seek" => "seek",
        "play" => "play",
        "stop" => "stop",
        "undo" => "undo",
        "redo" => "redo",
        "export_video" => "export",
        "export_status" => "export_status",
        "track_follow" => "track_follow",
        "track_plane" => "track_plane",
        "track_person" => "track_person",
        "track_status" => "track_status",
        _ => return None,
    })
}

fn info_path() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(base).join("ForgeStudio").join("control.json")
}

/// 起動中のアプリへ 1 つ要求を送り、結果を受け取る。
fn call_app(method: &str, params: Value) -> Result<Value, String> {
    let not_running = "Forge Studio が起動していません。アプリを起動してから、もう一度試してください。";
    let info: Value = std::fs::read_to_string(info_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).ok_or(not_running)?;
    let port = info["port"].as_u64().ok_or(not_running)? as u16;
    let token = info["token"].as_str().ok_or(not_running)?;
    let mut stream = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(3)).map_err(|_| not_running.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(620))).ok();
    let req = json!({"token": token, "id": 1, "method": method, "params": params});
    writeln!(stream, "{req}").map_err(|e| format!("送信できません: {e}"))?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(|e| format!("受信できません: {e}"))?;
    let resp: Value = serde_json::from_str(&line).map_err(|e| format!("応答を読めません: {e}"))?;
    if resp["ok"].as_bool() == Some(true) {
        Ok(resp["result"].clone())
    } else {
        Err(resp["error"].as_str().unwrap_or("不明なエラー").to_string())
    }
}

fn call_tool(params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let Some(method) = method_of(name) else {
        return json!({"isError": true, "content": [{"type": "text", "text": format!("知らないツールです: {name}")}]});
    };
    match call_app(method, args) {
        Ok(v) if method == "render_frame" => json!({"content": [
            {"type": "image", "data": v["png_base64"], "mimeType": "image/png"},
            {"type": "text", "text": format!("時刻 {} 秒の画面", v["time"])}
        ]}),
        Ok(v) => {
            let text = match &v {
                Value::String(s) => s.clone(),
                _ => serde_json::to_string_pretty(&v).unwrap_or_default(),
            };
            json!({"content": [{"type": "text", "text": text}]})
        }
        Err(e) => json!({"isError": true, "content": [{"type": "text", "text": e}]}),
    }
}

fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone(); // id の無いもの(通知)には返事をしない
    let method = msg["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => {
            let asked = msg["params"]["protocolVersion"].as_str().unwrap_or("");
            let version = if SUPPORTED_VERSIONS.contains(&asked) { asked } else { SUPPORTED_VERSIONS[0] };
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call_tool(&msg["params"]),
        _ => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Method not found: {method}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let r: Vec<Value> = batch.iter().filter_map(handle).collect();
                (!r.is_empty()).then(|| Value::Array(r))
            }
            Ok(msg) => handle(&msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("Parse error: {e}")}})),
        };
        if let Some(r) = reply {
            if writeln!(stdout, "{r}").is_err() || stdout.flush().is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_list() {
        let r = handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], SERVER_NAME);
        let r = handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        for t in tools {
            assert!(method_of(t["name"].as_str().unwrap()).is_some(), "{t}");
            assert_eq!(t["inputSchema"]["type"], "object");
        }
        assert!(handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let r = handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        let r = handle(&json!({"jsonrpc": "2.0", "id": 4, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], SUPPORTED_VERSIONS[0]);
    }
}
