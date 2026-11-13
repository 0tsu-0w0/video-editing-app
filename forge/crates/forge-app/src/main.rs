//! forge-gui: 依頼文のコピー → 返答の貼り付け → プレビュー → AviUtl2 へ追加、を1画面で行う。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use forge_core::spec::{EffectSpec, Issue, ParamKind};
use forge_core::{au2fx, emit, prompt};
use forge_gpu::{Checked, FrameInfo, Gpu, Image};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const PREVIEW_MAX: u32 = 1280;
const LOOP_SECONDS: f32 = 5.0;

/// 設定項目の現在値(画面用)。
#[derive(Clone, PartialEq)]
enum Value {
    Number(f32),
    Bool(bool),
    Color([f32; 3]),
    Choice(usize),
}

impl Value {
    fn to_floats(&self) -> Vec<f32> {
        match self {
            Value::Number(v) => vec![*v],
            Value::Bool(b) => vec![if *b { 1.0 } else { 0.0 }],
            Value::Color(c) => c.to_vec(),
            Value::Choice(i) => vec![*i as f32],
        }
    }
}

fn default_values(spec: &EffectSpec) -> Vec<Value> {
    spec.parameters
        .iter()
        .map(|p| match &p.kind {
            ParamKind::Float { default, .. } => Value::Number(*default as f32),
            ParamKind::Int { default, .. } => Value::Number(*default as f32),
            ParamKind::Bool { default } => Value::Bool(*default),
            ParamKind::Select { default, .. } => Value::Choice(*default),
            ParamKind::Color { .. } => {
                let d = p.default_values();
                Value::Color([d[0], d[1], d[2]])
            }
        })
        .collect()
}

/// 検証を通って読み込まれているエフェクト。
struct Loaded {
    spec: EffectSpec,
    checked: Checked,
    values: Vec<Value>,
}

/// 直前の取り込みの失敗(修復依頼文を作るために保持)。
struct Failed {
    stage: &'static str,
    issues: Vec<Issue>,
    source: String,
    refused: bool,
}

struct App {
    gpu: Result<Gpu, String>,
    instruction: String,
    reply: String,
    loaded: Option<Loaded>,
    failed: Option<Failed>,
    /// 画面下に出す一言(成功・案内・エラー)。
    status: String,
    input: Image,
    input_name: String,
    time: f32,
    playing: bool,
    dirty: bool,
    texture: Option<egui::TextureHandle>,
    render_error: Option<String>,
    library: Vec<PathBuf>,
    confirm_overwrite: bool,
}

fn library_dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    Path::new(&base).join("Forge").join("library")
}

fn script_dir() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_string());
    Path::new(&base).join("aviutl2").join("Script").join("Forge")
}

fn list_library() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(library_dir())
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("au2fx")))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn write_file(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{} を作れません: {e}", dir.display()))?;
    }
    std::fs::write(path, data).map_err(|e| format!("{} に書けません: {e}", path.display()))
}

