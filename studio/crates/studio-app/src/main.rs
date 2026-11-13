//! Forge Studio: 動画編集ソフト本体(Phase 1: 読み込み・配置・再生・書き出し)。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod timeline;

use audio::AudioEngine;
use egui::{Key, Modifiers};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use studio_core::command::Command::*;
use studio_core::project::PARAMS;
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

/// 書き出しの進み具合(別スレッドと共有)。
struct ExportJob {
    done: Arc<AtomicI64>,
    total: Arc<AtomicI64>,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<(), String>>>>,
    path: PathBuf,
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
}

impl Studio {
    fn new(gpu: Gpu) -> Self {
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
        let status = format!("音声出力: {}", audio.device_name);
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
            match studio_media::probe(&f) {
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
        self.handle_keys(&ctx);
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            self.import_files(dropped);
        }

        egui::Panel::top("menu").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("素材を読み込む").clicked() {
                    if let Some(f) = rfd::FileDialog::new().add_filter("動画・音声・画像", &["mp4", "mov", "m4v", "mkv", "wmv", "avi", "mp3", "wav", "m4a", "aac", "flac", "png", "jpg", "jpeg", "bmp", "gif", "webp"]).pick_files() {
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
                ui.separator();
                if ui.add_enabled(self.export.is_none(), egui::Button::new("書き出し")).clicked() {
                    self.start_export();
                }
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
                ui.label(&self.status);
            });
        });
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
                let actions = self.timeline.show(ui, &self.editor.project, self.playhead, self.selected, playing);
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
            egui::ScrollArea::vertical().show(ui, |ui| {
                for a in self.editor.project.assets.values() {
                    let kind = match &a.kind {
                        AssetKind::Video { width, height, duration, .. } => format!("動画 {width}×{height} {duration}"),
                        AssetKind::Audio { duration, .. } => format!("音声 {duration}"),
                        AssetKind::Image { width, height } => format!("画像 {width}×{height}"),
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
        let local = (self.playhead - clip.start).max(Time::ZERO).min(clip.duration);
        let labels = [("x", "X 位置"), ("y", "Y 位置"), ("scale", "拡大率"), ("rotation", "回転"), ("opacity", "不透明度"), ("volume", "音量"), ("pan", "パン")];
        let mut commit: Option<Command> = None;
        for (name, label) in labels {
            let param = clip.params.get(name).unwrap();
            let (_, lo, hi) = *PARAMS.iter().find(|p| p.0 == name).unwrap();
            let range = match name {
                "x" | "y" => -4000.0..=4000.0,
                "scale" => 0.0..=8.0,
                "rotation" => -360.0..=360.0,
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
                ui.centered_and_justified(|ui| ui.add(egui::Image::from_texture((tid, size))));
                return;
            }
        }
        self.preview = Some((egui::TextureId::User(u64::MAX), key));
        PENDING_SRV.with(|p| *p.borrow_mut() = Some(srv));
        let _ = size;
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
    fn new(window: &Window) -> Self {
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
        let mut studio = Studio::new(gpu);
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
}

impl ApplicationHandler for Runner {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let w = el.create_window(WindowAttributes::default().with_title("Forge Studio").with_inner_size(PhysicalSize::new(1600, 960))).unwrap();
        self.app = Some(App::new(&w));
        self.window = Some(w);
    }
    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if let (Some(w), Some(app)) = (&self.window, &self.app) {
            if app.studio.audio.is_playing() || app.studio.export.is_some() {
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
    let mut r = Runner { window: None, app: None };
    EventLoop::new().unwrap().run_app(&mut r).unwrap();
}
