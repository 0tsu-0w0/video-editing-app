//! Forge Studio: 動画編集ソフト本体(Phase 1: 読み込み・配置・再生・書き出し)。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod control;
mod timeline;

use audio::AudioEngine;
use egui::{Key, Modifiers};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use studio_core::command::Command::*;
use studio_core::project::PARAMS;
use serde_json::{json, Value};
use std::collections::VecDeque;
use studio_core::{AssetKind, Command, Editor, Id, Origin, Project, Time, TrackKind};
use studio_render::gpu::Gpu;
use studio_render::Renderer;
use timeline::{Action, Timeline};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

const PROJECT_EXT: &str = "fsproj";

/// Claude など外部(MCP)からの操作をどこまで許すか。
#[derive(Clone, Copy, PartialEq, Eq)]
enum McpMode {
    ReadOnly,
    Ask,
    Auto,
}

impl McpMode {
    fn label(self) -> &'static str {
        match self {
            McpMode::ReadOnly => "閲覧のみ",
            McpMode::Ask => "編集は確認する",
            McpMode::Auto => "自動で実行",
        }
    }
}

/// 利用者の承認を待っている外部からの操作。
struct Pending {
    title: String,
    summary: Vec<String>,
    action: PendingAction,
    reply: std::sync::mpsc::Sender<Result<Value, String>>,
}

enum PendingAction {
    Commands { label: String, commands: Vec<Command> },
    Export(PathBuf),
}

/// 書き出しの進み具合(別スレッドと共有)。
struct ExportJob {
    done: Arc<AtomicI64>,
    total: Arc<AtomicI64>,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<(), String>>>>,
    path: PathBuf,
}

/// トラッキングの進み具合(別スレッドと共有)。
struct TrackJob {
    done: Arc<AtomicI64>,
    total: Arc<AtomicI64>,
    cancel: Arc<AtomicBool>,
    /// 完成した追従のコマンドと、結果の説明
    result: Arc<Mutex<Option<Result<(Vec<Command>, String), String>>>>,
    from_mcp: bool,
    /// 履歴に残す名前
    label: &'static str,
}

/// プレビューで四隅をクリックして指定する用途。
#[derive(Clone, Copy, PartialEq)]
enum PinPick {
    /// その場に貼る(動かない)
    Static,
    /// 下の動画の平面を追跡して貼る
    Track,
}

struct Studio {
    editor: Editor,
    path: Option<PathBuf>,
    playhead: Time,
    selected: Option<Id>,
    timeline: Timeline,
    audio: AudioEngine,
    renderer: Renderer,
    preview: Option<(egui::TextureId, usize)>,
    status: String,
    export: Option<ExportJob>,
    /// スライダーを操作中の値(手を離したときに 1 つのコマンドにする)
    editing: Option<(Id, String, f64)>,
    last_project: Option<Arc<Project>>,
    control: Option<control::ControlServer>,
    mcp_mode: McpMode,
    pending: VecDeque<Pending>,
    track_job: Option<TrackJob>,
    /// プレビューのクリックで追跡点を指定するのを待っている、追従させるクリップ
    pick_track: Option<Id>,
    /// プレビューのクリックで四隅を指定中(貼り付けるクリップ、用途、指定済みの点)
    pick_corners: Option<(Id, PinPick, Vec<(f64, f64)>)>,
    last_track_result: String,
    /// 言葉での編集(手動ブリッジ)
    ai_instruction: String,
    ai_reply: String,
    ai_preview: Option<Result<(Vec<Command>, Vec<String>), String>>,
    /// 音声素材の波形(1 秒あたり 100 個の最大振幅)。裏で計算する
    waves: std::collections::HashMap<Id, Arc<Mutex<Option<Vec<f32>>>>>,
}

impl Studio {
    fn new(gpu: Gpu, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let mut editor = Editor::new(Project::default());
        editor
            .transaction(
                "新規",
                vec![
                    AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                    AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                    AddTrack { kind: TrackKind::Audio, name: None, index: None, id: None },
                ],
                Origin::User,
            )
            .unwrap();
        editor.dirty = false;
        let audio = AudioEngine::new(Arc::new(editor.project.clone()));
        let control = control::start(wake).ok();
        let status = format!(
            "音声出力: {} / 外部からの操作の受付: {}",
            audio.device_name,
            control.as_ref().map(|c| format!("127.0.0.1:{}", c.port)).unwrap_or("開始できませんでした".into())
        );
        let mcp_mode = match std::env::var("STUDIO_MCP_MODE").as_deref() {
            Ok("auto") => McpMode::Auto,
            Ok("readonly") => McpMode::ReadOnly,
            _ => McpMode::Ask,
        };
        Self {
            editor,
            path: None,
            playhead: Time::ZERO,
            selected: None,
            timeline: Timeline::default(),
            audio,
            renderer: Renderer::new(gpu),
            preview: None,
            status,
            export: None,
            editing: None,
            last_project: None,
            control,
            mcp_mode,
            pending: VecDeque::new(),
            track_job: None,
            pick_track: None,
            pick_corners: None,
            last_track_result: String::new(),
            ai_instruction: String::new(),
            ai_reply: String::new(),
            ai_preview: None,
            waves: std::collections::HashMap::new(),
        }
    }

    fn run(&mut self, label: &str, cmds: Vec<Command>) -> Option<Vec<Id>> {
        match self.editor.transaction(label, cmds, Origin::User) {
            Ok(o) => {
                self.status = o.summary.join(" / ");
                Some(o.created)
            }
            Err(e) => {
                self.status = format!("できませんでした: {}", e.error.message);
                None
            }
        }
    }

    fn import_files(&mut self, files: Vec<PathBuf>) {
        for f in files {
            match probe(&f) {
                Ok(kind) => {
                    let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    self.run("素材の読み込み", vec![ImportAsset { name, path: f.to_string_lossy().into(), kind, id: None }]);
                }
                Err(e) => self.status = format!("読み込めません: {e}"),
            }
        }
    }

    /// 素材を再生位置に置く(映像は空いている映像トラック、音声は空いている音声トラック)。
    fn place_asset(&mut self, asset: Id) {
        let p = &self.editor.project;
        let a = p.assets[&asset].clone();
        let dur = a.kind.duration().unwrap_or(Time::from_secs_f64(5.0));
        let at = self.playhead;
        let free = |kind: TrackKind| {
            p.tracks.iter().find(|t| t.kind == kind && !t.locked && t.clips.iter().all(|c| c.end() <= at || c.start >= at + dur)).map(|t| t.id)
        };
        let mut cmds = vec![];
        let mut new_tracks = 0;
        let mut target = |kind: TrackKind, cmds: &mut Vec<Command>| -> Id {
            match free(kind) {
                Some(t) => t,
                None => {
                    // 空きが無ければトラックを足す(ID は先に決めておく)
                    let id = Id(p.next_id + new_tracks);
                    new_tracks += 1;
                    cmds.push(AddTrack { kind, name: None, index: None, id: Some(id) });
                    id
                }
            }
        };
        if a.kind.has_video() {
            let t = target(TrackKind::Video, &mut cmds);
            cmds.push(AddClip { track: t, asset, start: at, duration: Some(dur), source_in: None, id: None });
        }
        if a.kind.has_audio() {
            let t = target(TrackKind::Audio, &mut cmds);
            cmds.push(AddClip { track: t, asset, start: at, duration: Some(dur), source_in: None, id: None });
        }
        if let Some(created) = self.run("素材を配置", cmds) {
            self.selected = created.last().copied();
        }
    }

    fn toggle_play(&mut self) {
        if self.audio.is_playing() {
            self.audio.stop();
            self.playhead = self.editor.project.settings.frame_rate.snap(self.playhead);
        } else {
            if self.playhead >= self.editor.project.duration() {
                self.playhead = Time::ZERO;
            }
            self.audio.set_project(Arc::new(self.editor.project.clone()));
            self.audio.play(self.playhead);
        }
    }

    fn seek(&mut self, t: Time) {
        self.playhead = t.max(Time::ZERO);
        if self.audio.is_playing() {
            self.audio.play(self.playhead);
        }
    }

