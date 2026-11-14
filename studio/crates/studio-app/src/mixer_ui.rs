//! ミキサーの窓: 音声トラックごとの音量・バランス・消音・ソロ・エフェクトと、音量のメーター。

use crate::Studio;
use studio_core::command::Command;
use studio_core::project::AudioEffect;
use studio_core::{Id, Origin, TrackKind};
use std::time::Duration;

/// メーター(縦長、左右 2 本)。値は直線のピーク。
fn meter(ui: &mut egui::Ui, (l, r): (f32, f32), height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, height), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, egui::Color32::from_gray(25));
    let to_y = |v: f32| {
        let db = studio_render::audio_fx::gain_to_db(v).clamp(-60.0, 6.0);
        rect.bottom() - (db + 60.0) / 66.0 * rect.height()
    };
    for (i, v) in [l, r].into_iter().enumerate() {
        let x0 = rect.left() + 2.0 + i as f32 * 10.0;
        let top = to_y(v);
        let col = if v >= 1.0 {
            egui::Color32::from_rgb(230, 60, 50)
        } else if v > 0.5 {
            egui::Color32::from_rgb(230, 200, 40)
        } else {
            egui::Color32::from_rgb(70, 200, 90)
        };
        if v > 0.0 {
            p.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x0 + 8.0, rect.bottom())), 0.0, col);
        }
    }
    // 0dB と -12dB / -24dB の目盛り
    for (db, c) in [(0.0, 140), (-12.0, 70), (-24.0, 70)] {
        let y = rect.bottom() - (db + 60.0) / 66.0 * rect.height();
        p.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(1.0, egui::Color32::from_gray(c)));
    }
}

impl Studio {
    /// 連続した操作(スライダー)を 1 つの Undo にまとめて実行する。
    fn mix_edit(&mut self, key: String, cmd: Command) {
        match self.editor.transaction_merged("ミキサー", vec![cmd], Origin::User, &key, Duration::from_secs(1)) {
            Ok(o) => self.status = o.summary.join(" / "),
            Err(e) => self.status = format!("できませんでした: {}", e.error.message),
        }
    }

