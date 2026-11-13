//! タイムライン(トラックとクリップの表示・選択・移動・長さ変更)。
//! 操作はすべてコマンドとして返し、実行は呼び出し側(エディタ)が行う。

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use studio_core::{Command, Id, Project, Time, TrackKind};

pub const HEADER_W: f32 = 120.0;
const RULER_H: f32 = 22.0;
const ROW_H: f32 = 36.0;
const EDGE: f32 = 6.0;

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    TrimStart,
    TrimEnd,
}

#[derive(Clone, Copy)]
struct Drag {
    clip: Id,
    kind: DragKind,
    /// ドラッグ開始時の、ポインタの時刻とクリップの元の値
    grab: Time,
    start: Time,
    duration: Time,
    source_in: Time,
    track: usize,
}

pub struct Timeline {
    /// 1 秒あたりのピクセル数
    pub zoom: f32,
    /// 左端の時刻(秒)
    pub scroll: f32,
    drag: Option<Drag>,
}

pub enum Action {
    Seek(Time),
    Select(Option<Id>),
    Command(Command),
}

impl Default for Timeline {
    fn default() -> Self {
        Self { zoom: 80.0, scroll: 0.0, drag: None }
    }
}

fn clip_color(kind: TrackKind, selected: bool) -> Color32 {
    match (kind, selected) {
        (TrackKind::Video, false) => Color32::from_rgb(70, 110, 170),
        (TrackKind::Video, true) => Color32::from_rgb(110, 160, 230),
        (TrackKind::Audio, false) => Color32::from_rgb(70, 140, 100),
        (TrackKind::Audio, true) => Color32::from_rgb(100, 200, 140),
    }
}

impl Timeline {
    fn x_of(&self, rect: Rect, t: Time) -> f32 {
        rect.left() + HEADER_W + (t.secs_f64() as f32 - self.scroll) * self.zoom
    }
    fn t_of(&self, rect: Rect, x: f32) -> Time {
        Time::from_secs_f64(((x - rect.left() - HEADER_W) / self.zoom + self.scroll).max(0.0) as f64)
    }

    /// スナップ: 再生位置・他のクリップの端・0 に、8px 以内なら吸着させる。
    fn snap(&self, p: &Project, rect: Rect, t: Time, playhead: Time, ignore: Id) -> Time {
        let mut best = (8.0f32, t);
        let mut consider = |c: Time| {
            let d = (self.x_of(rect, c) - self.x_of(rect, t)).abs();
            if d < best.0 {
                best = (d, c);
            }
        };
        consider(playhead);
        consider(Time::ZERO);
        for tr in &p.tracks {
            for c in tr.clips.iter().filter(|c| c.id != ignore) {
                consider(c.start);
                consider(c.end());
            }
        }
        p.settings.frame_rate.snap(best.1)
    }