    fn save(&mut self, as_new: bool) {
        let path = match (&self.path, as_new) {
            (Some(p), false) => p.clone(),
            _ => match rfd::FileDialog::new().add_filter("Forge Studio プロジェクト", &[PROJECT_EXT]).set_file_name("project.fsproj").save_file() {
                Some(p) => p,
                None => return,
            },
        };
        match std::fs::write(&path, self.editor.project.to_json()) {
            Ok(()) => {
                self.status = format!("保存しました: {}", path.display());
                self.path = Some(path);
                self.editor.dirty = false;
            }
            Err(e) => self.status = format!("保存できません: {e}"),
        }
    }

    fn open(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Forge Studio プロジェクト", &[PROJECT_EXT]).pick_file() else { return };
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| Project::from_json(&s)) {
            Ok(p) => {
                self.audio.stop();
                self.editor = Editor::new(p);
                self.path = Some(path.clone());
                self.playhead = Time::ZERO;
                self.selected = None;
                let gpu = std::mem::replace(&mut self.renderer, Renderer::new(Gpu::new(None).expect("GPU")));
                self.renderer = Renderer::new(gpu.gpu);
                let missing: Vec<String> = self.editor.project.assets.values().filter(|a| !Path::new(&a.path).exists()).map(|a| a.name.clone()).collect();
                self.status = if missing.is_empty() { format!("開きました: {}", path.display()) } else { format!("見つからない素材があります: {}", missing.join(", ")) };
            }
            Err(e) => self.status = e,
        }
    }

    fn start_export(&mut self) {
        if self.export.is_some() {
            return;
        }
        let Some(path) = rfd::FileDialog::new().add_filter("MP4 動画", &["mp4"]).set_file_name("output.mp4").save_file() else { return };
        self.start_export_to(path);
    }

    fn start_export_to(&mut self, path: PathBuf) {
        if self.export.is_some() {
            return;
        }
        let project = self.editor.project.clone();
        let job = ExportJob {
            done: Arc::new(AtomicI64::new(0)),
            total: Arc::new(AtomicI64::new(1)),
            cancel: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
            path: path.clone(),
        };
        let (done, total, cancel, result) = (job.done.clone(), job.total.clone(), job.cancel.clone(), job.result.clone());
        std::thread::spawn(move || {
            let r = studio_render::export(&project, &path, |pr| {
                done.store(pr.frame, Ordering::Relaxed);
                total.store(pr.total, Ordering::Relaxed);
                !cancel.load(Ordering::Relaxed)
            });
            *result.lock().unwrap() = Some(r);
        });
        self.export = Some(job);
    }

    /// 外部(MCP)からの要求を処理する。編集は権限に従って、すぐ実行するか承認待ちにする。
    fn process_requests(&mut self) {
        let mut reqs = vec![];
        if let Some(c) = &self.control {
            while let Ok(r) = c.rx.try_recv() {
                reqs.push(r);
            }
        }
        for r in reqs {
            let result = self.handle_request(&r.method, &r.params);
            match result {
                Handled::Done(v) => {
                    let _ = r.reply.send(v);
                }
                Handled::NeedsApproval(title, summary, action) => {
                    self.pending.push_back(Pending { title, summary, action, reply: r.reply });
                }
            }
        }
    }

    fn handle_request(&mut self, method: &str, params: &Value) -> Handled {
        use Handled::Done;
        let p = &self.editor.project;
        match method {
            "get_project" => Done(Ok(studio_core::ai::describe(p))),
            "get_command_schema" => Done(Ok(studio_core::ai::command_schema())),
            "get_state" => Done(Ok(json!({
                "playhead": (self.playhead.secs_f64() * 1000.0).round() / 1000.0,
                "playing": self.audio.is_playing(),
                "selected_clip": self.selected.map(|i| i.0),
                "duration": (p.duration().secs_f64() * 1000.0).round() / 1000.0,
                "project_file": self.path.as_ref().map(|x| x.to_string_lossy().to_string()),
                "unsaved_changes": self.editor.dirty,
                "permission": self.mcp_mode.label(),
                "can_undo": self.editor.can_undo(),
                "exporting": self.export.is_some(),
            }))),
            "seek" => match params.get("time").cloned().map(serde_json::from_value::<TimeArg>) {
                Some(Ok(t)) => {
                    self.seek(t.0);
                    Done(Ok(json!({"playhead": t.0.secs_f64()})))
                }
                _ => Done(Err("time(秒、または \"00:00:01.500\")を指定してください".into())),
            },
            "play" => {
                if !self.audio.is_playing() {
                    self.toggle_play();
                }
                Done(Ok(json!("再生しました")))
            }
            "stop" => {
                if self.audio.is_playing() {
                    self.toggle_play();
                }
                Done(Ok(json!({"playhead": self.playhead.secs_f64()})))
            }
            "undo" | "redo" => {
                if self.mcp_mode == McpMode::ReadOnly {
                    return Done(Err("閲覧のみの設定です。アプリ上部の「外部からの操作」で変更できます".into()));
                }
                let r = if method == "undo" { self.editor.undo() } else { self.editor.redo() };
                Done(r.map(|l| json!(format!("{l} を{}", if method == "undo" { "元に戻しました" } else { "やり直しました" }))).ok_or("戻せる操作がありません".into()))
            }
            "render_frame" => {
                let t = params.get("time").cloned().and_then(|v| serde_json::from_value::<TimeArg>(v).ok()).map(|t| t.0).unwrap_or(self.playhead);
                let width = params.get("width").and_then(|w| w.as_u64()).unwrap_or(640).clamp(64, 1920) as u32;
                Done(self.frame_png(t, width).map(|b64| json!({"png_base64": b64, "time": t.secs_f64()})))
            }
            "execute" | "import_file" => {
                let (label, commands) = if method == "import_file" {
                    let Some(path) = params.get("path").and_then(|x| x.as_str()) else { return Done(Err("path を指定してください".into())) };
                    let path = PathBuf::from(path);
                    let kind = match probe(&path) {
                        Ok(k) => k,
                        Err(e) => return Done(Err(e)),
                    };
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    ("素材の読み込み".to_string(), vec![Command::ImportAsset { name, path: path.to_string_lossy().into(), kind, id: None }])
                } else {
                    let cmds = match params.get("commands").cloned().map(serde_json::from_value::<Vec<Command>>) {
                        Some(Ok(c)) if !c.is_empty() => c,
                        Some(Err(e)) => return Done(Err(format!("commands を読めません: {e}。get_command_schema で書式を確認してください"))),
                        _ => return Done(Err("commands(コマンドの配列)を指定してください".into())),
                    };
                    let label = params.get("label").and_then(|l| l.as_str()).unwrap_or("Claude の操作").chars().take(60).collect();
                    (label, cmds)
                };
                // まず試し実行(失敗ならその理由をすぐ返す)
                let summary = match self.editor.dry_run(&commands) {
                    Ok(o) => o.summary,
                    Err(e) => return Done(Err(e.to_string())),
                };
                if params.get("dry_run").and_then(|d| d.as_bool()) == Some(true) {
                    return Done(Ok(json!({"dry_run": true, "summary": summary})));
                }
                match self.mcp_mode {
                    McpMode::ReadOnly => Done(Err("閲覧のみの設定です。アプリ上部の「外部からの操作」で変更できます".into())),
                    McpMode::Ask => Handled::NeedsApproval(label.clone(), summary, PendingAction::Commands { label, commands }),
                    McpMode::Auto => Done(self.apply_external(&label, commands)),
                }
            }
            "export" => {
                let Some(path) = params.get("path").and_then(|x| x.as_str()) else { return Done(Err("path を指定してください".into())) };
                if !path.to_ascii_lowercase().ends_with(".mp4") {
                    return Done(Err("書き出し先は .mp4 にしてください".into()));
                }
                if self.export.is_some() {
                    return Done(Err("書き出し中です".into()));
                }
                let path = PathBuf::from(path);
                let mut summary = vec![format!("書き出し先: {}", path.display())];
                if path.exists() {
                    summary.push("このファイルは既にあり、上書きされます".into());
                }
                // ファイルを書くので、権限の設定に関わらず必ず確認する
                Handled::NeedsApproval("動画の書き出し".into(), summary, PendingAction::Export(path))
            }
            "track_follow" => {
                if self.mcp_mode == McpMode::ReadOnly {
                    return Done(Err("閲覧のみの設定です。アプリ上部の「外部からの操作」で変更できます".into()));
                }
                let clip = params.get("clip").and_then(|c| c.as_u64()).map(Id);
                let (x, y) = (params.get("x").and_then(|v| v.as_f64()), params.get("y").and_then(|v| v.as_f64()));
                let (Some(clip), Some(x), Some(y)) = (clip, x, y) else {
                    return Done(Err("clip(追従させるクリップ ID)と x, y(画面中央を原点とする画素、y は下向き)を指定してください".into()));
                };
                let from = params.get("time").cloned().and_then(|v| serde_json::from_value::<TimeArg>(v).ok()).map(|t| t.0).unwrap_or(self.playhead);
                Done(self.start_tracking(clip, (x, y), from, true).map(|m| json!(format!("{m}。track_status で進み具合と結果を確認できます"))))
            }
            "track_plane" => {
                if self.mcp_mode == McpMode::ReadOnly {
                    return Done(Err("閲覧のみの設定です。アプリ上部の「外部からの操作」で変更できます".into()));
                }
                let clip = params.get("clip").and_then(|c| c.as_u64()).map(Id);
                let corners: Option<[[f64; 2]; 4]> = params.get("corners").cloned().and_then(|v| serde_json::from_value(v).ok());
                let (Some(clip), Some(corners)) = (clip, corners) else {
                    return Done(Err("clip(貼り付けるクリップ ID)と corners(画面上の平面の四隅 [[左上x,左上y],[右上],[右下],[左下]]、画面中央が原点の画素)を指定してください".into()));
                };
                let from = params.get("time").cloned().and_then(|v| serde_json::from_value::<TimeArg>(v).ok()).map(|t| t.0).unwrap_or(self.playhead);
                Done(self.start_plane_tracking(clip, corners.map(|c| (c[0], c[1])), from, true).map(|m| json!(format!("{m}。track_status で進み具合と結果を確認できます"))))
            }
            "track_status" => Done(Ok(match &self.track_job {
                Some(j) => json!({"tracking": true, "done": j.done.load(Ordering::Relaxed), "total": j.total.load(Ordering::Relaxed)}),
                None => json!({"tracking": false, "last_result": self.last_track_result}),
            })),
            "export_status" => Done(Ok(match &self.export {
                Some(j) => json!({"exporting": true, "done": j.done.load(Ordering::Relaxed), "total": j.total.load(Ordering::Relaxed)}),
                None => json!({"exporting": false, "last_status": self.status}),
            })),
            _ => Done(Err(format!("知らない操作です: {method}"))),
        }
    }

    fn apply_external(&mut self, label: &str, commands: Vec<Command>) -> Result<Value, String> {
        match self.editor.transaction(label, commands, Origin::Mcp) {
            Ok(o) => {
                self.status = format!("Claude: {}", o.summary.join(" / "));
                Ok(json!({"created_ids": o.created.iter().map(|i| i.0).collect::<Vec<_>>(), "summary": o.summary}))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn frame_png(&mut self, t: Time, width: u32) -> Result<String, String> {
        let p = self.editor.project.clone();
        let bgra = self.renderer.render_bgra(&p, t)?;
        let (w, h) = (p.settings.width, p.settings.height);
        let mut rgba = bgra;
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("画像を作れません")?;
        let height = ((h as f64 * width as f64 / w as f64).round() as u32).max(1);
        let small = image::imageops::resize(&img, width, height, image::imageops::FilterType::Triangle);
        let mut png = std::io::Cursor::new(Vec::new());
        small.write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        Ok(control::base64(png.get_ref()))
    }

    /// 承認待ちの操作を、1 件ずつ画面に出す。
    fn approval_window(&mut self, ctx: &egui::Context) {
        let Some(p) = self.pending.front() else { return };
        let mut decision = None;
        egui::Window::new("Claude からの操作の確認").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label(egui::RichText::new(&p.title).strong());
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for s in &p.summary {
                    ui.label(format!("・{s}"));
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("実行する").clicked() {
                    decision = Some(true);
                }
                if ui.button("拒否する").clicked() {
                    decision = Some(false);
                }
                if self.pending.len() > 1 {
                    ui.weak(format!("ほか {} 件が待っています", self.pending.len() - 1));
                }
            });
        });
        if let Some(ok) = decision {
            let p = self.pending.pop_front().unwrap();
            let r = if !ok {
                Err("利用者が拒否しました".to_string())
            } else {
                match p.action {
                    PendingAction::Commands { label, commands } => self.apply_external(&label, commands),
                    PendingAction::Export(path) => {
                        self.start_export_to(path.clone());
                        Ok(json!(format!("書き出しを開始しました: {}。export_status で進み具合を確認できます", path.display())))
                    }
                }
            };
            let _ = p.reply.send(r);
        }
    }

    /// `target` を、その下の動画の画面上の点 `screen`(中央原点の画素)の動きに追従させる解析を始める。
    fn start_tracking(&mut self, target: Id, screen: (f64, f64), from: Time, from_mcp: bool) -> Result<String, String> {
        if self.track_job.is_some() {
            return Err("トラッキングの実行中です".into());
        }
        let p = self.editor.project.clone();
        let tc = p.clip(target).ok_or("追従させるクリップがありません")?.clone();
        if !tc.contains(from) {
            return Err(format!("再生位置({from})が、追従させるクリップの表示範囲({}〜{})の外です", tc.start, tc.end()));
        }
        let source = studio_track::source_below(&p, target, from).ok_or("このクリップより下のトラックに、再生位置で表示されている動画がありません")?;
        let src_px = studio_track::screen_to_source(&p, source, from, screen).ok_or("指定した点は、下の動画の範囲の外です")?;
        let job = TrackJob {
            done: Arc::new(AtomicI64::new(0)),
            total: Arc::new(AtomicI64::new(1)),
            cancel: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
            from_mcp,
            label: "トラッキング追従",
        };
        let (done, total, cancel, result) = (job.done.clone(), job.total.clone(), job.cancel.clone(), job.result.clone());
        std::thread::spawn(move || {
            let r = (|| {
                let sc = p.clip(source).unwrap();
                let path = PathBuf::from(&p.assets[&sc.asset].path);
                let mut reader = studio_media::VideoReader::open(&path).map_err(|e| e.to_string())?;
                let f = reader.frame_at(sc.source_in + (from - sc.start)).map_err(|e| e.to_string())?;
                // 平らな場所をクリックしても、近くの追跡しやすい点に吸着させる
                let start = studio_track::best_point_near(&f.bgra, f.width, f.height, src_px, 24.0);
                let tr = studio_track::track_point(&p, source, from, tc.end(), start, |d, t| {
                    done.store(d as i64, Ordering::Relaxed);
                    total.store(t as i64, Ordering::Relaxed);
                    !cancel.load(Ordering::Relaxed)
                })?;
                let ok = tr.iter().filter(|k| k.pos.is_some()).count();
                let lost = tr.iter().find(|k| k.pos.is_none()).map(|k| k.time);
                let cmds = studio_track::follow_commands(&p, target, source, &tr)?;
                let msg = match lost {
                    Some(t) => format!("{ok} フレームを追跡し、{t} で見失ったのでそこで止めました(以降は手で調整してください)"),
                    None => format!("{ok} フレームを追跡し、クリップ {target} を追従させました"),
                };
                Ok((cmds, msg))
            })();
            *result.lock().unwrap() = Some(r);
        });
        self.track_job = Some(job);
        Ok(format!("クリップ {target} の追従を解析中(追跡元: クリップ {source})"))
    }

    /// `target` を、その下の動画の平面(画面上の四隅 `screen`)に貼り付け、平面を追跡して動かす解析を始める。
    fn start_plane_tracking(&mut self, target: Id, screen: [(f64, f64); 4], from: Time, from_mcp: bool) -> Result<String, String> {
        if self.track_job.is_some() {
            return Err("トラッキングの実行中です".into());
        }
        let p = self.editor.project.clone();
        let tc = p.clip(target).ok_or("貼り付けるクリップがありません")?.clone();
        if !tc.contains(from) {
            return Err(format!("再生位置({from})が、貼り付けるクリップの表示範囲({}〜{})の外です", tc.start, tc.end()));
        }
        let source = studio_track::source_below(&p, target, from).ok_or("このクリップより下のトラックに、再生位置で表示されている動画がありません")?;
        let mut quad = [(0.0f32, 0.0f32); 4];
        for (q, s) in quad.iter_mut().zip(screen) {
            *q = studio_track::screen_to_source(&p, source, from, s).ok_or("四隅のどれかが、下の動画の範囲の外です")?;
        }
        let job = TrackJob {
            done: Arc::new(AtomicI64::new(0)),
            total: Arc::new(AtomicI64::new(1)),
            cancel: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
            from_mcp,
            label: "平面トラッキング貼り付け",
        };
        let (done, total, cancel, result) = (job.done.clone(), job.total.clone(), job.cancel.clone(), job.result.clone());
        std::thread::spawn(move || {
            let r = (|| {
                let tr = studio_track::track_plane(&p, source, from, tc.end(), quad, |d, t| {
                    done.store(d as i64, Ordering::Relaxed);
                    total.store(t as i64, Ordering::Relaxed);
                    !cancel.load(Ordering::Relaxed)
                })?;
                let ok = tr.iter().filter(|k| k.corners.is_some()).count();
                let lost = tr.iter().find(|k| k.corners.is_none()).map(|k| k.time);
                let cmds = studio_track::pin_commands(&p, target, source, &tr)?;
                let msg = match lost {
                    Some(t) => format!("平面を {ok} フレーム追跡し、{t} で見失ったのでそこで止めました(以降は手で調整してください)"),
                    None => format!("平面を {ok} フレーム追跡し、クリップ {target} を貼り付けました"),
                };
                Ok((cmds, msg))
            })();
            *result.lock().unwrap() = Some(r);
        });
        self.track_job = Some(job);
        Ok(format!("クリップ {target} を貼る平面を解析中(追跡元: クリップ {source})"))
    }

    /// 終わったトラッキングの結果を反映する。
    fn finish_tracking(&mut self) {
        let Some(job) = &self.track_job else { return };
        let Some(r) = job.result.lock().unwrap().take() else { return };
        let from_mcp = job.from_mcp;
        let label = job.label;
        self.track_job = None;
        match r {
            Ok((cmds, msg)) => {
                if from_mcp && self.mcp_mode == McpMode::Ask {
                    let summary = self.editor.dry_run(&cmds).map(|o| [vec![msg.clone()], o.summary.into_iter().take(3).collect()].concat()).unwrap_or_else(|e| vec![e.to_string()]);
                    let (tx, _rx) = std::sync::mpsc::channel();
                    self.pending.push_back(Pending { title: label.into(), summary, action: PendingAction::Commands { label: label.into(), commands: cmds }, reply: tx });
                    self.last_track_result = format!("{msg}(承認待ち)");
                } else {
                    let origin = if from_mcp { Origin::Mcp } else { Origin::User };
                    match self.editor.transaction(label, cmds, origin) {
                        Ok(_) => self.last_track_result = msg.clone(),
                        Err(e) => self.last_track_result = format!("追従を設定できません: {}", e.error.message),
                    }
                }
                self.status = self.last_track_result.clone();
            }
            Err(e) => {
                self.last_track_result = format!("トラッキングできませんでした: {e}");
                self.status = self.last_track_result.clone();
            }
        }
    }

    /// 音声を持つ素材の波形を、まだなら裏で計算し始める。計算済みのものを返す。
    fn waveforms(&mut self) -> std::collections::HashMap<Id, Arc<Vec<f32>>> {
        let mut out = std::collections::HashMap::new();
        for a in self.editor.project.assets.values() {
            if !a.kind.has_audio() {
                continue;
            }
            let slot = self.waves.entry(a.id).or_insert_with(|| {
                let slot = Arc::new(Mutex::new(None));
                let (s2, path, dur) = (slot.clone(), PathBuf::from(&a.path), a.kind.duration().unwrap_or(Time::ZERO));
                std::thread::spawn(move || {
                    const RATE: u32 = 8000;
                    const PER: usize = 80; // 1 秒に 100 個
                    let Ok(mut r) = studio_media::AudioReader::open(&path, RATE) else {
                        *s2.lock().unwrap() = Some(vec![]);
                        return;
                    };
                    let total = studio_core::time::time_to_sample(dur, RATE).max(0) as usize;
                    let mut peaks = Vec::with_capacity(total / PER + 1);
                    let mut pos = 0usize;
                    while pos < total {
                        let n = (RATE as usize * 10).min(total - pos);
                        let Ok(buf) = r.read(pos as i64, n) else { break };
                        for ch in buf.chunks(PER * 2) {
                            peaks.push(ch.iter().fold(0.0f32, |m, v| m.max(v.abs())));
                        }
                        pos += n;
                    }
                    *s2.lock().unwrap() = Some(peaks);
                });
                slot
            });
            if let Some(v) = slot.lock().unwrap().as_ref() {
                out.insert(a.id, Arc::new(v.clone()));
            }
        }
        out
    }

    /// 言葉での編集の欄。依頼文をコピー → 手持ちの AI に貼る → 返答を貼って取り込む → 確認して実行。
    fn ai_panel(&mut self, ui: &mut egui::Ui) {
        ui.weak("やりたいことを書いて依頼文をコピーし、Claude などに貼り付けてください。返答を下に貼ると、内容を確認してから実行できます。");
        ui.add(egui::TextEdit::multiline(&mut self.ai_instruction).desired_rows(3).desired_width(f32::INFINITY).hint_text("例: 1 秒目から「はじめまして」という文字を、下からふわっと出して"));
        if ui.add_enabled(!self.ai_instruction.trim().is_empty(), egui::Button::new("依頼文をコピー")).clicked() {
            ui.ctx().copy_text(studio_core::ai::edit_request(&self.editor.project, self.playhead, self.selected, &self.ai_instruction));
            self.status = "依頼文をコピーしました。AI に貼り付けて、返答をこの下に貼ってください".into();
        }
        ui.add(egui::TextEdit::multiline(&mut self.ai_reply).desired_rows(3).desired_width(f32::INFINITY).hint_text("AI の返答を貼り付け").font(egui::TextStyle::Monospace));
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.ai_reply.trim().is_empty(), egui::Button::new("取り込む")).clicked() {
                self.ai_preview = Some(studio_core::ai::extract_commands(&self.ai_reply).and_then(|c| match self.editor.dry_run(&c) {
                    Ok(o) => Ok((c, o.summary)),
                    Err(e) => Err(e.to_string()),
                }));
            }
            if ui.button("消す").clicked() {
                self.ai_reply.clear();
                self.ai_preview = None;
            }
        });
        let mut run = None;
        match &self.ai_preview {
            Some(Ok((cmds, summary))) => {
                ui.label(egui::RichText::new("実行すると、次のように変わります").strong());
                for l in summary {
                    ui.label(format!("・{l}"));
                }
                if ui.button("実行する").clicked() {
                    run = Some(cmds.clone());
                }
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                if ui.button("修正依頼をコピー").clicked() {
                    ui.ctx().copy_text(studio_core::ai::repair_request(&self.ai_reply, e));
                    self.status = "修正依頼をコピーしました。AI に貼り付けて、新しい返答を貼ってください".into();
                }
            }
            None => {}
        }
        if let Some(cmds) = run {
            let label = format!("AI: {}", self.ai_instruction.trim().chars().take(30).collect::<String>());
            match self.editor.transaction(&label, cmds, Origin::Ai) {
                Ok(o) => {
                    self.status = format!("AI の編集を実行しました: {}", o.summary.join(" / "));
                    self.ai_preview = None;
                    self.ai_reply.clear();
                }
                Err(e) => self.ai_preview = Some(Err(e.to_string())),
            }
        }
    }

    fn add_text(&mut self) {
        let kind = AssetKind::Text { text: "テキスト".into(), style: studio_core::project::TextStyle { outline: 4.0, ..Default::default() } };
        if let Some(created) = self.run("文字の追加", vec![ImportAsset { name: "文字「テキスト」".into(), path: String::new(), kind, id: None }]) {
            self.place_asset(created[0]);
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let fr = self.editor.project.settings.frame_rate;
        let pressed = |k: Key, m: Modifiers| ctx.input_mut(|i| i.consume_key(m, k));
        if pressed(Key::Space, Modifiers::NONE) {
            self.toggle_play();
        }
        if pressed(Key::Z, Modifiers::COMMAND) {
            if let Some(l) = self.editor.undo() {
                self.status = format!("元に戻しました: {l}");
            }
        }
        if pressed(Key::Y, Modifiers::COMMAND) || pressed(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT) {
            if let Some(l) = self.editor.redo() {
                self.status = format!("やり直しました: {l}");
            }
        }
        if pressed(Key::S, Modifiers::COMMAND) {
            self.save(false);
        }
        if pressed(Key::ArrowRight, Modifiers::NONE) {
            let t = fr.frame_to_time(fr.time_to_frame(self.playhead) + 1);
            self.seek(t);
        }
        if pressed(Key::ArrowLeft, Modifiers::NONE) {
            let t = fr.frame_to_time((fr.time_to_frame(self.playhead) - 1).max(0));
            self.seek(t);
        }
        if pressed(Key::Home, Modifiers::NONE) {
            self.seek(Time::ZERO);
        }
        if pressed(Key::S, Modifiers::NONE) {
            self.split_at_playhead();
        }
        if pressed(Key::Delete, Modifiers::NONE) {
            if let Some(c) = self.selected.take() {
                self.run("削除", vec![RemoveClip { clip: c }]);
            }
        }
    }

    fn split_at_playhead(&mut self) {
        let t = self.playhead;
        let target = self.selected.filter(|id| self.editor.project.clip(*id).is_some_and(|c| c.contains(t) && c.start != t));
        let ids: Vec<Id> = match target {
            Some(id) => vec![id],
            None => self.editor.project.tracks.iter().flat_map(|tr| tr.clips.iter()).filter(|c| c.contains(t) && c.start != t).map(|c| c.id).collect(),
        };
        if ids.is_empty() {
            self.status = "再生位置にクリップがありません".into();
            return;
        }
        self.run("分割", ids.into_iter().map(|clip| SplitClip { clip, at: t, new_id: None }).collect());
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // 再生中は音声の時刻に合わせる
        if self.audio.is_playing() {
            self.playhead = self.audio.position();
            if self.playhead >= self.editor.project.duration() {
                self.audio.stop();
                self.playhead = self.editor.project.duration();
            }
            ctx.request_repaint();
        }
        self.process_requests();
        self.approval_window(&ctx);
        if self.pending.is_empty() {
            self.handle_keys(&ctx);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            self.import_files(dropped);
        }

        egui::Panel::top("menu").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("素材を読み込む").clicked() {
                    if let Some(f) = rfd::FileDialog::new().add_filter("動画・音声・画像", &["mp4", "mov", "m4v", "mkv", "wmv", "avi", "mp3", "wav", "m4a", "aac", "flac", "png", "jpg", "jpeg", "bmp", "gif", "webp", "gltf", "glb", "vrm"]).pick_files() {
                        self.import_files(f);
                    }
                }
                ui.separator();
                if ui.button("開く").clicked() {
                    self.open();
                }
                if ui.button("保存").clicked() {
                    self.save(false);
                }
                if ui.button("名前を付けて保存").clicked() {
                    self.save(true);
                }
                ui.separator();
                if ui.add_enabled(self.editor.can_undo(), egui::Button::new("元に戻す")).clicked() {
                    self.editor.undo();
                }
                if ui.add_enabled(self.editor.can_redo(), egui::Button::new("やり直し")).clicked() {
                    self.editor.redo();
                }
                if ui.button("分割 (S)").clicked() {
                    self.split_at_playhead();
                }
                ui.separator();
                if ui.button("+ 映像トラック").clicked() {
                    self.run("トラック追加", vec![AddTrack { kind: TrackKind::Video, name: None, index: None, id: None }]);
                }
                if ui.button("+ 音声トラック").clicked() {
                    self.run("トラック追加", vec![AddTrack { kind: TrackKind::Audio, name: None, index: None, id: None }]);
                }
                if ui.button("+ 文字").clicked() {
                    self.add_text();
                }
                ui.separator();
                if ui.add_enabled(self.export.is_none(), egui::Button::new("書き出し")).clicked() {
                    self.start_export();
                }
                ui.separator();
                ui.label("外部からの操作:");
                egui::ComboBox::from_id_salt("mcp_mode").selected_text(self.mcp_mode.label()).show_ui(ui, |ui| {
                    for m in [McpMode::ReadOnly, McpMode::Ask, McpMode::Auto] {
                        ui.selectable_value(&mut self.mcp_mode, m, m.label());
                    }
                });
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(job) = &self.export {
                    let (d, t) = (job.done.load(Ordering::Relaxed), job.total.load(Ordering::Relaxed).max(1));
                    ui.add(egui::ProgressBar::new(d as f32 / t as f32).desired_width(200.0).text(format!("書き出し中 {d}/{t}")));
                    if ui.button("中止").clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                if let Some(job) = &self.track_job {
                    let (d, t) = (job.done.load(Ordering::Relaxed), job.total.load(Ordering::Relaxed).max(1));
                    ui.add(egui::ProgressBar::new(d as f32 / t as f32).desired_width(200.0).text(format!("トラッキング中 {d}/{t}")));
                    if ui.button("中止").clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                ui.label(&self.status);
            });
        });
        self.finish_tracking();
        let finished = self.export.as_ref().and_then(|job| {
            let r = job.result.lock().unwrap().take();
            r.map(|r| (r, job.path.clone()))
        });
        if let Some((r, path)) = finished {
            self.status = match r {
                Ok(()) => format!("書き出しました: {}", path.display()),
                Err(e) => format!("書き出せませんでした: {e}"),
            };
            self.export = None;
        }

        egui::Panel::bottom("timeline").resizable(true).default_size(240.0).min_size(120.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let playing = self.audio.is_playing();
                let waves = self.waveforms();
                let actions = self.timeline.show(ui, &self.editor.project, self.playhead, self.selected, playing, &waves);
                for a in actions {
                    match a {
                        Action::Seek(t) => self.seek(t),
                        Action::Select(s) => self.selected = s,
                        Action::Command(c) => {
                            let label = c.name().to_string();
                            self.run(&label, vec![c]);
                        }
                    }
                }
            });
        });

        egui::Panel::left("assets").resizable(true).default_size(220.0).show(ui, |ui| {
            ui.heading("素材");
            ui.weak("ファイルをウィンドウにドロップしても読み込めます");
            let mut place = None;
            let mut remove = None;
            let list_h = (ui.available_height() * 0.45).max(120.0);
            egui::ScrollArea::vertical().id_salt("asset_list").max_height(list_h).show(ui, |ui| {
                for a in self.editor.project.assets.values() {
                    let kind = match &a.kind {
                        AssetKind::Video { width, height, duration, .. } => format!("動画 {width}×{height} {duration}"),
                        AssetKind::Audio { duration, .. } => format!("音声 {duration}"),
                        AssetKind::Image { width, height } => format!("画像 {width}×{height}"),
                        AssetKind::Model { animation } if *animation > 0.0 => format!("3D モデル(動き {animation:.1} 秒)"),
                        AssetKind::Model { .. } => "3D モデル".into(),
                        AssetKind::Text { text, .. } => format!("文字 {}", text.chars().take(16).collect::<String>().replace('\n', " ")),
                    };
                    ui.group(|ui| {
                        ui.label(&a.name);
                        ui.weak(kind);
                        ui.horizontal(|ui| {
                            if ui.small_button("再生位置に置く").clicked() {
                                place = Some(a.id);
                            }
                            if ui.small_button("削除").clicked() {
                                remove = Some(a.id);
                            }
                        });
                    });
                }
            });
            if let Some(a) = place {
                self.place_asset(a);
            }
            if let Some(a) = remove {
                self.run("素材の削除", vec![RemoveAsset { asset: a }]);
            }
            ui.separator();
            egui::ScrollArea::vertical().id_salt("ai_panel").show(ui, |ui| {
                egui::CollapsingHeader::new(egui::RichText::new("AI に頼む(言葉で編集)").strong()).default_open(true).show(ui, |ui| self.ai_panel(ui));
            });
        });

        egui::Panel::right("inspector").resizable(true).default_size(260.0).show(ui, |ui| self.inspector(ui));

        egui::CentralPanel::default().show(ui, |ui| self.preview(ui));
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected.filter(|id| self.editor.project.clip(*id).is_some()) else {
            ui.heading("プロジェクト");
            let s = &self.editor.project.settings;
            ui.label(format!("{}×{}  {:.3} fps  {} Hz", s.width, s.height, s.frame_rate.fps(), s.sample_rate));
            ui.label(format!("長さ {}", self.editor.project.duration()));
            ui.add_space(8.0);
            ui.weak("クリップを選ぶと、ここで位置・大きさ・不透明度・音量などを調整できます。");
            ui.add_space(8.0);
            ui.heading("操作");
            ui.weak("Space: 再生/停止\n← →: 1 フレーム移動\nS: 分割\nDelete: 削除\nCtrl+Z / Ctrl+Y: 元に戻す / やり直し\nCtrl+ホイール: タイムラインの拡大縮小");
            return;
        };
        let clip = self.editor.project.clip(id).unwrap().clone();
        let asset = self.editor.project.assets.get(&clip.asset).map(|a| a.name.clone()).unwrap_or_default();
        ui.heading(format!("クリップ {id}"));
        ui.label(asset);
        ui.label(format!("{} 〜 {}(長さ {})", clip.start, clip.end(), clip.duration));
        ui.separator();
        // 文字の内容と見た目
        if let Some(studio_core::Asset { kind: AssetKind::Text { text, style }, .. }) = self.editor.project.assets.get(&clip.asset).cloned() {
            let aid = clip.asset;
            let mut t = text.clone();
            let mut st = style.clone();
            let mut changed = false;
            changed |= ui.add(egui::TextEdit::multiline(&mut t).desired_rows(2).desired_width(f32::INFINITY)).changed();
            ui.horizontal(|ui| {
                ui.label("大きさ");
                changed |= ui.add(egui::DragValue::new(&mut st.size).range(4.0..=1000.0).speed(1.0)).changed();
                ui.label("縁取り");
                changed |= ui.add(egui::DragValue::new(&mut st.outline).range(0.0..=100.0).speed(0.2)).changed();
            });
            ui.horizontal(|ui| {
                let color = |ui: &mut egui::Ui, label: &str, hex: &mut String| {
                    let c = studio_core::project::parse_rgba(hex).unwrap_or([255; 4]);
                    let mut c32 = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
                    ui.label(label);
                    if ui.color_edit_button_srgba(&mut c32).changed() {
                        let [r, g, b, a] = c32.to_srgba_unmultiplied();
                        *hex = format!("#{r:02X}{g:02X}{b:02X}{a:02X}");
                        return true;
                    }
                    false
                };
                changed |= color(ui, "色", &mut st.color);
                changed |= color(ui, "縁の色", &mut st.outline_color);
            });
            ui.horizontal(|ui| {
                for (v, l) in [("left", "左揃え"), ("center", "中央"), ("right", "右揃え")] {
                    changed |= ui.selectable_value(&mut st.align, v.to_string(), l).changed();
                }
            });
            ui.horizontal(|ui| {
                ui.label("フォント");
                egui::ComboBox::from_id_salt("font").selected_text(&st.font).show_ui(ui, |ui| {
                    for f in ["meiryo", "meiryob", "YuGothM", "YuGothB", "msgothic", "msmincho", "BIZ-UDGothicR", "arial", "impact"] {
                        changed |= ui.selectable_value(&mut st.font, f.to_string(), f).changed();
                    }
                });
            });
            if changed {
                let cmd = SetText { asset: aid, text: Some(t), font: Some(st.font), size: Some(st.size), color: Some(st.color), outline: Some(st.outline), outline_color: Some(st.outline_color), align: Some(st.align) };
                if let Err(e) = self.editor.transaction_merged("文字の変更", vec![cmd], Origin::User, &format!("text:{aid}"), std::time::Duration::from_secs(2)) {
                    self.status = format!("できませんでした: {}", e.error.message);
                }
            }
            ui.separator();
        }
        // フェード
        ui.horizontal(|ui| {
            let (mut fi, mut fo) = (clip.fade_in.secs_f64(), clip.fade_out.secs_f64());
            let max = clip.duration.secs_f64();
            ui.label("フェード イン");
            let a = ui.add(egui::DragValue::new(&mut fi).range(0.0..=max).speed(0.02).suffix(" 秒").fixed_decimals(2)).changed();
            ui.label("アウト");
            let b = ui.add(egui::DragValue::new(&mut fo).range(0.0..=max).speed(0.02).suffix(" 秒").fixed_decimals(2)).changed();
            if a || b {
                let (fi, fo) = (Time::from_secs_f64(fi), Time::from_secs_f64(fo));
                // 合計が長さを超えないよう、動かした方に合わせてもう一方を縮める
                let (fi, fo) = if a { (fi, fo.min(clip.duration - fi)) } else { (fi.min(clip.duration - fo), fo) };
                if let Err(e) = self.editor.transaction_merged("フェード", vec![SetFade { clip: id, fade_in: Some(fi), fade_out: Some(fo) }], Origin::User, &format!("fade:{id}"), std::time::Duration::from_secs(2)) {
                    self.status = format!("できませんでした: {}", e.error.message);
                }
            }
        });
        ui.separator();
        let local = (self.playhead - clip.start).max(Time::ZERO).min(clip.duration);
        let labels = [("x", "X 位置"), ("y", "Y 位置"), ("scale", "拡大率"), ("rotation", "回転"), ("rot_y", "3D 横の向き"), ("rot_x", "3D 縦の傾き"), ("opacity", "不透明度"), ("volume", "音量"), ("pan", "パン")];
        let is_model = matches!(self.editor.project.assets.get(&clip.asset).map(|a| &a.kind), Some(AssetKind::Model { .. }));
        let mut commit: Option<Command> = None;
        for (name, label) in labels {
            if matches!(name, "rot_x" | "rot_y") && !is_model {
                continue;
            }
            let param = clip.params.get(name).unwrap();
            let (_, lo, hi) = *PARAMS.iter().find(|p| p.0 == name).unwrap();
            let range = match name {
                "x" | "y" => -4000.0..=4000.0,
                "scale" => 0.0..=8.0,
                "rotation" | "rot_x" | "rot_y" => -360.0..=360.0,
                "volume" => 0.0..=4.0,
                _ => lo..=hi,
            };
            let mut v = match &self.editing {
                Some((c, n, v)) if *c == id && n == name => *v,
                _ => param.eval(local),
            };
            ui.horizontal(|ui| {
                ui.label(label);
                let keyed = !param.keys.is_empty();
                let r = ui.add(egui::Slider::new(&mut v, range).clamping(egui::SliderClamping::Never));
                if r.changed() {
                    self.editing = Some((id, name.to_string(), v.clamp(lo, hi)));
                }
                let finished = r.drag_stopped() || (r.changed() && !r.dragged()) || r.lost_focus();
                if finished {
                    if let Some((_, _, val)) = self.editing.take() {
                        commit = Some(if keyed {
                            SetKey { clip: id, param: name.into(), time: local, value: val, interp: Default::default() }
                        } else {
                            SetParam { clip: id, param: name.into(), value: val }
                        });
                    }
                }
                if ui.small_button(if keyed { "◆" } else { "◇" }).on_hover_text("再生位置にキーフレームを置く").clicked() {
                    commit = Some(SetKey { clip: id, param: name.into(), time: local, value: v.clamp(lo, hi), interp: Default::default() });
                }
            });
        }
        if let Some(c) = commit {
            let l = c.name().to_string();
            self.run(&l, vec![c]);
        }
        ui.separator();
        let on_video = self.editor.project.find_clip(id).is_some_and(|(ti, _)| self.editor.project.tracks[ti].kind == TrackKind::Video);
        if on_video {
            let picking = self.pick_track == Some(id);
            let label = if picking { "クリックを待っています(取り消す)" } else { "点を追跡して追従させる" };
            if ui.add_enabled(self.track_job.is_none(), egui::Button::new(label)).on_hover_text("下のトラックの動画の上で追いたい点をプレビューでクリックすると、このクリップがその動きに合わせて動きます(再生位置からクリップの終わりまで)").clicked() {
                self.pick_track = if picking { None } else { Some(id) };
                self.pick_corners = None;
                if !picking {
                    self.status = "プレビュー上で、追いかけたい点(模様のはっきりした所)をクリックしてください".into();
                }
            }
            // 四隅の貼り付け(壁・机・画面などの平面に合わせて変形する)
            let pins = self.editor.project.clip(id).map_or(0, |c| c.pin.len());
            ui.horizontal_wrapped(|ui| {
                for (mode, name, tip) in [
                    (PinPick::Static, "四隅を指定して貼る", "プレビューで 左上→右上→右下→左下 の順にクリックした四角形に合わせて変形します"),
                    (PinPick::Track, "平面を追跡して貼る", "下の動画の平面(壁・机など)の四隅を 左上→右上→右下→左下 の順にクリックすると、平面を追跡して貼り付けます(再生位置からクリップの終わりまで)"),
                ] {
                    let active = self.pick_corners.as_ref().is_some_and(|(c, m, _)| *c == id && *m == mode);
                    let label = if active { "クリックを待っています(取り消す)" } else { name };
                    if ui.add_enabled(self.track_job.is_none(), egui::Button::new(label)).on_hover_text(tip).clicked() {
                        self.pick_corners = if active { None } else { Some((id, mode, vec![])) };
                        self.pick_track = None;
                        if !active {
                            self.status = "プレビューで、四隅を 左上 → 右上 → 右下 → 左下 の順にクリックしてください".into();
                        }
                    }
                }
            });
            if pins > 0 {
                ui.horizontal(|ui| {
                    ui.label(format!("四隅で変形中(キー {pins} 個)。位置・拡大・回転は使われません"));
                    if ui.button("外す").clicked() {
                        self.run("四隅を外す", vec![Command::SetPin { clip: id, keys: vec![] }]);
                    }
                });
            }
        }
        if ui.button("このクリップを削除").clicked() {
            self.selected = None;
            self.run("削除", vec![RemoveClip { clip: id }]);
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        // スライダー操作中の値は、確定前でもプレビューに反映する
        let project: Arc<Project> = match &self.editing {
            Some((id, name, v)) => {
                let mut p = self.editor.project.clone();
                if let Some((ti, ci)) = p.find_clip(*id) {
                    let slot = p.tracks[ti].clips[ci].params.get_mut(name).unwrap();
                    if slot.keys.is_empty() {
                        slot.value = *v;
                    }
                }
                Arc::new(p)
            }
            None => match &self.last_project {
                Some(p) if **p == self.editor.project => p.clone(),
                _ => Arc::new(self.editor.project.clone()),
            },
        };
        if self.last_project.as_ref().is_none_or(|p| !Arc::ptr_eq(p, &project)) {
            self.audio.set_project(project.clone());
            self.last_project = Some(project.clone());
        }
        let (pw, ph) = (project.settings.width as f32, project.settings.height as f32);
        ui.horizontal(|ui| {
            if ui.button(if self.audio.is_playing() { "⏸ 停止" } else { "▶ 再生" }).clicked() {
                self.toggle_play();
            }
            if ui.button("⏮").clicked() {
                self.seek(Time::ZERO);
            }
            let fr = project.settings.frame_rate;
            ui.monospace(format!("{}  [{}]  / {}", self.playhead, fr.time_to_frame(self.playhead), project.duration()));
        });
        let tex = match self.renderer.render(&project, self.playhead) {
            Ok(t) => t,
            Err(e) => {
                ui.label(format!("描画できません: {e}"));
                return;
            }
        };
        let key = tex.srv.as_raw_ptr();
        let srv = tex.srv.clone();
        let avail = ui.available_size();
        let scale = (avail.x / pw).min(avail.y / ph).max(0.01);
        let size = egui::vec2(pw * scale, ph * scale);
        if let Some((tid, k)) = self.preview {
            if k == key {
                let (area, resp) = ui.allocate_exact_size(avail, egui::Sense::click());
                let img = egui::Rect::from_center_size(area.center(), size);
                ui.painter().image(tid, img, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                let to_screen = |p: egui::Pos2| (((p.x - img.center().x) / scale) as f64, ((p.y - img.center().y) / scale) as f64);
                let to_ui = |s: (f64, f64)| egui::pos2(img.center().x + s.0 as f32 * scale, img.center().y + s.1 as f32 * scale);
                // 選択中のクリップの四隅を表示する
                if let Some(c) = self.selected.and_then(|id| project.clip(id)).filter(|c| c.contains(self.playhead)) {
                    if let Some(k) = studio_core::project::eval_pin(&c.pin, self.playhead - c.start) {
                        let pts: Vec<egui::Pos2> = k.iter().map(|p| to_ui((p[0], p[1]))).collect();
                        ui.painter().add(egui::Shape::closed_line(pts.clone(), egui::Stroke::new(1.5, egui::Color32::from_rgb(0, 200, 255))));
                        for p in pts {
                            ui.painter().circle_filled(p, 3.5, egui::Color32::from_rgb(0, 200, 255));
                        }
                    }
                }
                if let Some((target, mode, pts)) = self.pick_corners.clone() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                    ui.painter().rect_stroke(img, 0.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 200, 0)), egui::StrokeKind::Outside);
                    let mut shown: Vec<egui::Pos2> = pts.iter().map(|s| to_ui(*s)).collect();
                    if let Some(h) = resp.hover_pos().filter(|p| img.contains(*p)) {
                        shown.push(h);
                    }
                    let col = egui::Color32::from_rgb(255, 200, 0);
                    ui.painter().add(egui::Shape::line(shown.clone(), egui::Stroke::new(1.5, col)));
                    if shown.len() == 4 {
                        ui.painter().line_segment([shown[3], shown[0]], egui::Stroke::new(1.0, col));
                    }
                    for (i, p) in shown.iter().take(pts.len()).enumerate() {
                        ui.painter().circle_filled(*p, 4.0, col);
                        ui.painter().text(*p + egui::vec2(6.0, -6.0), egui::Align2::LEFT_BOTTOM, ["左上", "右上", "右下", "左下"][i], egui::FontId::proportional(13.0), col);
                    }
                    if let Some(pos) = resp.clicked().then(|| resp.interact_pointer_pos()).flatten().filter(|p| img.contains(*p)) {
                        let mut pts = pts;
                        pts.push(to_screen(pos));
                        if pts.len() < 4 {
                            self.status = format!("次は{}をクリックしてください", ["左上", "右上", "右下", "左下"][pts.len()]);
                            self.pick_corners = Some((target, mode, pts));
                        } else {
                            self.pick_corners = None;
                            let corners = [pts[0], pts[1], pts[2], pts[3]];
                            self.status = match mode {
                                PinPick::Track => self.start_plane_tracking(target, corners, self.playhead, false).unwrap_or_else(|e| e),
                                PinPick::Static => {
                                    let local = project.clip(target).map_or(Time::ZERO, |c| (self.playhead - c.start).max(Time::ZERO).min(c.duration));
                                    let keys = vec![studio_core::project::PinKey { time: local, corners: corners.map(|p| [p.0, p.1]) }];
                                    if self.run("四隅に貼る", vec![Command::SetPin { clip: target, keys }]).is_some() {
                                        "四隅に合わせて貼り付けました".into()
                                    } else {
                                        self.status.clone()
                                    }
                                }
                            };
                        }
                    }
                }
                if let Some(target) = self.pick_track {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                    ui.painter().rect_stroke(img, 0.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 200, 0)), egui::StrokeKind::Outside);
                    if let Some(pos) = resp.clicked().then(|| resp.interact_pointer_pos()).flatten().filter(|p| img.contains(*p)) {
                        let screen = (((pos.x - img.center().x) / scale) as f64, ((pos.y - img.center().y) / scale) as f64);
                        self.pick_track = None;
                        self.status = match self.start_tracking(target, screen, self.playhead, false) {
                            Ok(m) => m,
                            Err(e) => e,
                        };
                    }
                }
                return;
            }
        }
        self.preview = Some((egui::TextureId::User(u64::MAX), key));
        PENDING_SRV.with(|p| *p.borrow_mut() = Some(srv));
        let _ = size;
    }
}