    pub(crate) fn mixer_window(&mut self, ctx: &egui::Context) {
        if !self.show_mixer {
            return;
        }
        // メーター: 今の音量へ素早く上がり、ゆっくり下がる
        let levels = self.audio.levels();
        let mut target: std::collections::HashMap<u64, (f32, f32)> = Default::default();
        if let Some(l) = &levels {
            for (id, a, b) in &l.tracks {
                target.insert(id.0, (*a, *b));
            }
            target.insert(0, l.master);
        }
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        let fall = 10f32.powf(-20.0 * dt / 20.0); // 1 秒に 20dB 下がる
        let keys: Vec<u64> = self.meters.keys().copied().chain(target.keys().copied()).collect();
        for k in keys {
            let t = target.get(&k).copied().unwrap_or((0.0, 0.0));
            let m = self.meters.entry(k).or_insert((0.0, 0.0));
            m.0 = t.0.max(m.0 * fall);
            m.1 = t.1.max(m.1 * fall);
        }
        if self.meters.values().any(|v| v.0 > 1e-4 || v.1 > 1e-4) {
            ctx.request_repaint();
        }

        let mut open = true;
        let project = self.editor.project.clone();
        let audio_tracks: Vec<(Id, String)> = project.tracks.iter().filter(|t| t.kind == TrackKind::Audio).map(|t| (t.id, t.name.clone())).collect();
        let mut edits: Vec<(String, Command)> = vec![];
        egui::Window::new("ミキサー").open(&mut open).order(egui::Order::Foreground).default_size([640.0, 420.0]).resizable(true).show(ctx, |ui| {
            if audio_tracks.is_empty() {
                ui.label("音声トラックがありません。上の「+ 音声トラック」で追加し、音声の素材を置いてください。");
            }
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    for t in project.tracks.iter().filter(|t| t.kind == TrackKind::Audio) {
                        let id = t.id;
                        ui.group(|ui| {
                            ui.set_width(170.0);
                            ui.vertical(|ui| {
                                ui.strong(&t.name);
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(t.muted, "M").on_hover_text("消音").clicked() {
                                        edits.push((format!("mute:{id}"), Command::SetTrackFlags { track: id, muted: Some(!t.muted), locked: None }));
                                    }
                                    if ui.selectable_label(t.audio.solo, "S").on_hover_text("ソロ(ソロのトラックだけ鳴らす)").clicked() {
                                        edits.push((format!("solo:{id}"), Command::SetTrackAudio { track: id, gain_db: None, pan: None, solo: Some(!t.audio.solo) }));
                                    }
                                });
                                ui.horizontal(|ui| {
                                    // 縦のフェーダーを長くして、細かく動かせるようにする
                                    ui.spacing_mut().slider_width = 200.0;
                                    meter(ui, self.meters.get(&id.0).copied().unwrap_or_default(), 200.0);
                                    let mut g = t.audio.gain_db;
                                    if ui.add(egui::Slider::new(&mut g, -60.0..=12.0).vertical().suffix(" dB").fixed_decimals(1)).on_hover_text("音量").changed() {
                                        edits.push((format!("gain:{id}"), Command::SetTrackAudio { track: id, gain_db: Some(g), pan: None, solo: None }));
                                    }
                                });
                                let mut p = t.audio.pan;
                                ui.horizontal(|ui| {
                                    ui.label("左右");
                                    if ui.add(egui::Slider::new(&mut p, -1.0..=1.0).fixed_decimals(2)).changed() {
                                        edits.push((format!("pan:{id}"), Command::SetTrackAudio { track: id, gain_db: None, pan: Some(p), solo: None }));
                                    }
                                });
                                ui.separator();
                                // エフェクト
                                let mut fx = t.audio.effects.clone();
                                let mut changed = false;
                                let mut remove = None;
                                for (i, e) in fx.iter_mut().enumerate() {
                                    let title = match e {
                                        AudioEffect::Eq { .. } => "イコライザ",
                                        AudioEffect::Compressor { .. } => "コンプレッサー",
                                        AudioEffect::Duck { .. } => "ダッキング",
                                    };
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(title).strong());
                                        if ui.small_button("×").on_hover_text("外す").clicked() {
                                            remove = Some(i);
                                        }
                                    });
                                    let s = |ui: &mut egui::Ui, v: &mut f64, r: std::ops::RangeInclusive<f64>, label: &str, suffix: &str| -> bool {
                                        ui.horizontal(|ui| {
                                            ui.label(label);
                                            ui.add(egui::Slider::new(v, r).suffix(suffix).fixed_decimals(1)).changed()
                                        })
                                        .inner
                                    };
                                    match e {
                                        AudioEffect::Eq { low_db, mid_db, high_db } => {
                                            changed |= s(ui, low_db, -24.0..=24.0, "低", " dB");
                                            changed |= s(ui, mid_db, -24.0..=24.0, "中", " dB");
                                            changed |= s(ui, high_db, -24.0..=24.0, "高", " dB");
                                        }
                                        AudioEffect::Compressor { threshold_db, ratio, makeup_db } => {
                                            changed |= s(ui, threshold_db, -60.0..=0.0, "しきい値", " dB");
                                            changed |= s(ui, ratio, 1.0..=20.0, "比率", ":1");
                                            changed |= s(ui, makeup_db, 0.0..=24.0, "持ち上げ", " dB");
                                        }
                                        AudioEffect::Duck { source, amount_db } => {
                                            ui.horizontal(|ui| {
                                                ui.label("きっかけ");
                                                let name = audio_tracks.iter().find(|(i, _)| i == source).map_or("(無し)".to_string(), |(_, n)| n.clone());
                                                egui::ComboBox::from_id_salt(("duck", id, i)).selected_text(name).show_ui(ui, |ui| {
                                                    for (oid, n) in audio_tracks.iter().filter(|(o, _)| *o != id) {
                                                        changed |= ui.selectable_value(source, *oid, n).changed();
                                                    }
                                                });
                                            });
                                            changed |= s(ui, amount_db, -40.0..=0.0, "下げる量", " dB");
                                        }
                                    }
                                    ui.add_space(4.0);
                                }
                                if let Some(i) = remove {
                                    fx.remove(i);
                                    changed = true;
                                }
                                egui::ComboBox::from_id_salt(("addfx", id)).selected_text("+ エフェクト").show_ui(ui, |ui| {
                                    if ui.selectable_label(false, "イコライザ").clicked() {
                                        fx.push(AudioEffect::Eq { low_db: 0.0, mid_db: 0.0, high_db: 0.0 });
                                        changed = true;
                                    }
                                    if ui.selectable_label(false, "コンプレッサー").clicked() {
                                        fx.push(AudioEffect::Compressor { threshold_db: -18.0, ratio: 4.0, makeup_db: 3.0 });
                                        changed = true;
                                    }
                                    let other = audio_tracks.iter().find(|(o, _)| *o != id).map(|(o, _)| *o);
                                    if ui.add_enabled(other.is_some(), egui::Button::selectable(false, "ダッキング")).on_hover_text("ほかのトラック(声など)に音がある間、このトラックを下げる").clicked() {
                                        fx.push(AudioEffect::Duck { source: other.unwrap(), amount_db: -12.0 });
                                        changed = true;
                                    }
                                });
                                if changed {
                                    edits.push((format!("fx:{id}"), Command::SetTrackEffects { track: id, effects: fx }));
                                }
                            });
                        });
                    }
                    // 全体
                    ui.group(|ui| {
                        ui.set_width(120.0);
                        ui.vertical(|ui| {
                            ui.strong("全体");
                            ui.horizontal(|ui| {
                                ui.spacing_mut().slider_width = 200.0;
                                meter(ui, self.meters.get(&0).copied().unwrap_or_default(), 200.0);
                                let mut g = project.settings.master_gain_db;
                                if ui.add(egui::Slider::new(&mut g, -60.0..=12.0).vertical().suffix(" dB").fixed_decimals(1)).changed() {
                                    edits.push(("master".into(), Command::SetMasterGain { gain_db: g }));
                                }
                            });
                            let m = self.meters.get(&0).copied().unwrap_or_default();
                            let db = studio_render::audio_fx::gain_to_db(m.0.max(m.1));
                            ui.label(if db > -100.0 { format!("ピーク {db:.1} dB") } else { "ピーク -∞".into() });
                            if m.0.max(m.1) >= 1.0 {
                                ui.colored_label(egui::Color32::from_rgb(230, 60, 50), "音割れ");
                            }
                        });
                    });
                });
            });
        });
        for (k, c) in edits {
            self.mix_edit(k, c);
        }
        if !open {
            self.show_mixer = false;
        }
    }
}
