use eframe::egui::{self, StrokeKind, Color32, FontId, Sense, RichText, Stroke, Margin, Align};
use crate::gui::app::InterPrepApp;
use crate::gui::types::*;
use crate::gui::theme::*;
use crate::gui::utils::*;
use egui_phosphor::regular as ph;

// ─── Timeline (Gantt) ─────────────────────────────────────────────────────────

impl InterPrepApp {
    pub fn draw_timeline_workspace(&mut self, ui: &mut egui::Ui) {
        // Layout
        const LEFT_COL:    f32 = 220.0;
        const DAY_W:       f32 = 36.0;
        const ROW_H:       f32 = 64.0;
        const PILL_H:      f32 = 26.0;
        const MONTH_H:     f32 = 22.0;
        const DOW_H:       f32 = 18.0;
        const DAY_NUM_H:   f32 = 28.0;
        const HEADER_H:    f32 = MONTH_H + DOW_H + DAY_NUM_H; // 68

        // Compute a date-range string for the toolbar based on a 60-day window.
        let range_start = self.gantt_offset;
        let range_end   = self.gantt_offset + 60;
        let (_, ds, mns) = day_to_month_day(range_start);
        let (_, de, mne) = day_to_month_day(range_end);
        let date_range_text = format!("{mns} {ds} – {mne} {de}");

        // ── Top toolbar ────────────────────────────────────────────────────
        // Design: 52 high, BG, border-bottom BD, padding 0 16, gap 8
        egui::Panel::top("gantt_top")
            .frame(egui::Frame::new()
                .fill(BG)
                .inner_margin(egui::vec2(16.0, 0.0))
                .stroke(Stroke::NONE))
            .show_inside(ui, |ui| {
                ui.set_min_height(52.0);
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    // Sidebar toggle — 30x30 pill, SURFACE bg, 0.5px border
                    if ui.add(
                        egui::Button::new(RichText::new(ph::SIDEBAR_SIMPLE).size(13.0).color(INK_MUT))
                            .fill(SURFACE).corner_radius(100.0)
                            .min_size(egui::vec2(30.0, 30.0))
                            .stroke(Stroke::new(0.5, BD)),
                    ).clicked() { self.sidebar_open = !self.sidebar_open; }

                    ui.label(RichText::new("Timeline").size(14.0).strong().color(INK));
                    ui.add_space(4.0);

                    // Segmented range nav: ◀ "May 1 – Jul 1" ▶
                    egui::Frame::new()
                        .fill(SURFACE)
                        .corner_radius(100.0)
                        .stroke(Stroke::new(0.5, BD))
                        .inner_margin(Margin { left: 3, right: 3, top: 3, bottom: 3 })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if ui.add(
                                    egui::Button::new(RichText::new(ph::CARET_LEFT).size(13.0).color(INK_MUT))
                                        .fill(Color32::TRANSPARENT).stroke(Stroke::NONE)
                                        .corner_radius(100.0)
                                        .min_size(egui::vec2(26.0, 26.0))
                                ).clicked() { self.gantt_offset -= 7; }
                                ui.add(egui::Label::new(
                                    RichText::new(&date_range_text).size(12.0).color(INK_MUT)
                                ));
                                if ui.add(
                                    egui::Button::new(RichText::new(ph::CARET_RIGHT).size(13.0).color(INK_MUT))
                                        .fill(Color32::TRANSPARENT).stroke(Stroke::NONE)
                                        .corner_radius(100.0)
                                        .min_size(egui::vec2(26.0, 26.0))
                                ).clicked() { self.gantt_offset += 7; }
                            });
                        });

                    // Today — transparent pill with 0.5px border
                    if ui.add(
                        egui::Button::new(RichText::new("Today").size(12.0).color(INK_MUT))
                            .fill(Color32::TRANSPARENT)
                            .corner_radius(100.0)
                            .stroke(Stroke::new(0.5, BD))
                            .min_size(egui::vec2(58.0, 28.0))
                    ).clicked() { self.gantt_offset = -14; }

                    // Add Job — white pill on the right
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(
                            egui::Button::new(
                                RichText::new(format!("{}  Add Job", ph::PLUS))
                                    .size(12.0).color(BG).strong()
                            )
                            .fill(INK).corner_radius(100.0)
                            .stroke(Stroke::NONE)
                            .min_size(egui::vec2(96.0, 28.0))
                        ).clicked() { self.show_new_job = true; }
                    });
                });
            });

        // Hairline under header
        egui::Panel::top("gantt_top_rule")
            .frame(egui::Frame::new().fill(BD).stroke(Stroke::NONE))
            .show_inside(ui, |ui| { ui.set_height(1.0); });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show_inside(ui, |ui| {
                let avail = ui.available_rect_before_wrap();
                let visible_days = ((avail.width() - LEFT_COL) / DAY_W).ceil() as i32 + 2;
                let painter = ui.painter();

                let day_x = |day_off: i32| -> f32 {
                    avail.min.x + LEFT_COL + (day_off - self.gantt_offset) as f32 * DAY_W
                };
                let day_center_x = |day_off: i32| day_x(day_off) + DAY_W * 0.5;

                // ── Today blue rectangle (under day-num + dow rows) ───────
                let today_visible = -self.gantt_offset >= 0
                    && (-self.gantt_offset) < visible_days;
                if today_visible {
                    let cx = day_center_x(0);
                    let blue_y_top = avail.min.y + MONTH_H + 2.0;
                    let blue_y_bot = avail.min.y + HEADER_H - 2.0;
                    let blue_w = DAY_W * 0.78;
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(cx - blue_w * 0.5, blue_y_top),
                            egui::pos2(cx + blue_w * 0.5, blue_y_bot),
                        ),
                        4.0,
                        ACCENT,
                    );
                }

                // ── Header rows ──────────────────────────────────────────
                // Vertical grid lines + day-of-week + day-number
                for d in 0..visible_days {
                    let day_off = self.gantt_offset + d;
                    let (_m, day_num, _mn) = day_to_month_day(day_off);
                    let x = avail.min.x + LEFT_COL + d as f32 * DAY_W;
                    let cx = x + DAY_W * 0.5;
                    let is_today = day_off == 0;

                    // Day-of-week letter (row 2)
                    painter.text(
                        egui::pos2(cx, avail.min.y + MONTH_H + DOW_H * 0.5),
                        egui::Align2::CENTER_CENTER,
                        day_of_week_letter(day_off),
                        FontId::proportional(10.0),
                        if is_today { INK } else { INK_FAINT },
                    );
                    // Day number (row 3)
                    painter.text(
                        egui::pos2(cx, avail.min.y + MONTH_H + DOW_H + DAY_NUM_H * 0.5),
                        egui::Align2::CENTER_CENTER,
                        &day_num.to_string(),
                        FontId::proportional(11.5),
                        if is_today { INK } else { INK_MUT },
                    );

                    // Vertical grid line (through job rows AND add job row)
                    let rows_bottom = avail.min.y + HEADER_H + self.jobs.len() as f32 * ROW_H + 40.0;
                    painter.line_segment(
                        [
                            egui::pos2(x, avail.min.y + HEADER_H),
                            egui::pos2(x, rows_bottom),
                        ],
                        Stroke::new(0.5, BD),
                    );
                }

                // Month labels (row 1 — only at month boundaries)
                let mut last_month: u8 = 0;
                for d in 0..visible_days {
                    let day_off = self.gantt_offset + d;
                    let (month_num, _day_num, month_name) = day_to_month_day(day_off);
                    let x = avail.min.x + LEFT_COL + d as f32 * DAY_W;
                    if month_num != last_month {
                        last_month = month_num;
                        painter.text(
                            egui::pos2(x + 6.0, avail.min.y + MONTH_H * 0.5),
                            egui::Align2::LEFT_CENTER,
                            &format!("{} 2026", month_name.to_uppercase()),
                            FontId::proportional(10.5),
                            INK_FAINT,
                        );
                    }
                }

                // Today vertical accent line (extends through job rows and add job row)
                if today_visible {
                    let tx = day_center_x(0);
                    let num_rows = self.jobs.len() as f32;
                    let line_bottom = avail.min.y + HEADER_H + num_rows * ROW_H + 40.0;
                    painter.line_segment(
                        [
                            egui::pos2(tx, avail.min.y + HEADER_H),
                            egui::pos2(tx, line_bottom),
                        ],
                        Stroke::new(1.5, ACCENT.linear_multiply(0.5)),
                    );
                }

                // ── Left column background + border ──────────────────────
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        avail.min,
                        egui::vec2(LEFT_COL, HEADER_H + self.jobs.len() as f32 * ROW_H + 40.0), // 40.0 is Add job row height
                    ),
                    0.0, BG,
                );
                painter.line_segment(
                    [
                        egui::pos2(avail.min.x + LEFT_COL, avail.min.y),
                        egui::pos2(avail.min.x + LEFT_COL, avail.min.y + HEADER_H + self.jobs.len() as f32 * ROW_H + 40.0),
                    ],
                    Stroke::new(1.0, BD),
                );

                // Header bottom separator (top of first row, spanning entire width)
                painter.line_segment(
                    [
                        egui::pos2(avail.min.x, avail.min.y + HEADER_H),
                        egui::pos2(avail.max.x, avail.min.y + HEADER_H),
                    ],
                    Stroke::new(1.0, BD),
                );

                // ── Job rows ─────────────────────────────────────────────
                let mut clicked_stage: Option<(usize, usize, egui::Pos2)> = None;

                let jobs_snap: Vec<(usize, String, String, Color32, usize, Vec<StageNote>)> =
                    self.jobs.iter().map(|j| (
                        j.id, j.company.clone(), j.role.clone(),
                        j.avatar_color, j.current_stage,
                        j.stage_notes.clone(),
                    )).collect();

                for (row_idx, (jid, company, role, av_col, cur_stage, stage_notes))
                    in jobs_snap.iter().enumerate()
                {
                    let row_y = avail.min.y + HEADER_H + row_idx as f32 * ROW_H;
                    let track_y = row_y + ROW_H * 0.5;

                    // Row separator
                    painter.line_segment(
                        [
                            egui::pos2(avail.min.x, row_y + ROW_H),
                            egui::pos2(avail.max.x, row_y + ROW_H),
                        ],
                        Stroke::new(0.5, BD),
                    );

                    // ── Left column ────────────────────────────────────
                    let av_size = 30.0;
                    let av_rect = egui::Rect::from_min_size(
                        egui::pos2(avail.min.x + 14.0, track_y - av_size * 0.5),
                        egui::vec2(av_size, av_size),
                    );
                    painter.rect_filled(av_rect, 8.0, *av_col);
                    painter.text(
                        av_rect.center(), egui::Align2::CENTER_CENTER,
                        &company.chars().next().unwrap_or('?').to_string(),
                        FontId::proportional(13.0), INK,
                    );

                    let tx = avail.min.x + 14.0 + av_size + 10.0;
                    painter.text(
                        egui::pos2(tx, track_y - 8.0),
                        egui::Align2::LEFT_CENTER,
                        company,
                        FontId::proportional(13.0),
                        INK,
                    );
                    let role_short: String = role.chars().take(20).collect();
                    painter.text(
                        egui::pos2(tx, track_y + 8.0),
                        egui::Align2::LEFT_CENTER,
                        &role_short,
                        FontId::proportional(11.0),
                        INK_MUT,
                    );

                    // ── Stage pills ────────────────────────────────────
                    let last_idx = stage_notes.len().saturating_sub(1);
                    for (si, note) in stage_notes.iter().enumerate() {
                        let col = STAGE_COLS[si % STAGE_COLS.len()];

                        // Determine end day
                        let end_day = if si < last_idx {
                            stage_notes[si + 1].day_offset
                        } else if si == *cur_stage {
                            // current/last stage extends to today
                            0
                        } else {
                            note.day_offset + 14
                        };

                        let raw_start = day_center_x(note.day_offset);
                        let raw_end   = day_center_x(end_day);

                        // Clip to visible track area
                        let track_left  = avail.min.x + LEFT_COL + 4.0;
                        let track_right = avail.max.x - 4.0;
                        let pill_x1 = raw_start.max(track_left);
                        let pill_x2 = raw_end.min(track_right);
                        if pill_x2 <= pill_x1 + 4.0 { continue; }

                        let pill_rect = egui::Rect::from_min_max(
                            egui::pos2(pill_x1, track_y - PILL_H * 0.5),
                            egui::pos2(pill_x2, track_y + PILL_H * 0.5),
                        );

                        // Capsule bg (semi-transparent)
                        painter.rect_filled(pill_rect, PILL_H * 0.5, col.linear_multiply(0.15));
                        // Capsule border
                        painter.rect_stroke(
                            pill_rect, PILL_H * 0.5,
                            Stroke::new(1.0, col.linear_multiply(0.25)),
                            StrokeKind::Inside,
                        );

                        // Stage dot at left edge
                        let pill_w = pill_x2 - pill_x1;
                        if pill_w > 14.0 {
                            painter.circle_filled(
                                egui::pos2(pill_x1 + PILL_H * 0.5, track_y),
                                5.0,
                                col,
                            );
                        }

                        // Stage label inside (centred or left-anchored)
                        let label = STAGES[si % STAGES.len()];
                        if pill_w > 60.0 {
                            painter.text(
                                egui::pos2(pill_x1 + PILL_H * 0.5 + 14.0, track_y),
                                egui::Align2::LEFT_CENTER,
                                label,
                                FontId::proportional(11.5),
                                col,
                            );
                        } else if pill_w > 30.0 {
                            let s: String = label.chars().take(3).collect();
                            painter.text(
                                egui::pos2(pill_rect.center().x, track_y),
                                egui::Align2::CENTER_CENTER,
                                &(s + "…"),
                                FontId::proportional(10.5),
                                col,
                            );
                        }

                        // Click target on the pill
                        if ui.interact(
                            pill_rect,
                            egui::Id::new(("pill", jid, si)),
                            Sense::click(),
                        ).clicked() {
                            clicked_stage = Some((
                                *jid, si,
                                egui::pos2(pill_rect.center().x, track_y + PILL_H + 6.0),
                            ));
                        }
                    }

                    // ── Empty next-stage circle ────────────────────────
                    if *cur_stage + 1 < STAGES.len() {
                        // Position it 1 day after today (since current stage extends to today)
                        let next_x = day_center_x(1);
                        let next_r = 12.0;
                        if next_x > avail.min.x + LEFT_COL + next_r
                            && next_x < avail.max.x - next_r
                        {
                            let center = egui::pos2(next_x, track_y);
                            painter.circle_filled(
                                center,
                                next_r,
                                Color32::from_rgb(28, 28, 28),
                            );

                            // Dashed border
                            let dash_len = 3.0;
                            let gap_len = 3.0;
                            let circumference = std::f32::consts::TAU * next_r;
                            let num_dashes = (circumference / (dash_len + gap_len)) as i32;
                            for i in 0..num_dashes {
                                let angle1 = i as f32 * (std::f32::consts::TAU / num_dashes as f32);
                                let angle2 = angle1 + (dash_len / circumference) * std::f32::consts::TAU;
                                painter.line_segment(
                                    [
                                        center + egui::vec2(angle1.cos() * next_r, angle1.sin() * next_r),
                                        center + egui::vec2(angle2.cos() * next_r, angle2.sin() * next_r),
                                    ],
                                    Stroke::new(1.0, BD),
                                );
                            }

                            // Plus icon in the middle
                            painter.text(
                                center,
                                egui::Align2::CENTER_CENTER,
                                egui_phosphor::regular::PLUS,
                                FontId::proportional(12.0),
                                INK_MUT,
                            );
                        }
                    }
                }

                // ── "+ Add job" row at bottom of left column ─────────────
                let add_y = avail.min.y + HEADER_H + jobs_snap.len() as f32 * ROW_H;
                if add_y + 36.0 < avail.max.y {
                    let add_rect = egui::Rect::from_min_size(
                        egui::pos2(avail.min.x, add_y),
                        egui::vec2(LEFT_COL, 40.0),
                    );
                    let add_resp = ui.interact(
                        add_rect,
                        egui::Id::new("add_job_row"),
                        Sense::click(),
                    );
                    if add_resp.hovered() {
                        ui.painter().rect_filled(add_rect, 0.0, SURFACE2);
                    }
                    ui.painter().text(
                        egui::pos2(add_rect.left() + 16.0, add_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "+",
                        FontId::proportional(15.0),
                        INK_FAINT,
                    );
                    ui.painter().text(
                        egui::pos2(add_rect.left() + 32.0, add_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "Add job",
                        FontId::proportional(12.5),
                        INK_FAINT,
                    );
                    if add_resp.clicked() {
                        self.show_new_job = true;
                    }
                    
                    // Horizontal separator under "+ Add job" row
                    ui.painter().line_segment(
                        [
                            egui::pos2(avail.min.x, add_rect.bottom()),
                            egui::pos2(avail.max.x, add_rect.bottom()),
                        ],
                        Stroke::new(0.5, BD),
                    );
                }

                if let Some((jid, si, pos)) = clicked_stage {
                    if let Some(ji) = self.jobs.iter().position(|j| j.id == jid) {
                        self.se_job     = Some(ji);
                        self.se_stage   = Some(si);
                        self.se_outcome = self.jobs[ji].stage_notes[si].outcome.clone();
                        self.se_notes   = self.jobs[ji].stage_notes[si].notes.clone();
                        self.se_popover_pos = pos;
                    }
                }

                if self.se_job.is_some() {
                    self.draw_stage_editor(ui.ctx());
                }
            });
    }

    pub fn draw_stage_editor(&mut self, ctx: &egui::Context) {
        let mut still_open = self.se_job.is_some();
        egui::Window::new("stage_editor_win")
            .title_bar(false)
            .fixed_size([300.0, 230.0])
            .fixed_pos(self.se_popover_pos)
            .frame(
                egui::Frame::new()
                    .fill(SURFACE2)
                    .corner_radius(12.0)
                    .stroke(Stroke::new(1.0, BD))
                    .inner_margin(egui::vec2(16.0, 14.0))
            )
            .show(ctx, |ui| {
                let ji = match self.se_job   { Some(j) => j, None => return };
                let si = match self.se_stage { Some(s) => s, None => return };
                let stage_name = STAGES.get(si).copied().unwrap_or("Stage");
                let col = STAGE_COLS[si % STAGE_COLS.len()];

                ui.horizontal(|ui| {
                    ui.label(RichText::new(stage_name).size(13.5).strong().color(col));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(
                            egui::Button::new(RichText::new("✕").size(13.0).color(INK_FAINT))
                                .fill(Color32::TRANSPARENT).stroke(Stroke::NONE)
                        ).clicked() { still_open = false; }
                    });
                });
                ui.add_space(10.0);
                ui.label(RichText::new("Outcome").size(11.5).color(INK_MUT));
                egui::ComboBox::from_id_salt("se_outcome")
                    .selected_text(&self.se_outcome)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for opt in &["Submitted", "Passed", "In progress", "Scheduled", "Rejected", "Offer received"] {
                            ui.selectable_value(&mut self.se_outcome, opt.to_string(), *opt);
                        }
                    });
                ui.add_space(8.0);
                ui.label(RichText::new("Notes").size(11.5).color(INK_MUT));
                ui.add(
                    egui::TextEdit::multiline(&mut self.se_notes)
                        .desired_rows(3)
                        .desired_width(ui.available_width())
                );
                ui.add_space(12.0);
                if ui.add(
                    egui::Button::new(RichText::new("Save").size(13.0).color(BG).strong())
                        .fill(INK).corner_radius(8.0)
                        .min_size(egui::vec2(ui.available_width(), 32.0))
                ).clicked() {
                    self.jobs[ji].stage_notes[si].outcome = self.se_outcome.clone();
                    self.jobs[ji].stage_notes[si].notes   = self.se_notes.clone();
                    if si >= self.jobs[ji].current_stage {
                        self.jobs[ji].current_stage = si;
                    }
                    still_open = false;
                }
            });
        if !still_open {
            self.se_job   = None;
            self.se_stage = None;
        }
    }
}