/// ファイルを素材として調べる(3D モデルは中身を読んで確かめる)。
fn probe(path: &std::path::Path) -> Result<AssetKind, String> {
    if studio_render::model::is_model_path(path) {
        studio_render::model::probe(path)
    } else {
        studio_media::probe(path).map_err(|e| e.to_string())
    }
}

enum Handled {
    Done(Result<Value, String>),
    NeedsApproval(String, Vec<String>, PendingAction),
}

/// 外部から受け取る時刻(秒の数値、または文字列)。
struct TimeArg(Time);
impl<'de> serde::Deserialize<'de> for TimeArg {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        studio_core::command::secs::deserialize(d).map(TimeArg)
    }
}

trait RawPtr {
    fn as_raw_ptr(&self) -> usize;
}
impl RawPtr for ID3D11ShaderResourceView {
    fn as_raw_ptr(&self) -> usize {
        windows::core::Interface::as_raw(self) as usize
    }
}

thread_local! {
    /// 新しく egui に登録するプレビューのテクスチャ(登録は描画側で行う)
    static PENDING_SRV: std::cell::RefCell<Option<ID3D11ShaderResourceView>> = const { std::cell::RefCell::new(None) };
}

fn setup_fonts(ctx: &egui::Context) {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
    for file in ["meiryo.ttc", "YuGothM.ttc", "msgothic.ttc"] {
        if let Ok(bytes) = std::fs::read(Path::new(&dir).join("Fonts").join(file)) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("jp".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().insert(0, "jp".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}

// ------------------------------------------------------------ ウィンドウと描画

struct App {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain,
    rtv: Option<ID3D11RenderTargetView>,
    egui_ctx: egui::Context,
    renderer: egui_directx11::Renderer,
    winit_state: egui_winit::State,
    studio: Studio,
}

impl App {
    fn new(window: &Window, proxy: winit::event_loop::EventLoopProxy<()>) -> Self {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let RawWindowHandle::Win32(h) = window.window_handle().unwrap().as_raw() else { panic!("Win32 のウィンドウではありません") };
        let hwnd = HWND(h.hwnd.get() as _);
        let size = window.inner_size();
        let (mut device, mut ctx) = (None, None);
        unsafe {
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx))
                .expect("Direct3D 11 を初期化できません");
        }
        let (device, ctx): (ID3D11Device, ID3D11DeviceContext) = (device.unwrap(), ctx.unwrap());
        let factory: IDXGIFactory = unsafe { CreateDXGIFactory() }.unwrap();
        let sd = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC { Width: size.width.max(1), Height: size.height.max(1), Format: DXGI_FORMAT_R8G8B8A8_UNORM, ..Default::default() },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: hwnd,
            Windowed: true.into(),
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            Flags: 0,
        };
        let mut swap = None;
        unsafe { factory.CreateSwapChain(&device, &sd, &mut swap) }.ok().unwrap();
        let swap = swap.unwrap();
        let egui_ctx = egui::Context::default();
        setup_fonts(&egui_ctx);
        let renderer = egui_directx11::Renderer::new(&device).unwrap();
        let winit_state = egui_winit::State::new(egui_ctx.clone(), egui_ctx.viewport_id(), &window, None, None, None);
        let gpu = Gpu::new(Some((device.clone(), ctx.clone()))).expect("描画の初期化に失敗");
        let rtv = Some(Self::make_rtv(&device, &swap));
        let mut studio = Studio::new(gpu, move || {
            let _ = proxy.send_event(());
        });
        // 起動時に渡されたファイル: プロジェクトなら開き、素材なら読み込んで順に並べる
        for arg in std::env::args_os().skip(1).map(PathBuf::from) {
            if arg.extension().is_some_and(|e| e.eq_ignore_ascii_case(PROJECT_EXT)) {
                match std::fs::read_to_string(&arg).map_err(|e| e.to_string()).and_then(|s| Project::from_json(&s)) {
                    Ok(p) => {
                        studio.editor = Editor::new(p);
                        studio.path = Some(arg);
                    }
                    Err(e) => studio.status = e,
                }
            } else {
                let before: Vec<Id> = studio.editor.project.assets.keys().copied().collect();
                studio.import_files(vec![arg]);
                let new = studio.editor.project.assets.keys().copied().find(|k| !before.contains(k));
                if let Some(a) = new {
                    studio.playhead = studio.editor.project.duration();
                    studio.place_asset(a);
                }
            }
        }
        studio.playhead = Time::ZERO;
        if std::env::var("STUDIO_AUTOPLAY").is_ok() {
            studio.toggle_play();
        }
        Self { device, ctx, swap, rtv, egui_ctx, renderer, winit_state, studio }
    }

    fn make_rtv(device: &ID3D11Device, swap: &IDXGISwapChain) -> ID3D11RenderTargetView {
        let back: ID3D11Texture2D = unsafe { swap.GetBuffer(0) }.unwrap();
        let mut rtv = None;
        unsafe { device.CreateRenderTargetView(&back, None, Some(&mut rtv)) }.unwrap();
        rtv.unwrap()
    }

    fn render(&mut self, window: &Window) {
        let input = self.winit_state.take_egui_input(window);
        let studio = &mut self.studio;
        let out = self.egui_ctx.run_ui(input, |ui| studio.ui(ui));
        // プレビューのテクスチャが作り直されたら、egui に登録し直す
        if let Some(srv) = PENDING_SRV.with(|p| p.borrow_mut().take()) {
            if let Some((old, _)) = self.studio.preview {
                if old != egui::TextureId::User(u64::MAX) {
                    self.renderer.unregister_user_texture(old);
                }
            }
            let key = srv.as_raw_ptr();
            let id = self.renderer.register_user_texture(srv);
            self.studio.preview = Some((id, key));
            self.egui_ctx.request_repaint();
        }
        let title = format!(
            "Forge Studio - {}{}",
            self.studio.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or("新規プロジェクト".into()),
            if self.studio.editor.dirty { " *" } else { "" }
        );
        window.set_title(&title);
        let (r, platform, _) = egui_directx11::split_output(out);
        self.winit_state.handle_platform_output(window, platform);
        let Some(rtv) = self.rtv.clone() else { return };
        unsafe { self.ctx.ClearRenderTargetView(&rtv, &[0.08, 0.08, 0.08, 1.0]) };
        let _ = self.renderer.render(&self.ctx, &rtv, &self.egui_ctx, r);
        let _ = unsafe { self.swap.Present(1, DXGI_PRESENT(0)) };
    }

    fn resize(&mut self, s: PhysicalSize<u32>) {
        if s.width == 0 || s.height == 0 {
            return;
        }
        self.rtv = None;
        unsafe { self.swap.ResizeBuffers(2, s.width, s.height, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0)) }.unwrap();
        self.rtv = Some(Self::make_rtv(&self.device, &self.swap));
    }
}