    pub fn show(&mut self, ui: &mut Ui, p: &Project, playhead: Time, selected: Option<Id>, follow: bool, waves: &std::collections::HashMap<Id, std::sync::Arc<Vec<f32>>>) -> Vec<Action> {
        let mut actions = Vec::new();
        // 再生中は、再生位置が見える範囲から出たらページ送りする
        if follow {
            let visible = ((ui.available_width() - HEADER_W) / self.zoom).max(0.5);
            let t = playhead.secs_f64() as f32;
            if t < self.scroll || t > self.scroll + visible * 0.95 {
                self.scroll = (t - visible * 0.05).max(0.0);
            }
        }
        let rows = p.tracks.len().max(1) as f32;
        let size = Vec2::new(ui.available_width(), (RULER_H + rows * ROW_H).max(ui.available_height()));
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        // 左右のパネルの境界線より手前に描く(タイムラインの上に線が重ならないように)
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Middle, ui.id().with("timeline_layer"))).with_clip_rect(rect.intersect(ui.clip_rect()));
        let vis = ui.visuals().clone();
        painter.rect_filled(rect, 0.0, vis.extreme_bg_color);

        // ズーム(Ctrl+ホイール)とスクロール(ホイール)
        if resp.hovered() {
            let (scroll, ctrl) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.ctrl));
            if ctrl && scroll.y != 0.0 {
                let pointer_t = ui.input(|i| i.pointer.hover_pos()).map(|pp| self.t_of(rect, pp.x).secs_f64() as f32);
                self.zoom = (self.zoom * (1.0 + scroll.y * 0.002)).clamp(2.0, 2000.0);
                if let Some(pt) = pointer_t {
                    let px = ui.input(|i| i.pointer.hover_pos()).unwrap().x;
                    self.scroll = (pt - (px - rect.left() - HEADER_W) / self.zoom).max(0.0);
                }
            } else if scroll.x != 0.0 || scroll.y != 0.0 {
                self.scroll = (self.scroll - (scroll.x + scroll.y) / self.zoom).max(0.0);
            }
        }

        // 目盛り
        let ruler = Rect::from_min_size(rect.min, Vec2::new(rect.width(), RULER_H));
        painter.rect_filled(ruler, 0.0, vis.faint_bg_color);
        let step = [0.1f32, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0].into_iter().find(|s| s * self.zoom >= 70.0).unwrap_or(1800.0);
        let mut t = (self.scroll / step).floor() * step;
        while self.x_of(rect, Time::from_secs_f64(t as f64)) < rect.right() {
            let x = self.x_of(rect, Time::from_secs_f64(t as f64));
            if x >= rect.left() + HEADER_W {
                painter.line_segment([Pos2::new(x, ruler.bottom() - 6.0), Pos2::new(x, ruler.bottom())], Stroke::new(1.0, vis.text_color()));
                let label = if step < 1.0 { format!("{t:.1}") } else { format!("{}:{:02}", t as i64 / 60, t as i64 % 60) };
                painter.text(Pos2::new(x + 3.0, ruler.top() + 2.0), Align2::LEFT_TOP, label, FontId::proportional(11.0), vis.weak_text_color());
            }
            t += step;
        }

        // トラックとクリップ
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let mut hovered_clip: Option<(Id, DragKind)> = None;
        for (ti, track) in p.tracks.iter().enumerate() {
            let row = Rect::from_min_size(Pos2::new(rect.left(), ruler.bottom() + ti as f32 * ROW_H), Vec2::new(rect.width(), ROW_H));
            if ti % 2 == 1 {
                painter.rect_filled(row, 0.0, vis.faint_bg_color.gamma_multiply(0.5));
            }
            let head = Rect::from_min_size(row.min, Vec2::new(HEADER_W, ROW_H));
            painter.rect_filled(head, 0.0, vis.widgets.noninteractive.bg_fill);
            let flags = format!("{}{}", if track.muted { " 🔇" } else { "" }, if track.locked { " 🔒" } else { "" });
            painter.text(head.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, format!("{}{}", track.name, flags), FontId::proportional(13.0), vis.text_color());
            for c in &track.clips {
                let (mut start, mut dur, mut row_i) = (c.start, c.duration, ti);
                if let Some(d) = self.drag.filter(|d| d.clip == c.id) {
                    if let Some(pp) = pointer {
                        let now = self.t_of(rect, pp.x);
                        let delta = now - d.grab;
                        match d.kind {
                            DragKind::Move => {
                                start = self.snap(p, rect, (d.start + delta).max(Time::ZERO), playhead, c.id);
                                let r = ((pp.y - ruler.bottom()) / ROW_H).floor() as isize;
                                if r >= 0 && (r as usize) < p.tracks.len() && p.tracks[r as usize].kind == track.kind {
                                    row_i = r as usize;
                                }
                            }
                            DragKind::TrimStart => {
                                let s = self.snap(p, rect, d.start + delta, playhead, c.id).max(d.start - d.source_in).min(d.start + d.duration - Time(1));
                                start = s;
                                dur = d.duration - (s - d.start);
                            }
                            DragKind::TrimEnd => {
                                let e = self.snap(p, rect, d.start + d.duration + delta, playhead, c.id).max(d.start + Time(1));
                                dur = e - d.start;
                            }
                        }
                    }
                }
                let top = ruler.bottom() + row_i as f32 * ROW_H;
                let r = Rect::from_min_max(Pos2::new(self.x_of(rect, start).max(rect.left() + HEADER_W), top + 3.0), Pos2::new(self.x_of(rect, start + dur), top + ROW_H - 3.0));
                if r.right() <= rect.left() + HEADER_W || r.left() >= rect.right() {
                    continue;
                }
                let sel = selected == Some(c.id);
                painter.rect_filled(r, CornerRadius::same(4), clip_color(track.kind, sel));
                if sel {
                    painter.rect_stroke(r, CornerRadius::same(4), Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
                }
                // 音声の波形(1 秒に 100 個の最大振幅)
                if track.kind == TrackKind::Audio {
                    if let Some(w) = waves.get(&c.asset) {
                        let painter = painter.with_clip_rect(r);
                        let mid = r.center().y;
                        let half = r.height() / 2.0 - 2.0;
                        let col = Color32::from_rgba_unmultiplied(220, 255, 230, 150);
                        let mut x = r.left().max(rect.left() + HEADER_W);
                        while x < r.right().min(rect.right()) {
                            let src = c.source_in + (self.t_of(rect, x) - start);
                            let i = (src.secs_f64() * 100.0) as usize;
                            let span = ((1.0 / self.zoom as f64) * 100.0).ceil().max(1.0) as usize;
                            let v = w.get(i..(i + span).min(w.len())).map(|s| s.iter().cloned().fold(0.0f32, f32::max)).unwrap_or(0.0);
                            let vol = c.params.volume.eval(src - c.source_in).max(0.0) as f32 * c.fade_gain(self.t_of(rect, x) - start) as f32;
                            let hgt = (v * vol).min(1.0) * half;
                            painter.line_segment([Pos2::new(x, mid - hgt), Pos2::new(x, mid + hgt)], Stroke::new(1.0, col));
                            x += 1.0;
                        }
                    }
                }
                // フェードの三角形
                let fade_col = Color32::from_rgba_unmultiplied(0, 0, 0, 110);
                if c.fade_in.0 > 0 {
                    let fx = self.x_of(rect, start + c.fade_in).min(r.right());
                    painter.with_clip_rect(r).add(egui::Shape::convex_polygon(vec![r.left_top(), Pos2::new(fx, r.top()), r.left_bottom()], fade_col, Stroke::NONE));
                }
                if c.fade_out.0 > 0 {
                    let fx = self.x_of(rect, start + dur - c.fade_out).max(r.left());
                    painter.with_clip_rect(r).add(egui::Shape::convex_polygon(vec![Pos2::new(fx, r.top()), r.right_top(), r.right_bottom()], fade_col, Stroke::NONE));
                }
                // トランジション: 端に斜線の帯
                let tr_col = Color32::from_rgba_unmultiplied(255, 255, 255, 70);
                for (tr, at_start) in [(c.transition_in, true), (c.transition_out, false)] {
                    let Some(tr) = tr else { continue };
                    let (x0, x1) = if at_start { (r.left(), self.x_of(rect, start + tr.duration).min(r.right())) } else { (self.x_of(rect, start + dur - tr.duration).max(r.left()), r.right()) };
                    let band = Rect::from_min_max(Pos2::new(x0, r.top()), Pos2::new(x1, r.bottom()));
                    let cp = painter.with_clip_rect(band.intersect(r));
                    let mut x = band.left() - band.height();
                    while x < band.right() {
                        cp.line_segment([Pos2::new(x, band.bottom()), Pos2::new(x + band.height(), band.top())], Stroke::new(1.0, tr_col));
                        x += 6.0;
                    }
                }
                if c.mask.is_some() {
                    painter.with_clip_rect(r).text(r.right_top() + Vec2::new(-4.0, 2.0), Align2::RIGHT_TOP, "◐", FontId::proportional(11.0), Color32::from_white_alpha(200));
                }
                let name = p.assets.get(&c.asset).map(|a| a.name.as_str()).unwrap_or("?");
                painter.with_clip_rect(r).text(r.left_center() + Vec2::new(5.0, 0.0), Align2::LEFT_CENTER, name, FontId::proportional(12.0), Color32::WHITE);
                if let Some(pp) = pointer {
                    if r.contains(pp) && self.drag.is_none() {
                        let kind = if pp.x - r.left() < EDGE { DragKind::TrimStart } else if r.right() - pp.x < EDGE { DragKind::TrimEnd } else { DragKind::Move };
                        hovered_clip = Some((c.id, kind));
                    }
                }
            }
        }
        if let Some((_, k)) = hovered_clip {
            if k != DragKind::Move {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }

        // 再生位置
        let px = self.x_of(rect, playhead);
        if px >= rect.left() + HEADER_W {
            painter.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(1.5, Color32::from_rgb(255, 80, 80)));
        }

        // 操作
        if resp.drag_started() {
            if let (Some(pp), Some((id, kind))) = (resp.interact_pointer_pos(), hovered_clip) {
                let (ti, c) = p.tracks.iter().enumerate().find_map(|(i, t)| t.clips.iter().find(|c| c.id == id).map(|c| (i, c))).unwrap();
                self.drag = Some(Drag { clip: id, kind, grab: self.t_of(rect, pp.x), start: c.start, duration: c.duration, source_in: c.source_in, track: ti });
                actions.push(Action::Select(Some(id)));
            }
        }
        if resp.dragged() && self.drag.is_none() {
            if let Some(pp) = resp.interact_pointer_pos() {
                actions.push(Action::Seek(p.settings.frame_rate.snap(self.t_of(rect, pp.x))));
            }
        }
        if resp.drag_stopped() {
            if let (Some(d), Some(pp)) = (self.drag.take(), resp.interact_pointer_pos()) {
                let delta = self.t_of(rect, pp.x) - d.grab;
                let cmd = match d.kind {
                    DragKind::Move => {
                        let start = self.snap(p, rect, (d.start + delta).max(Time::ZERO), playhead, d.clip);
                        let r = ((pp.y - ruler.bottom()) / ROW_H).floor() as isize;
                        let to = (r >= 0 && (r as usize) < p.tracks.len() && r as usize != d.track && p.tracks[r as usize].kind == p.tracks[d.track].kind)
                            .then(|| p.tracks[r as usize].id);
                        (start != d.start || to.is_some()).then_some(Command::MoveClip { clip: d.clip, start, track: to })
                    }
                    DragKind::TrimStart => {
                        let s = self.snap(p, rect, d.start + delta, playhead, d.clip).max(d.start - d.source_in).min(d.start + d.duration - Time(1));
                        let shift = s - d.start;
                        (shift != Time::ZERO).then_some(Command::TrimClip { clip: d.clip, start: Some(s), duration: Some(d.duration - shift), source_in: Some(d.source_in + shift) })
                    }
                    DragKind::TrimEnd => {
                        let e = self.snap(p, rect, d.start + d.duration + delta, playhead, d.clip).max(d.start + Time(1));
                        (e - d.start != d.duration).then_some(Command::TrimClip { clip: d.clip, start: None, duration: Some(e - d.start), source_in: None })
                    }
                };
                if let Some(c) = cmd {
                    actions.push(Action::Command(c));
                }
            }
        }
        if resp.clicked() {
            if let Some(pp) = resp.interact_pointer_pos() {
                match hovered_clip {
                    Some((id, _)) => actions.push(Action::Select(Some(id))),
                    None if pp.y < ruler.bottom() || pp.x > rect.left() + HEADER_W => {
                        actions.push(Action::Select(None));
                        actions.push(Action::Seek(p.settings.frame_rate.snap(self.t_of(rect, pp.x))));
                    }
                    None => {}
                }
            }
        }
        actions
    }
}
