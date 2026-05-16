use eframe::egui::{self, StrokeKind, Color32, CornerRadius, FontId, Sense, RichText, Stroke, Margin, Align};
use crate::gui::app::InterPrepApp;
use crate::gui::types::*;
use crate::gui::theme::*;
use crate::gui::utils::*;
use egui_phosphor::regular as ph;

// ─── Chat workspace ───────────────────────────────────────────────────────────

impl InterPrepApp {
    pub fn draw_chat_workspace(&mut self, ui: &mut egui::Ui) {
        let job_info = self.selected_job().map(|j| {
            (
                j.id,
                j.company.clone(),
                j.role.clone(),
                j.location.clone(),
                j.status,
                j.avatar_color,
                j.research_status.clone(),
            )
        });

        // ── Header: design spec — height 56, padding 0 16, gap 10, bg BG ──
        //
        // The right cluster (status pill + actions) is built FIRST inside a RTL
        // outer layout. The left cluster (toggle + identity + meta) is then
        // nested LTR — it gets only the REMAINING width, so the truncate()
        // labels actually shrink instead of spilling under the right cluster.
        //
        // Responsive: hide labels < 720, hide meta < 600, hide status < 480.
        egui::Panel::top("chat_header")
            .frame(egui::Frame::new()
                .fill(BG)
                .inner_margin(egui::vec2(16.0, 0.0))
                .stroke(Stroke::NONE))
            .show_inside(ui, |ui| {
                ui.set_min_height(56.0);

                let total_w = ui.available_width();
                let show_labels = total_w > 720.0;
                let show_meta   = total_w > 600.0;
                let show_status = total_w > 480.0;

                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    if let Some((_job_id, company, role, location, status, av_color, _research_status)) = &job_info {
                        // 1) Actions — visual LTR: Mock Interview, Add Note.
                        //    In RTL layout, first added = rightmost, so iterate reversed.
                        let actions = [
                            (ph::USERS_THREE, "Mock Interview"),
                            (ph::FILE_TEXT,   "Add Note"),
                        ];
                        for (icon, label) in actions.iter().rev() {
                            let text = if show_labels {
                                format!("{icon}  {label}")
                            } else {
                                icon.to_string()
                            };
                            let clicked = ui.add(
                                egui::Button::new(
                                    RichText::new(text).size(12.0).color(INK_MUT)
                                )
                                .fill(SURFACE)
                                .corner_radius(100.0)
                                .stroke(Stroke::NONE)
                                .min_size(egui::vec2(if show_labels { 0.0 } else { 30.0 }, 30.0)),
                            ).clicked();
                            if clicked {
                                match *label {
                                    "Mock Interview" => self.start_typed_thread("Mock Interview"),
                                    _ => {}
                                }
                            }
                        }

                        // 2) Status pill (left of actions in visual order)
                        if show_status {
                            ui.add_space(4.0);
                            let sc = status.color();
                            let sl = status.label();
                            let chip_w = sl.len() as f32 * 6.5 + 18.0;
                            let (chip_r, _) = ui.allocate_exact_size(egui::vec2(chip_w, 22.0), Sense::hover());
                            if ui.is_rect_visible(chip_r) {
                                ui.painter().rect_filled(chip_r, 100.0, sc.linear_multiply(0.12));
                                ui.painter().text(chip_r.center(), egui::Align2::CENTER_CENTER,
                                    sl, FontId::proportional(11.0), sc);
                            }
                            ui.add_space(6.0);
                        }

                        // 3) Left cluster — gets only the remaining width
                        ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;

                            // Sidebar toggle
                            if ui.add(
                                egui::Button::new(RichText::new(ph::SIDEBAR_SIMPLE).size(13.0).color(INK_MUT))
                                    .fill(SURFACE)
                                    .corner_radius(100.0)
                                    .min_size(egui::vec2(30.0, 30.0))
                                    .stroke(Stroke::new(0.5, BD)),
                            ).clicked() {
                                self.sidebar_open = !self.sidebar_open;
                            }

                            // Avatar — 30x30, rounded 8, tinted bg
                            let (av, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::hover());
                            if ui.is_rect_visible(av) {
                                ui.painter().rect_filled(av, 8.0, av_color.linear_multiply(0.10));
                                ui.painter().rect_stroke(
                                    av, 8.0,
                                    Stroke::new(0.5, av_color.linear_multiply(0.30)),
                                    StrokeKind::Inside,
                                );
                                ui.painter().text(
                                    av.center(), egui::Align2::CENTER_CENTER,
                                    &company.chars().next().unwrap_or('?').to_string(),
                                    FontId::proportional(12.0), *av_color,
                                );
                            }

                            // Identity — company 13/600 + role 11/secondary, truncating
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.add(egui::Label::new(
                                    RichText::new(company).size(13.0).strong().color(INK)
                                ).truncate());
                                ui.add(egui::Label::new(
                                    RichText::new(role).size(11.0).color(INK_MUT)
                                ).truncate());
                            });

                            // Meta (location + date) — hidden when constrained
                            if show_meta {
                                if !location.is_empty() {
                                    ui.add(egui::Label::new(
                                        RichText::new(format!("{} {}", ph::MAP_PIN, location))
                                            .size(11.0).color(INK_FAINT)
                                    ).truncate());
                                }
                                ui.add(egui::Label::new(
                                    RichText::new(format!("{} Apr 18, 2026", ph::CALENDAR))
                                        .size(11.0).color(INK_FAINT)
                                ).truncate());
                            }
                        });
                    } else {
                        // No job selected — just toggle + brand
                        ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            if ui.add(
                                egui::Button::new(RichText::new(ph::SIDEBAR_SIMPLE).size(13.0).color(INK_MUT))
                                    .fill(SURFACE)
                                    .corner_radius(100.0)
                                    .min_size(egui::vec2(30.0, 30.0))
                                    .stroke(Stroke::new(0.5, BD)),
                            ).clicked() {
                                self.sidebar_open = !self.sidebar_open;
                            }
                            ui.label(RichText::new("InterPrep").size(14.0).strong().color(INK));
                        });
                    }
                });
            });

        // Hairline rule under the header
        egui::Panel::top("chat_header_rule")
            .frame(egui::Frame::new().fill(BD).stroke(Stroke::NONE))
            .show_inside(ui, |ui| { ui.set_height(1.0); });

        // ── Composer: design spec — outer padding 10/16/14, inner surface card r=20 ──
        egui::Panel::bottom("chat_composer_panel")
            .frame(egui::Frame::new()
                .fill(BG)
                .inner_margin(Margin { left: 16, right: 16, top: 10, bottom: 14 })
                .stroke(Stroke::NONE))
            .show_inside(ui, |ui| {
                // Hairline above composer (separates from messages)
                let sep = ui.allocate_space(egui::vec2(ui.available_width(), 1.0)).1;
                ui.painter().rect_filled(sep, 0.0, BD);
                ui.add_space(10.0);

                // Single rounded surface card containing textarea + action bar
                egui::Frame::new()
                    .fill(SURFACE)
                    .corner_radius(20.0)
                    .stroke(Stroke::new(0.5, BD))
                    .inner_margin(Margin { left: 16, right: 10, top: 10, bottom: 8 })
                    .show(ui, |ui| {
                        let te = ui.add(
                            egui::TextEdit::multiline(&mut self.draft)
                                .hint_text("Ask anything about this role, practice questions, review your resume…")
                                .frame(egui::Frame::new())
                                .desired_width(ui.available_width())
                                .desired_rows(2),
                        );
                        if te.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
                        {
                            self.send_message();
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            // Left: attach + mic icons (transparent, hover surface2)
                            for icon in [ph::PAPERCLIP, ph::MICROPHONE] {
                                ui.add(
                                    egui::Button::new(RichText::new(icon).size(14.0).color(INK_FAINT))
                                        .fill(Color32::TRANSPARENT)
                                        .corner_radius(100.0)
                                        .stroke(Stroke::NONE)
                                        .min_size(egui::vec2(30.0, 30.0)),
                                );
                            }
                            // Right: send button — 32x32 pill, white when has text else surface2
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                let has_text = !self.draft.trim().is_empty();
                                if ui.add(
                                    egui::Button::new(
                                        RichText::new(ph::PAPER_PLANE_RIGHT).size(13.0)
                                            .color(if has_text { BG } else { INK_FAINT })
                                    )
                                    .fill(if has_text { INK } else { SURFACE2 })
                                    .corner_radius(100.0)
                                    .min_size(egui::vec2(32.0, 32.0))
                                    .stroke(Stroke::NONE),
                                ).clicked() { self.send_message(); }
                            });
                        });
                    });

                ui.add_space(6.0);
                ui.with_layout(egui::Layout::top_down(Align::Center), |ui| {
                    ui.label(
                        RichText::new("InterPrep AI may make mistakes. Always verify important information.")
                            .size(10.5).color(INK_FAINT)
                    );
                });
            });

        // Messages or empty/new-chat state
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show_inside(ui, |ui| {
                let thread_data = self.selected_thread().map(|t| (t.messages.clone(), t.title.clone()));
                match thread_data {
                    None => self.draw_empty_chat(ui),
                    Some((messages, title)) => self.draw_message_stream(ui, &messages, &title),
                }
            });
    }

    pub fn draw_empty_chat(&mut self, ui: &mut egui::Ui) {
        // Design: gradient magenta spotlight card 160×110, radius 30, sparkle 32px
        // heading "Start a new chat" 20/700 fontDisplay, helper 13/secondary
        // 4 chips, flex-wrap, radius 100, 0.5px border
        let chips = [
            "Help me prep for system design",
            "Analyze the job description",
            "Generate behavioral questions",
            "Review my cover letter",
        ];

        ui.vertical_centered(|ui| {
            ui.add_space(60.0);

            // Spotlight card — magenta with a top sheen overlay to fake a gradient
            let (icon_rect, _) =
                ui.allocate_exact_size(egui::vec2(160.0, 110.0), Sense::hover());
            if ui.is_rect_visible(icon_rect) {
                let p = ui.painter();
                p.rect_filled(icon_rect, 30.0, MAGENTA);
                // Top sheen (rgba lighter pink) for gradient hint
                p.rect_filled(
                    egui::Rect::from_min_size(
                        icon_rect.min,
                        egui::vec2(icon_rect.width(), icon_rect.height() * 0.55),
                    ),
                    30.0,
                    Color32::from_rgba_unmultiplied(244, 114, 182, 80),
                );
                // Outer glow
                p.rect_stroke(
                    icon_rect.expand(1.5),
                    30.0,
                    Stroke::new(1.0, MAGENTA.linear_multiply(0.45)),
                    StrokeKind::Inside,
                );
                p.text(
                    icon_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    ph::SPARKLE,
                    FontId::proportional(38.0),
                    Color32::from_rgba_unmultiplied(255, 255, 255, 230),
                );
            }

            ui.add_space(20.0);
            ui.label(RichText::new("Start a new chat").size(20.0).strong().color(INK));
            ui.add_space(6.0);
            ui.label(
                RichText::new("Ask anything about your application, practice\nquestions, or get interview tips.")
                    .size(13.0).color(INK_MUT)
            );
            ui.add_space(20.0);

            // Prompt chips — flex-wrap-like, two per row, pill radius 100
            for chunk in chips.chunks(2) {
                ui.horizontal(|ui| {
                    let total_w = ui.available_width();
                    ui.add_space((total_w - 440.0).max(0.0) * 0.5);
                    for chip in chunk {
                        if ui.add(
                            egui::Button::new(
                                RichText::new(*chip).size(12.0).color(INK_MUT)
                            )
                            .fill(SURFACE)
                            .corner_radius(100.0)
                            .stroke(Stroke::new(0.5, BD))
                            .min_size(egui::vec2(210.0, 32.0)),
                        ).clicked() {
                            self.draft = chip.to_string();
                        }
                        ui.add_space(6.0);
                    }
                });
                ui.add_space(6.0);
            }
        });
    }

    pub fn draw_message_stream(&mut self, ui: &mut egui::Ui, messages: &[ChatMsg], thread_title: &str) {
        let last_ai_idx = messages.iter().rposition(|m| m.role == MsgRole::Ai);
        let is_research_thread = thread_title == "Company Research";
        let is_prep_thread     = thread_title == "Application Prep";

        // Snapshot prep-related job data once so we can render the scorecard
        // card and "Open Resume" button without re-borrowing self in the loop.
        let (prep_scorecard, prep_docx_path): (Option<serde_json::Value>, Option<String>) =
            if is_prep_thread {
                self.selected_job()
                    .map(|j| (j.scorecard.clone(), j.resume_docx_path.clone()))
                    .unwrap_or((None, None))
            } else {
                (None, None)
            };

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add_space(24.0);
                for (msg_idx, msg) in messages.iter().enumerate() {
                    match msg.role {
                        MsgRole::User => {
                            // Design: user bubble = white bg, dark text, asymmetric corners 20/20/6/20.
                            // Width: 68% of available width capped at 560 (matches the React
                            // design's `maxWidth: '68%'` rule).
                            let panel_w   = ui.available_width();
                            let bubble_max = (panel_w * 0.68).min(560.0).max(200.0);
                            ui.with_layout(
                                egui::Layout::right_to_left(Align::Min),
                                |ui| {
                                    ui.add_space(24.0);
                                    egui::Frame::new()
                                        .fill(INK)
                                        .corner_radius(CornerRadius { nw: 20, ne: 20, se: 6, sw: 20 })
                                        .inner_margin(Margin { left: 16, right: 16, top: 10, bottom: 10 })
                                        .show(ui, |ui| {
                                            ui.set_max_width(bubble_max);
                                            ui.add(egui::Label::new(
                                                RichText::new(&msg.text).size(13.0).color(BG)
                                            ).wrap());
                                        });
                                },
                            );
                            ui.add_space(10.0);
                        }
                        MsgRole::Ai => {
                            // Design: AI bubble = surface bg, asymmetric corners 6/20/20/20, sparkle avatar 26x26
                            ui.add_space(4.0);
                            ui.horizontal_top(|ui| {
                                ui.add_space(24.0);
                                // Circle avatar — 26x26, surface bg, 0.5px border, sparkle 12px ink-muted
                                let (av, _) = ui.allocate_exact_size(
                                    egui::vec2(26.0, 26.0), Sense::hover()
                                );
                                if ui.is_rect_visible(av) {
                                    ui.painter().circle_filled(av.center(), 13.0, SURFACE);
                                    ui.painter().circle_stroke(av.center(), 13.0, Stroke::new(0.5, BD));
                                    ui.painter().text(
                                        av.center(), egui::Align2::CENTER_CENTER,
                                        ph::SPARKLE,
                                        FontId::proportional(12.0), INK_MUT,
                                    );
                                }
                                ui.add_space(10.0);
                                // AI bubble width: 80% of remaining panel width
                                // (after avatar + spacing), capped at 640 to keep
                                // prose readable on very wide windows.
                                let avail_w = ui.available_width();
                                let bubble_max = (avail_w * 0.80).min(640.0).max(220.0);
                                ui.vertical(|ui| {
                                    ui.set_max_width(bubble_max);

                                    // ── Thinking / progress disclosure (above the bubble) ──
                                    // Shown only when the agent emitted stage logs. Behaves like
                                    // Claude's tool-use card / OpenAI's reasoning panel: collapsible
                                    // chevron, defaults to open while streaming.
                                    if !msg.logs.is_empty() {
                                        Self::draw_thinking_disclosure(ui, msg, msg_idx);
                                        ui.add_space(6.0);
                                    }

                                    // AI message bubble — asymmetric corners (top-left "points" to avatar)
                                    egui::Frame::new()
                                        .fill(SURFACE)
                                        .corner_radius(CornerRadius { nw: 6, ne: 20, se: 20, sw: 20 })
                                        .inner_margin(Margin { left: 16, right: 16, top: 12, bottom: 12 })
                                        .show(ui, |ui| {
                                            if msg.streaming && msg.text.trim().is_empty() {
                                                ui.label(RichText::new("…").size(14.0).color(INK_FAINT));
                                            } else if msg.streaming {
                                                // Trim trailing whitespace before appending the
                                                // cursor so the caret stays glued to the last
                                                // real character instead of floating below a
                                                // tower of blank lines.
                                                let trimmed = msg.text.trim_end();
                                                let display = format!("{trimmed}\u{258a}");
                                                render_markdown(ui, &display);
                                            } else {
                                                render_markdown(ui, &msg.text);
                                            }
                                        });
                                    ui.add_space(6.0);

                                    // Action row — only on the last AI message
                                    // Design: SURFACE2 pills, no border, radius 100
                                    if last_ai_idx == Some(msg_idx) {
                                        ui.horizontal(|ui| {
                                            for (icon, label) in [
                                                (ph::COPY, "Copy"),
                                                (ph::ARROWS_CLOCKWISE, "Regenerate"),
                                                (ph::FLOPPY_DISK, "Save"),
                                            ] {
                                                ui.add(
                                                    egui::Button::new(
                                                        RichText::new(format!("{} {}", icon, label))
                                                            .size(11.0).color(INK_MUT)
                                                    )
                                                    .fill(SURFACE2)
                                                    .corner_radius(100.0)
                                                    .stroke(Stroke::NONE)
                                                );
                                                ui.add_space(2.0);
                                            }
                                        });

                                        // "View Full Report" — only in a finished Company Research thread
                                        if is_research_thread && !msg.streaming && !msg.text.is_empty() {
                                            ui.add_space(8.0);
                                            let btn = ui.add(
                                                egui::Button::new(
                                                    RichText::new(
                                                        format!("{}  View Full Report", ph::ARTICLE)
                                                    )
                                                    .size(12.5)
                                                    .color(INK)
                                                )
                                                .fill(SURFACE2)
                                                .corner_radius(20.0)
                                                .stroke(Stroke::new(1.0, MAGENTA.linear_multiply(0.5)))
                                                .min_size(egui::vec2(0.0, 34.0)),
                                            );
                                            if btn.clicked() {
                                                self.show_report_modal = true;
                                                self.report_modal_text = msg.text.clone();
                                            }
                                        }

                                        // Application-Prep extras: scorecard card + action buttons.
                                        if is_prep_thread && !msg.streaming {
                                            if let Some(card) = &prep_scorecard {
                                                ui.add_space(8.0);
                                                Self::draw_scorecard_card(ui, card);
                                            }

                                            // Side-by-side action buttons: Open Resume + Simulate KO Screen
                                            let has_docx = prep_docx_path.is_some();
                                            let has_resume_text = self.selected_job()
                                                .and_then(|j| j.tailored_resume.as_ref())
                                                .map(|s| !s.trim().is_empty())
                                                .unwrap_or(false);
                                            if has_docx || has_resume_text {
                                                ui.add_space(8.0);
                                                ui.horizontal(|ui| {
                                                    if let Some(path) = &prep_docx_path {
                                                        let btn = ui.add(
                                                            egui::Button::new(
                                                                RichText::new(
                                                                    format!("{}  Open Tailored Resume", ph::FILE_TEXT)
                                                                )
                                                                .size(12.5).color(INK)
                                                            )
                                                            .fill(SURFACE2)
                                                            .corner_radius(20.0)
                                                            .stroke(Stroke::new(1.0, MAGENTA.linear_multiply(0.5)))
                                                            .min_size(egui::vec2(0.0, 34.0)),
                                                        );
                                                        if btn.clicked() {
                                                            if let Err(e) = open::that(path) {
                                                                eprintln!("Could not open {path}: {e}");
                                                            }
                                                        }
                                                        ui.add_space(6.0);
                                                    }
                                                    if has_resume_text {
                                                        let btn = ui.add(
                                                            egui::Button::new(
                                                                RichText::new(
                                                                    format!("{}  Simulate Knockout Screen", ph::TARGET)
                                                                )
                                                                .size(12.5).color(INK)
                                                            )
                                                            .fill(SURFACE2)
                                                            .corner_radius(20.0)
                                                            .stroke(Stroke::new(1.0, MAGENTA.linear_multiply(0.5)))
                                                            .min_size(egui::vec2(0.0, 34.0)),
                                                        );
                                                        if btn.clicked() {
                                                            if let Some(jid) = self.selected_job {
                                                                self.start_knockout_screen(jid);
                                                            }
                                                        }
                                                    }
                                                });
                                            }
                                        }
                                    }
                                });
                            });
                            ui.add_space(14.0);
                        }
                    }
                }
                ui.add_space(24.0);
            });
    }

    /// Collapsible "Thinking" / agent-progress disclosure rendered above the
    /// AI bubble. Defaults to open while the message is still streaming so the
    /// user can watch progress, and stays in whatever state the user leaves it
    /// when the stream finishes.
    fn draw_thinking_disclosure(ui: &mut egui::Ui, msg: &ChatMsg, msg_idx: usize) {
        let logs_id = egui::Id::new(("ai_thinking", msg_idx));
        let mut open = ui.data_mut(|d| {
            *d.get_temp_mut_or_insert_with::<bool>(logs_id, || true)
        });

        let count = msg.logs.len();
        let plural = if count == 1 { "" } else { "s" };
        let title = if msg.streaming {
            format!("Thinking… · {count} step{plural}")
        } else {
            format!("Thought for {count} step{plural}")
        };
        let chev = if open { ph::CARET_DOWN } else { ph::CARET_RIGHT };

        egui::Frame::new()
            .fill(BG)
            .corner_radius(10.0)
            .stroke(Stroke::new(0.5, BD))
            .inner_margin(Margin { left: 12, right: 12, top: 8, bottom: 8 })
            .show(ui, |ui| {
                // Header — clickable to toggle. Whole row reacts.
                let header = ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new(chev).size(11.0).color(INK_FAINT));
                    ui.label(RichText::new(title).size(12.0).color(INK_MUT).strong());

                    // Preview the latest log line when collapsed, like ChatGPT.
                    if !open {
                        if let Some(last) = msg.logs.last() {
                            ui.add_space(4.0);
                            let preview: String = last.chars().take(72).collect();
                            ui.add(egui::Label::new(
                                RichText::new(preview).size(11.0).color(INK_FAINT)
                            ).truncate());
                        }
                    }
                }).response;

                // Make the whole header row clickable, not just the labels.
                let click = ui.interact(header.rect, logs_id.with("hdr"), Sense::click());
                if click.clicked() {
                    open = !open;
                    ui.data_mut(|d| d.insert_temp(logs_id, open));
                }

                if open {
                    ui.add_space(6.0);
                    for line in &msg.logs {
                        Self::render_log_entry(ui, line);
                    }
                }
            });
    }

    /// Renders a single agent-progress log line. Each log entry is normalised
    /// before display because the backend sometimes pads events with stray
    /// `.`, `---` separators and blank lines meant for terminal output; those
    /// would otherwise blow up the disclosure with huge vertical gaps.
    /// Inline `**bold**` markers in the cleaned text are rendered as bold.
    fn render_log_entry(ui: &mut egui::Ui, raw: &str) {
        let cleaned: String = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty()
                && !l.chars().all(|c| c == '-' || c == '.' || c == '_' || c == '*'))
            .collect::<Vec<_>>()
            .join(" ");

        if cleaned.is_empty() {
            return;
        }

        ui.horizontal_wrapped(|ui| {
            // y-spacing handles wrap-line gap; x-spacing 0 so split segments
            // butt against each other (spaces are baked into the segments).
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 2.0);
            ui.label(RichText::new("·  ").size(11.5).color(INK_FAINT));

            for (i, part) in cleaned.split("**").enumerate() {
                if part.is_empty() {
                    continue;
                }
                let rt = RichText::new(part).size(11.5);
                ui.label(if i % 2 == 1 {
                    rt.strong().color(INK)
                } else {
                    rt.color(INK_MUT)
                });
            }
        });
        ui.add_space(3.0);
    }

    /// Compact ATS-style scorecard rendered below the cover-letter bubble.
    /// Pulls fields from the JSON the backend returned; gracefully falls back
    /// to "—" when a key is missing or has an unexpected shape.
    fn draw_scorecard_card(ui: &mut egui::Ui, card: &serde_json::Value) {
        let verbatim = card["verbatim_match_score"]
            .as_i64()
            .map(|n| format!("{n}%"))
            .unwrap_or_else(|| "—".to_owned());
        let title_align = card["role_title_alignment"].as_str().unwrap_or("—").to_owned();
        let quant       = card["quantification_check"].as_str().unwrap_or("—").to_owned();
        let recommend   = card["hire_recommendation"].as_str().unwrap_or("—").to_owned();

        let skills: Vec<String> = card["skills_matched"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();

        let recommend_color = if recommend.eq_ignore_ascii_case("Hire") {
            C_OFFER
        } else if recommend.eq_ignore_ascii_case("No Hire") {
            C_REJECTED
        } else {
            INK_MUT
        };

        egui::Frame::new()
            .fill(SURFACE)
            .corner_radius(12.0)
            .stroke(Stroke::new(1.0, BD))
            .inner_margin(Margin { left: 14, right: 14, top: 12, bottom: 12 })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ph::CHART_BAR).size(14.0).color(MAGENTA));
                    ui.add_space(6.0);
                    ui.label(RichText::new("ATS Scorecard").size(12.5).strong().color(INK));
                });
                ui.add_space(8.0);

                // Two columns of metric pairs
                let cell = |ui: &mut egui::Ui, label: &str, value: &str, color: Color32| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(label).size(10.5).color(INK_FAINT));
                        ui.add_space(1.0);
                        ui.label(RichText::new(value).size(13.0).strong().color(color));
                    });
                };

                ui.horizontal(|ui| {
                    cell(ui, "Verbatim Match",     &verbatim,    INK);
                    ui.add_space(24.0);
                    cell(ui, "Role Title Aligned", &title_align, INK);
                    ui.add_space(24.0);
                    cell(ui, "Quantification",     &quant,       INK);
                    ui.add_space(24.0);
                    cell(ui, "Recommendation",     &recommend,   recommend_color);
                });

                if !skills.is_empty() {
                    ui.add_space(10.0);
                    ui.label(RichText::new("Skills matched").size(10.5).color(INK_FAINT));
                    ui.add_space(3.0);
                    // Pre-truncate skill names to a hard char limit BEFORE handing
                    // them to egui. This guarantees each pill is bounded — egui's
                    // wrap layout reliably breaks rows when no pill can exceed
                    // the row width. `wrap_mode = Extend` keeps each pill single-line.
                    const MAX_SKILL_CHARS: usize = 28;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                        for s in &skills {
                            let display: String = if s.chars().count() > MAX_SKILL_CHARS {
                                let truncated: String = s.chars().take(MAX_SKILL_CHARS - 1).collect();
                                format!("{truncated}…")
                            } else {
                                s.clone()
                            };
                            egui::Frame::new()
                                .fill(SURFACE2)
                                .corner_radius(100.0)
                                .inner_margin(Margin { left: 8, right: 8, top: 3, bottom: 3 })
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&display).size(11.0).color(INK_MUT)
                                        )
                                        .wrap_mode(egui::TextWrapMode::Extend),
                                    );
                                });
                        }
                    });
                }
            });
    }
}