struct Runner {
    window: Option<Window>,
    app: Option<App>,
    proxy: winit::event_loop::EventLoopProxy<()>,
}

impl ApplicationHandler<()> for Runner {
    /// 外部からの要求が届いたら、画面を更新して処理する
    fn user_event(&mut self, _el: &ActiveEventLoop, _e: ()) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let w = el.create_window(WindowAttributes::default().with_title("Forge Studio").with_inner_size(PhysicalSize::new(1600, 960))).unwrap();
        self.app = Some(App::new(&w, self.proxy.clone()));
        self.window = Some(w);
    }
    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if let (Some(w), Some(app)) = (&self.window, &self.app) {
            if app.studio.audio.is_playing() || app.studio.export.is_some() || !app.studio.pending.is_empty() {
                w.request_redraw();
            }
        }
    }
    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        let (Some(w), Some(app)) = (&self.window, &mut self.app) else { return };
        let r = app.winit_state.on_window_event(w, &ev);
        if r.repaint {
            w.request_redraw();
        }
        match ev {
            WindowEvent::CloseRequested => {
                if app.studio.editor.dirty {
                    let ok = rfd::MessageDialog::new()
                        .set_title("Forge Studio")
                        .set_description("保存していない変更があります。終了しますか?")
                        .set_buttons(rfd::MessageButtons::YesNo)
                        .show();
                    if ok != rfd::MessageDialogResult::Yes {
                        return;
                    }
                }
                el.exit()
            }
            WindowEvent::Resized(s) => app.resize(s),
            WindowEvent::RedrawRequested => app.render(w),
            _ => {}
        }
    }
}

fn main() {
    // Media Foundation の初期化は、素材を開くときに各スレッドで行う(UI スレッドはウィンドウ側が先に初期化する)
    let el = EventLoop::<()>::with_user_event().build().unwrap();
    let mut r = Runner { window: None, app: None, proxy: el.create_proxy() };
    el.run_app(&mut r).unwrap();
}