fn setup_fonts(ctx: &egui::Context) {
    // egui の標準フォントには日本語が無いので、Windows のフォントを読む。
    let candidates = [("meiryo.ttc", 0), ("YuGothM.ttc", 0), ("msgothic.ttc", 0)];
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
    for (file, index) in candidates {
        if let Ok(bytes) = std::fs::read(Path::new(&dir).join("Fonts").join(file)) {
            let mut data = egui::FontData::from_owned(bytes);
            data.index = index;
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("jp".to_owned(), data);
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().insert(0, "jp".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        cc.egui_ctx.set_style(style);
        let gpu = Gpu::new(false);
        let status = match &gpu {
            Ok(g) if g.software => "GPU を使えないため、ソフトウェア描画で動いています。".to_string(),
            Ok(_) => "作りたい効果を書いて、依頼文をコピーしてください。".to_string(),
            Err(e) => format!("描画を初期化できません: {e}"),
        };
        let mut app = Self {
            gpu,
            instruction: String::new(),
            reply: String::new(),
            loaded: None,
            failed: None,
            status,
            input: forge_gpu::test_image(640, 360),
            input_name: "テスト画像".to_string(),
            time: 0.5,
            playing: false,
            dirty: true,
            texture: None,
            render_error: None,
            library: list_library(),
            confirm_overwrite: false,
        };
        // 起動時に渡されたファイル(.au2fx や画像)を開く。exe へのドロップでも同じ。
        for arg in std::env::args_os().skip(1) {
            let p = PathBuf::from(arg);
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("au2fx")) {
                app.open_au2fx(&p);
            } else {
                app.open_image(&p);
            }
        }
        app
    }

    /// 検証して、通れば表示対象にする。
    fn load_spec(&mut self, spec: EffectSpec, source: String) {
        self.confirm_overwrite = false;
        let Ok(gpu) = &self.gpu else { return };
        match forge_gpu::check_spec(gpu, &spec) {
            Ok(checked) => {
                self.status = if checked.warnings.is_empty() {
                    format!("「{}」を読み込みました。", spec.meta.name)
                } else {
                    format!("「{}」を読み込みました。注意: {}", spec.meta.name, checked.warnings.join(" / "))
                };
                let values = default_values(&spec);
                self.loaded = Some(Loaded { spec, checked, values });
                self.failed = None;
                self.dirty = true;
            }
            Err(f) => {
                let refused = f.issues.iter().any(|i| i.code == "E_REFUSAL");
                self.status = format!("{}で問題が見つかりました。", f.stage);
                self.failed = Some(Failed { stage: f.stage, issues: f.issues, source, refused });
            }
        }
    }

    fn import_reply(&mut self) {
        let text = self.reply.trim().to_string();
        if text.is_empty() {
            self.status = "AI の返答を貼り付けてから押してください。".to_string();
            return;
        }
        let parsed = prompt::extract_json(&text).and_then(|j| EffectSpec::from_json(&j).map(|s| (s, j)));
        match parsed {
            Ok((spec, json)) => self.load_spec(spec, json),
            Err(e) => {
                self.status = "返答を読み取れませんでした。".to_string();
                self.failed = Some(Failed { stage: "返答の読み取り", issues: vec![e], source: text, refused: false });
            }
        }
    }

    fn open_au2fx(&mut self, path: &Path) {
        let result = std::fs::read(path)
            .map_err(|e| format!("{} を読めません: {e}", path.display()))
            .and_then(|b| au2fx::unpack(&b).map_err(|e| e.to_string()));
        match result {
            Ok((_, spec)) => {
                let json = serde_json::to_string_pretty(&spec).unwrap_or_default();
                self.load_spec(spec, json);
            }
            Err(e) => self.status = e,
        }
    }

    fn open_image(&mut self, path: &Path) {
        match image::open(path) {
            Ok(img) => {
                let img = if img.width() > PREVIEW_MAX || img.height() > PREVIEW_MAX {
                    img.resize(PREVIEW_MAX, PREVIEW_MAX, image::imageops::FilterType::Triangle)
                } else {
                    img
                };
                let rgba = img.to_rgba8();
                self.input = Image::from_rgba8(rgba.width(), rgba.height(), rgba.as_raw());
                self.input_name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.dirty = true;
            }
            Err(e) => self.status = format!("画像を開けません: {e}"),
        }
    }

    fn overrides(&self) -> HashMap<String, Vec<f32>> {
        match &self.loaded {
            Some(l) => l.spec.parameters.iter().zip(&l.values).map(|(p, v)| (p.id.clone(), v.to_floats())).collect(),
            None => HashMap::new(),
        }
    }

    fn refresh_preview(&mut self, ctx: &egui::Context) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let output = match (&self.gpu, &self.loaded) {
            (Ok(gpu), Some(l)) => {
                let info = FrameInfo {
                    time: self.time,
                    progress: self.time / LOOP_SECONDS,
                    frame: (self.time * 30.0).floor(),
                    seed: 12345.0,
                };
                forge_gpu::render(gpu, &l.spec, &l.checked.compiled, &self.input, info, &self.overrides())
            }
            _ => Ok(self.input.clone()),
        };
        match output {
            Ok(img) => {
                self.render_error = None;
                let color = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width as usize, img.height as usize],
                    &img.to_rgba8(),
                );
                match &mut self.texture {
                    Some(t) => t.set(color, egui::TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("preview", color, egui::TextureOptions::LINEAR)),
                }
            }
            Err(e) => self.render_error = Some(e),
        }
    }

    fn install(&mut self) {
        let Some(l) = &self.loaded else { return };
        let dst = script_dir().join(emit::file_name(&l.spec));
        if dst.exists() && !self.confirm_overwrite {
            self.confirm_overwrite = true;
            self.status = format!("{} は既にあります。上書きする場合はもう一度押してください。", dst.display());
            return;
        }
        let existed = dst.exists();
        self.confirm_overwrite = false;
        self.status = match write_file(&dst, l.checked.anm2.as_bytes()) {
            Ok(()) if existed => format!(
                "{} を上書きしました。設定項目を変えた場合は AviUtl2 の再起動、シェーダだけなら「キャッシュを破棄」で反映されます。",
                dst.display()
            ),
            Ok(()) => format!("{} に追加しました。AviUtl2 を再起動すると「Forge」の中に出ます。", dst.display()),
            Err(e) => e,
        };
    }

    fn save_to_library(&mut self) {
        let Some(l) = &self.loaded else { return };
        let path = library_dir().join(format!("{}.au2fx", l.spec.meta.name));
        self.status = match au2fx::pack(&l.spec).map_err(|e| e.to_string()).and_then(|b| write_file(&path, &b)) {
            Ok(()) => format!("ライブラリに保存しました({})。", path.display()),
            Err(e) => e,
        };
        self.library = list_library();
    }

    fn export_au2fx(&mut self) {
        let Some(l) = &self.loaded else { return };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}.au2fx", l.spec.meta.name))
            .add_filter("Forge エフェクト", &["au2fx"])
            .save_file()
        else {
            return;
        };
        self.status = match au2fx::pack(&l.spec).map_err(|e| e.to_string()).and_then(|b| write_file(&path, &b)) {
            Ok(()) => format!("{} に書き出しました。", path.display()),
            Err(e) => e,
        };
    }

    // ------------------------------------------------------------ 画面

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("1. 作りたい効果");
        ui.add(
            egui::TextEdit::multiline(&mut self.instruction)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("例: VHS風のノイズと色ズレ"),
        );
        let has_text = !self.instruction.trim().is_empty();
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(has_text, egui::Button::new("依頼文をコピー")).clicked() {
                ui.ctx().copy_text(prompt::build_request(&self.instruction));
                self.status = "依頼文をコピーしました。Claude などに貼り付け、返答を下の欄に貼ってください。".to_string();
            }
            if let Some(l) = &self.loaded {
                if ui
                    .add_enabled(has_text, egui::Button::new("今の効果の修正依頼をコピー"))
                    .on_hover_text("上の欄に「もっと強く」などの修正内容を書いて押します")
                    .clicked()
                {
                    ui.ctx().copy_text(prompt::build_revision(&l.spec, &self.instruction));
                    self.status = "修正依頼文をコピーしました。AI に貼り付け、返答を下の欄に貼ってください。".to_string();
                }
            }
        });

        ui.add_space(10.0);
        ui.heading("2. AI の返答");
        egui::ScrollArea::vertical().id_salt("reply").max_height(170.0).show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut self.reply)
                    .desired_rows(7)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("AI の返答をここに貼り付け"),
            );
        });
        ui.horizontal(|ui| {
            if ui.button("貼り付け").clicked() {
                match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                    Ok(t) => self.reply = t,
                    Err(_) => self.status = "クリップボードに文字がありません。".to_string(),
                }
            }
            if ui.button("取り込む").clicked() {
                self.import_reply();
            }
            if ui.button("消す").clicked() {
                self.reply.clear();
            }
        });

        if self.failed.is_some() {
            ui.add_space(8.0);
            self.failure_box(ui);
        }

        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("ライブラリ");
            if ui.small_button("ファイルを開く").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Forge エフェクト", &["au2fx"]).pick_file() {
                    self.open_au2fx(&p);
                }
            }
        });
        if self.library.is_empty() {
            ui.weak("保存したエフェクトがここに並びます。");
        }
        let mut open: Option<PathBuf> = None;
        egui::ScrollArea::vertical().id_salt("library").show(ui, |ui| {
            for p in &self.library {
                let name = p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if ui.selectable_label(false, name).clicked() {
                    open = Some(p.clone());
                }
            }
        });
        if let Some(p) = open {
            self.open_au2fx(&p);
        }
    }

    fn failure_box(&mut self, ui: &mut egui::Ui) {
        let Some(f) = &self.failed else { return };
        let mut copy = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.colored_label(ui.visuals().error_fg_color, format!("{}で問題が見つかりました", f.stage));
            egui::ScrollArea::vertical().id_salt("issues").max_height(130.0).show(ui, |ui| {
                for i in &f.issues {
                    ui.label(&i.message);
                }
            });
            if f.refused {
                ui.weak("この効果は作れないと AI が判断しました。内容を変えて依頼し直してください。");
            } else if ui.button("修復依頼文をコピー").clicked() {
                copy = true;
            }
        });
        if copy {
            ui.ctx().copy_text(prompt::build_repair(&f.source, f.stage, &f.issues));
            self.status = "修復依頼文をコピーしました。AI に貼り付け、返答をもう一度取り込んでください。".to_string();
        }
    }

    fn right_panel(&mut self, ui: &mut egui::Ui) {
        let Some(l) = &mut self.loaded else {
            ui.heading("設定項目");
            ui.weak("エフェクトを読み込むと、ここで値を調整できます。");
            return;
        };
        ui.heading(&l.spec.meta.name);
        if !l.spec.meta.description.is_empty() {
            ui.weak(&l.spec.meta.description);
        }
        ui.add_space(6.0);
        let mut changed = false;
        egui::ScrollArea::vertical().id_salt("params").max_height(ui.available_height() - 150.0).show(ui, |ui| {
            for (p, v) in l.spec.parameters.iter().zip(l.values.iter_mut()) {
                match (&p.kind, v) {
                    (ParamKind::Float { min, max, step, .. }, Value::Number(x)) => {
                        ui.label(&p.label);
                        changed |= ui
                            .add(egui::Slider::new(x, *min as f32..=*max as f32).step_by(*step))
                            .changed();
                    }
                    (ParamKind::Int { min, max, .. }, Value::Number(x)) => {
                        ui.label(&p.label);
                        changed |= ui.add(egui::Slider::new(x, *min as f32..=*max as f32).step_by(1.0)).changed();
                    }
                    (ParamKind::Bool { .. }, Value::Bool(b)) => {
                        changed |= ui.checkbox(b, &p.label).changed();
                    }
                    (ParamKind::Color { .. }, Value::Color(c)) => {
                        ui.horizontal(|ui| {
                            changed |= ui.color_edit_button_rgb(c).changed();
                            ui.label(&p.label);
                        });
                    }
                    (ParamKind::Select { options, .. }, Value::Choice(i)) => {
                        ui.label(&p.label);
                        egui::ComboBox::from_id_salt(&p.id)
                            .selected_text(options.get(*i).cloned().unwrap_or_default())
                            .show_ui(ui, |ui| {
                                for (n, o) in options.iter().enumerate() {
                                    changed |= ui.selectable_value(i, n, o).changed();
                                }
                            });
                    }
                    _ => {}
                }
            }
            if l.spec.parameters.is_empty() {
                ui.weak("調整できる項目はありません。");
            }
        });
        let mut reset = false;
        if !l.spec.parameters.is_empty() && ui.small_button("既定値に戻す").clicked() {
            reset = true;
        }
        if reset {
            l.values = default_values(&l.spec);
            changed = true;
        }
        self.dirty |= changed;

        ui.add_space(8.0);
        ui.separator();
        let label = if self.confirm_overwrite { "上書きする" } else { "AviUtl2 に追加" };
        if ui.add_sized([ui.available_width(), 30.0], egui::Button::new(label)).clicked() {
            self.install();
        }
        ui.horizontal(|ui| {
            if ui.button("ライブラリに保存").clicked() {
                self.save_to_library();
            }
            if ui.button("書き出す").clicked() {
                self.export_au2fx();
            }
        });
    }

    fn center_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("画像を開く").clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("画像", &["png", "jpg", "jpeg"])
                    .pick_file()
                {
                    self.open_image(&p);
                }
            }
            if ui.button("テスト画像").clicked() {
                self.input = forge_gpu::test_image(640, 360);
                self.input_name = "テスト画像".to_string();
                self.dirty = true;
            }
            ui.weak(format!("{}({}×{})", self.input_name, self.input.width, self.input.height));
        });
        ui.horizontal(|ui| {
            if ui.button(if self.playing { "停止" } else { "再生" }).clicked() {
                self.playing = !self.playing;
            }
            ui.label("時間");
            if ui
                .add(egui::Slider::new(&mut self.time, 0.0..=LOOP_SECONDS).suffix(" 秒").fixed_decimals(2))
                .changed()
            {
                self.dirty = true;
            }
        });
        ui.add_space(4.0);
        if let Some(e) = &self.render_error {
            ui.colored_label(ui.visuals().error_fg_color, format!("描画できません: {e}"));
        }
        if let Some(tex) = &self.texture {
            let avail = ui.available_size();
            let size = tex.size_vec2();
            let scale = (avail.x / size.x).min(avail.y / size.y).min(2.0).max(0.05);
            let shown = size * scale;
            let (rect, _) = ui.allocate_exact_size(shown, egui::Sense::hover());
            // 透明部分が分かるよう、市松模様を下に敷く
            let painter = ui.painter_at(rect);
            let cell = 12.0;
            let (dark, light) = (egui::Color32::from_gray(70), egui::Color32::from_gray(95));
            painter.rect_filled(rect, 0.0, dark);
            let mut y = rect.top();
            let mut row = 0;
            while y < rect.bottom() {
                let mut x = rect.left() + if row % 2 == 0 { 0.0 } else { cell };
                while x < rect.right() {
                    painter.rect_filled(
                        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell, cell)).intersect(rect),
                        0.0,
                        light,
                    );
                    x += cell * 2.0;
                }
                y += cell;
                row += 1;
            }
            painter.image(
                tex.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.playing && self.loaded.is_some() {
            self.time = (self.time + ctx.input(|i| i.stable_dt).min(0.1)) % LOOP_SECONDS;
            self.dirty = true;
            ctx.request_repaint();
        }
        // ファイルのドロップ(画像 / .au2fx)
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for p in dropped {
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("au2fx")) {
                self.open_au2fx(&p);
            } else {
                self.open_image(&p);
            }
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.label(&self.status);
            ui.add_space(2.0);
        });
        egui::SidePanel::left("left").resizable(true).default_width(360.0).min_width(280.0).show(ctx, |ui| {
            ui.add_space(4.0);
            self.left_panel(ui);
        });
        egui::SidePanel::right("right").resizable(true).default_width(280.0).min_width(220.0).show(ctx, |ui| {
            ui.add_space(4.0);
            self.right_panel(ui);
        });
        self.refresh_preview(ctx);
        egui::CentralPanel::default().show(ctx, |ui| self.center_panel(ui));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Forge")
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("Forge", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
