use eframe::egui::{self, Color32, FontId, Sense, RichText, Stroke, Margin, Align};
use crate::gui::app::InterPrepApp;
use crate::gui::types::*;
use crate::gui::theme::*;
use crate::gui::utils::render_markdown;
use egui_phosphor::regular as ph;

/// Password/key field with show/hide toggle button.
fn key_field(ui: &mut egui::Ui, value: &mut String, visible: &mut bool, hint: &str) {
    ui.horizontal(|ui| {
        let btn_w   = 36.0f32;
        let gap     = 6.0f32;
        let field_w = ui.available_width() - btn_w - gap;
        ui.add(
            egui::TextEdit::singleline(value)
                .password(!*visible)
                .hint_text(hint)
                .desired_width(field_w)
                .margin(egui::vec2(10.0, 8.0)),
        );
        ui.add_space(gap);
        let eye = if *visible { ph::EYE_SLASH } else { ph::EYE };
        if ui.add(
            egui::Button::new(RichText::new(eye).size(14.0).color(INK_MUT))
                .fill(SURFACE2)
                .corner_radius(8.0)
                .min_size(egui::vec2(btn_w, 36.0))
                .stroke(Stroke::NONE),
        ).clicked() {
            *visible = !*visible;
        }
    });
}

// ─── New Job modal ────────────────────────────────────────────────────────────

impl InterPrepApp {
    pub fn draw_new_job_modal(&mut self, ctx: &egui::Context) {
        ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("nj_bd")))
            .rect_filled(ctx.viewport_rect(), 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 160));

        let screen_h = ctx.viewport_rect().height();
        let modal_h = (screen_h - 80.0).min(680.0).max(380.0);

        egui::Window::new("new_job_modal_win")
            .title_bar(false)
            .fixed_size([460.0, modal_h])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::new()
                    .fill(SURFACE)
                    .corner_radius(16.0)
                    .stroke(Stroke::new(1.0, BD))
                    .inner_margin(egui::vec2(28.0, 24.0))
            )
            .show(ctx, |ui| {
                // Cannot create a job without at least one master resume —
                // the tailoring step needs something to start from, and
                // company research uses the tailored resume as context.
                let has_resume = !self.resume_store.is_empty();

                // ── Footer buttons — declared first so the central panel respects its height ──
                egui::Panel::bottom("nj_footer")
                    .frame(egui::Frame::new())
                    .resizable(false)
                    .show_inside(ui, |ui| {
                        if !has_resume {
                            ui.label(
                                RichText::new(
                                    "Add at least one master resume in Settings → Resume \
                                     before creating a job."
                                )
                                .size(11.5)
                                .color(C_REJECTED),
                            );
                            ui.add_space(8.0);
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let cancel_w = (ui.available_width() - 12.0) * 0.33;
                            let create_w = ui.available_width() - cancel_w - 12.0;
                            if ui.add(
                                egui::Button::new(RichText::new("Cancel").size(14.0).color(INK_MUT))
                                    .fill(SURFACE2).corner_radius(20.0)
                                    .min_size(egui::vec2(cancel_w, 44.0))
                                    .stroke(Stroke::NONE)
                            ).clicked() { self.show_new_job = false; }
                            ui.add_space(8.0);

                            let (create_fill, create_color) = if has_resume {
                                (INK, BG)
                            } else {
                                (SURFACE3, INK_FAINT)
                            };
                            let create_btn = ui.add_enabled(
                                has_resume,
                                egui::Button::new(
                                    RichText::new("Create Job").size(14.0).color(create_color).strong()
                                )
                                .fill(create_fill)
                                .corner_radius(20.0)
                                .min_size(egui::vec2(create_w, 44.0))
                                .stroke(Stroke::NONE),
                            );
                            if create_btn.clicked() { self.create_job(); }
                        });
                    });

                // ── Header — title + close + optional error ──
                egui::Panel::top("nj_header")
                    .frame(egui::Frame::new())
                    .resizable(false)
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Add New Job").size(20.0).strong().color(INK));
                                ui.add_space(2.0);
                                ui.label(RichText::new("Track a new opportunity").size(13.0).color(INK_MUT));
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(
                                    egui::Button::new(RichText::new("✕").size(12.0).color(INK_MUT))
                                        .fill(SURFACE2).corner_radius(20.0)
                                        .min_size(egui::vec2(28.0, 28.0)).stroke(Stroke::NONE)
                                ).clicked() { self.show_new_job = false; }
                            });
                        });
                        if let Some(err) = &self.nj_error.clone() {
                            ui.add_space(8.0);
                            ui.label(RichText::new(err).size(12.5).color(C_REJECTED));
                        }
                        ui.add_space(16.0);
                    });

                // ── Scrollable form fields ──
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .id_salt("nj_form_scroll")
                            .show(ui, |ui| {
                                ui.add_space(4.0);

                                // Company + Role
                                ui.horizontal(|ui| {
                                    let half_w = (ui.available_width() - 16.0) / 2.0;
                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 2.0;
                                            ui.label(RichText::new("Company Name").size(13.0).color(INK_MUT));
                                            ui.label(RichText::new("*").size(13.0).color(C_REJECTED));
                                        });
                                        ui.add_space(6.0);
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.nj_company)
                                                .hint_text("e.g. Google")
                                                .desired_width(half_w)
                                                .margin(egui::vec2(12.0, 10.0))
                                        );
                                    });
                                    ui.add_space(8.0);
                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 2.0;
                                            ui.label(RichText::new("Role Title").size(13.0).color(INK_MUT));
                                            ui.label(RichText::new("*").size(13.0).color(C_REJECTED));
                                        });
                                        ui.add_space(6.0);
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.nj_role)
                                                .hint_text("e.g. Software Engineer")
                                                .desired_width(half_w)
                                                .margin(egui::vec2(12.0, 10.0))
                                        );
                                    });
                                });
                                ui.add_space(16.0);

                                // Location + Status
                                ui.horizontal(|ui| {
                                    let half_w = (ui.available_width() - 16.0) / 2.0;
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new("Location").size(13.0).color(INK_MUT));
                                        ui.add_space(6.0);
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.nj_location)
                                                .hint_text("City, State")
                                                .desired_width(half_w)
                                                .margin(egui::vec2(12.0, 10.0))
                                        );
                                    });
                                    ui.add_space(8.0);
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new("Status").size(13.0).color(INK_MUT));
                                        ui.add_space(6.0);
                                        ui.spacing_mut().button_padding = egui::vec2(12.0, 10.0);
                                        egui::ComboBox::from_id_salt("nj_status")
                                            .selected_text(self.nj_status.label())
                                            .width(half_w)
                                            .show_ui(ui, |ui| {
                                                for s in [
                                                    JobStatus::Applied, JobStatus::Screening,
                                                    JobStatus::Technical, JobStatus::Offer, JobStatus::Rejected,
                                                ] {
                                                    ui.selectable_value(&mut self.nj_status, s, s.label());
                                                }
                                            });
                                    });
                                });
                                ui.add_space(16.0);

                                // Job Description — same style as other text fields
                                ui.label(RichText::new("Job Description").size(13.0).color(INK_MUT));
                                ui.add_space(6.0);
                                let jd_edit = egui::TextEdit::multiline(&mut self.nj_job_description)
                                    .hint_text("Paste the JD here so InterPrep can research the company, process, policies, reviews, and likely questions.")
                                    .desired_width(ui.available_width())
                                    .margin(egui::vec2(12.0, 10.0));
                                ui.add_sized([ui.available_width(), 152.0], jd_edit);
                                ui.add_space(16.0);

                                // Master resume — Auto (LLM picks) or force a specific one.
                                ui.label(RichText::new("Master Resume").size(13.0).color(INK_MUT));
                                ui.add_space(6.0);
                                let resumes: Vec<(u64, String)> = self.resume_store
                                    .items()
                                    .iter()
                                    .map(|r| (r.id, r.name.clone()))
                                    .collect();
                                let selected_label = match self.nj_resume_id {
                                    None => "Auto · best match".to_owned(),
                                    Some(id) => resumes
                                        .iter()
                                        .find(|(rid, _)| *rid == id)
                                        .map(|(_, n)| n.clone())
                                        .unwrap_or_else(|| "Auto · best match".to_owned()),
                                };
                                ui.spacing_mut().button_padding = egui::vec2(12.0, 10.0);
                                egui::ComboBox::from_id_salt("nj_resume_picker")
                                    .selected_text(selected_label)
                                    .width(ui.available_width())
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.nj_resume_id,
                                            None,
                                            "Auto · best match (recommended)",
                                        );
                                        for (id, name) in &resumes {
                                            ui.selectable_value(
                                                &mut self.nj_resume_id,
                                                Some(*id),
                                                name,
                                            );
                                        }
                                    });
                                ui.add_space(16.0);

                                // Notes
                                ui.label(RichText::new("Notes").size(13.0).color(INK_MUT));
                                ui.add_space(6.0);
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.nj_notes)
                                        .hint_text("Any initial notes...")
                                        .desired_rows(3)
                                        .desired_width(ui.available_width())
                                        .margin(egui::vec2(12.0, 10.0))
                                );
                                ui.add_space(8.0);
                            });
                    });
            });
    }
}

// ─── Settings modal ───────────────────────────────────────────────────────────

impl InterPrepApp {
    pub fn draw_settings_modal(&mut self, ctx: &egui::Context) {
        ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("st_bd")))
            .rect_filled(ctx.viewport_rect(), 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 160));

        let modal_w = 600.0f32;
        let modal_h = 580.0f32;
        let left_w  = 140.0f32;

        egui::Window::new("settings_modal_win")
            .title_bar(false)
            .fixed_size([modal_w, modal_h])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::new()
                    .fill(SURFACE)
                    .corner_radius(20.0)
                    .stroke(Stroke::new(0.5, BD))
                    .inner_margin(egui::vec2(0.0, 0.0))
            )
            .show(ctx, |ui| {
                ui.set_min_size(egui::vec2(modal_w, modal_h));
                ui.horizontal_top(|ui| {
                    // ── Left sidebar ──
                    egui::Frame::new()
                        .fill(BG)
                        .inner_margin(Margin { left: 16, right: 16, top: 16, bottom: 16 })
                        .show(ui, |ui| {
                            ui.set_min_width(left_w - 32.0);
                            ui.set_max_width(left_w - 32.0);
                            ui.set_min_height(modal_h - 32.0);
                            ui.vertical(|ui| {
                                // Title + close
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Settings").size(13.0).strong().color(INK));
                                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                        if ui.add(
                                            egui::Button::new(RichText::new(ph::X).size(13.0).color(INK_MUT))
                                                .fill(SURFACE).corner_radius(12.0)
                                                .min_size(egui::vec2(24.0, 24.0)).stroke(Stroke::NONE)
                                        ).clicked() { self.show_settings = false; }
                                    });
                                });
                                ui.add_space(16.0);

                                let tabs: [(&str, &str); 6] = [
                                    (ph::USER,       "Account"),
                                    (ph::SUN,        "Appearance"),
                                    (ph::UPLOAD,     "Resume"),
                                    (ph::BELL,       "Notifications"),
                                    (ph::SHIELD,     "Data & Privacy"),
                                    (ph::KEY,        "API Keys"),
                                ];

                                for (i, (icon, label)) in tabs.iter().enumerate() {
                                    let active = self.settings_tab == i;
                                    let rw = ui.available_width();
                                    let (r, resp) = ui.allocate_exact_size(egui::vec2(rw, 30.0), Sense::click());
                                    if ui.is_rect_visible(r) {
                                        if active {
                                            ui.painter().rect_filled(r, 15.0, SURFACE);
                                        } else if resp.hovered() {
                                            ui.painter().rect_filled(r, 15.0, SURFACE2);
                                        }
                                        ui.painter().text(
                                            egui::pos2(r.left() + 14.0, r.center().y),
                                            egui::Align2::CENTER_CENTER,
                                            icon,
                                            FontId::proportional(12.0),
                                            if active { INK } else { INK_MUT },
                                        );
                                        ui.painter().text(
                                            egui::pos2(r.left() + 28.0, r.center().y),
                                            egui::Align2::LEFT_CENTER,
                                            label,
                                            FontId::proportional(12.0),
                                            if active { INK } else { INK_MUT },
                                        );
                                    }
                                    if resp.clicked() { self.settings_tab = i; }
                                    ui.add_space(1.0);
                                }
                            });
                        });

                    // ── Right content ──
                    ui.vertical(|ui| {
                        ui.set_min_height(modal_h - 4.0);
                        ui.add_space(24.0);
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            let tab_name = [
                                "Account", "Appearance", "Resume",
                                "Notifications", "Data & Privacy", "API Keys",
                            ][self.settings_tab];
                            ui.label(RichText::new(tab_name).size(15.0).strong().color(INK));
                        });
                        ui.add_space(18.0);
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            ui.vertical(|ui| {
                                ui.set_max_width(modal_w - left_w - 60.0);
                                match self.settings_tab {
                                    // Account
                                    0 => {
                                        egui::Frame::new()
                                            .fill(BG)
                                            .corner_radius(15.0)
                                            .stroke(Stroke::new(0.5, BD))
                                            .inner_margin(Margin { left: 16, right: 16, top: 16, bottom: 16 })
                                            .show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    let (av, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), Sense::hover());
                                                    if ui.is_rect_visible(av) {
                                                        ui.painter().circle_filled(av.center(), 22.0, SURFACE2);
                                                        ui.painter().circle_stroke(av.center(), 22.0, Stroke::new(0.5, BD));
                                                        ui.painter().text(av.center(), egui::Align2::CENTER_CENTER,
                                                            "J", FontId::proportional(18.0), INK);
                                                    }
                                                    ui.add_space(14.0);
                                                    ui.vertical(|ui| {
                                                        ui.label(RichText::new("Jordan Chen").size(13.0).strong().color(INK));
                                                        ui.label(RichText::new("jordan@example.com").size(12.0).color(INK_MUT));
                                                    });
                                                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                                        ui.add(
                                                            egui::Button::new(RichText::new("Edit Profile").size(12.0).color(INK_MUT))
                                                                .fill(SURFACE2).corner_radius(20.0).stroke(Stroke::NONE)
                                                        );
                                                    });
                                                });
                                            });
                                        ui.add_space(12.0);
                                        egui::Frame::new()
                                            .fill(BG)
                                            .corner_radius(10.0)
                                            .stroke(Stroke::new(0.5, BD))
                                            .inner_margin(Margin { left: 14, right: 14, top: 14, bottom: 14 })
                                            .show(ui, |ui| {
                                                ui.label(
                                                    RichText::new("Manage your account details, subscription, and connected services.")
                                                        .size(12.0).color(INK_FAINT)
                                                );
                                            });
                                    }
                                    // Appearance
                                    1 => {
                                        egui::Frame::new()
                                            .fill(BG)
                                            .corner_radius(10.0)
                                            .stroke(Stroke::new(0.5, BD))
                                            .inner_margin(Margin { left: 16, right: 16, top: 16, bottom: 16 })
                                            .show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                                                    if ui.is_rect_visible(dot) {
                                                        ui.painter().circle_filled(dot.center(), 4.0, ACCENT);
                                                    }
                                                    ui.add_space(6.0);
                                                    ui.label(RichText::new("Dark mode only").size(13.0).strong().color(INK));
                                                });
                                                ui.add_space(8.0);
                                                ui.label(
                                                    RichText::new("InterPrep uses a dark canvas exclusively — optimized for focus during long prep sessions.")
                                                        .size(12.0).color(INK_MUT)
                                                );
                                            });
                                    }
                                    // Resume — master library used to tailor per-job docs
                                    2 => {
                                        // Card sits outside the scroll area so its rounded
                                        // corners are never clipped. Height is derived from
                                        // the modal dimensions directly because
                                        // `available_height()` is unreliable inside the
                                        // nested horizontal/vertical chain.
                                        let card_h = (modal_h - 96.0).max(200.0);
                                        egui::Frame::new()
                                            .fill(BG)
                                            .corner_radius(10.0)
                                            .stroke(Stroke::new(0.5, BD))
                                            .inner_margin(Margin { left: 16, right: 16, top: 16, bottom: 16 })
                                            .show(ui, |ui| {
                                                ui.set_min_height(card_h - 32.0);
                                                egui::ScrollArea::vertical()
                                                    .max_height(card_h - 32.0)
                                                    .auto_shrink([false, false])
                                                    .id_salt("resume_scroll")
                                                    .show(ui, |ui| {
                                                        ui.label(RichText::new("Master resumes").size(13.0).strong().color(INK));
                                                        ui.add_space(4.0);
                                                        ui.label(
                                                            RichText::new(
                                                                "Add one or more variants — InterPrep picks the closest \
                                                                 match for each job and tailors it to the JD before \
                                                                 starting company research."
                                                            ).size(12.0).color(INK_MUT)
                                                        );

                                                        ui.add_space(12.0);
                                                        ui.separator();
                                                        ui.add_space(10.0);

                                                        // ── List of resumes ──────────────────
                                                        let resumes: Vec<(u64, String, usize)> = self.resume_store
                                                            .items()
                                                            .iter()
                                                            .map(|r| (r.id, r.name.clone(), r.text.len()))
                                                            .collect();

                                                        if resumes.is_empty() {
                                                            ui.horizontal(|ui| {
                                                                ui.label(RichText::new(ph::WARNING).size(13.0).color(C_APPLIED));
                                                                ui.add_space(6.0);
                                                                ui.label(
                                                                    RichText::new("No resumes yet — add one below.")
                                                                        .size(12.0).color(INK_FAINT)
                                                                );
                                                            });
                                                        } else {
                                                            let mut to_remove: Option<u64> = None;
                                                            for (idx, (id, name, len)) in resumes.iter().enumerate() {
                                                                if idx > 0 {
                                                                    ui.add_space(4.0);
                                                                    ui.separator();
                                                                    ui.add_space(4.0);
                                                                }
                                                                ui.horizontal(|ui| {
                                                                    ui.label(RichText::new(ph::FILE_TEXT).size(13.0).color(MAGENTA));
                                                                    ui.add_space(8.0);
                                                                    ui.vertical(|ui| {
                                                                        ui.spacing_mut().item_spacing.y = 1.0;
                                                                        ui.label(RichText::new(name).size(13.0).strong().color(INK));
                                                                        ui.label(
                                                                            RichText::new(format!("{} characters", len))
                                                                                .size(11.0).color(INK_FAINT)
                                                                        );
                                                                    });
                                                                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                                                        if ui.add(
                                                                            egui::Button::new(
                                                                                RichText::new(format!("{} Remove", ph::TRASH))
                                                                                    .size(11.0).color(C_REJECTED)
                                                                            )
                                                                            .fill(SURFACE2)
                                                                            .corner_radius(14.0)
                                                                            .stroke(Stroke::NONE),
                                                                        ).clicked() {
                                                                            to_remove = Some(*id);
                                                                        }
                                                                    });
                                                                });
                                                            }
                                                            if let Some(id) = to_remove {
                                                                self.resume_store.remove(id);
                                                            }
                                                        }

                                                        ui.add_space(12.0);
                                                        ui.separator();
                                                        ui.add_space(10.0);

                                                        // ── Upload-resume button ─────────────
                                                        if ui.add(
                                                            egui::Button::new(
                                                                RichText::new(format!("{}  Upload a resume (.txt, .md, .docx, .pdf)", ph::UPLOAD))
                                                                    .size(12.5).color(INK)
                                                            )
                                                            .fill(SURFACE2)
                                                            .corner_radius(20.0)
                                                            .stroke(Stroke::new(1.0, BD))
                                                            .min_size(egui::vec2(ui.available_width(), 36.0)),
                                                        ).clicked() {
                                                            self.resume_form_error = None;
                                                            if let Some(path) = rfd::FileDialog::new()
                                                                .add_filter("Resume files", &["txt", "md", "docx", "pdf"])
                                                                .pick_file()
                                                            {
                                                                match crate::storage::read_resume_file(&path) {
                                                                    Some((name, text)) if text.len() >= 80 => {
                                                                        self.resume_store.add(name, text);
                                                                    }
                                                                    Some(_) => {
                                                                        self.resume_form_error = Some(
                                                                            "That file is too short to be a usable resume.".to_owned()
                                                                        );
                                                                    }
                                                                    None => {
                                                                        self.resume_form_error = Some(
                                                                            "Could not read that file. Try .txt, .md, or .docx.".to_owned()
                                                                        );
                                                                    }
                                                                }
                                                            }
                                                        }

                                                        if let Some(err) = &self.resume_form_error.clone() {
                                                            ui.add_space(6.0);
                                                            ui.label(RichText::new(err).size(11.5).color(C_REJECTED));
                                                        }

                                                        ui.add_space(8.0);
                                                        ui.label(
                                                            RichText::new("Stored at %APPDATA%/InterPrep/resumes.json — survives restart.")
                                                                .size(10.5).color(INK_FAINT)
                                                        );
                                                    });
                                            });
                                    }
                                    // API Keys
                                    5 => {
                                        let card_h = (modal_h - 96.0).max(200.0);
                                        egui::Frame::new()
                                            .fill(BG)
                                            .corner_radius(10.0)
                                            .stroke(Stroke::new(0.5, BD))
                                            .inner_margin(Margin { left: 16, right: 16, top: 16, bottom: 16 })
                                            .show(ui, |ui| {
                                                ui.set_min_height(card_h - 32.0);
                                                egui::ScrollArea::vertical()
                                                    .max_height(card_h - 32.0)
                                                    .auto_shrink([false, false])
                                                    .id_salt("api_keys_scroll")
                                                    .show(ui, |ui| {
                                                        // ── Gemini ──────────────────────
                                                        ui.label(RichText::new("Gemini API Key").size(13.0).strong().color(INK));
                                                        ui.add_space(6.0);
                                                        key_field(ui, &mut self.gemini_api_key, &mut self.gemini_key_visible, "AIza...");
                                                        ui.add_space(4.0);
                                                        ui.label(
                                                            RichText::new("Powers AI chat · get yours at aistudio.google.com · cleared on restart")
                                                                .size(11.0).color(INK_FAINT)
                                                        );

                                                        ui.add_space(12.0);
                                                        ui.separator();
                                                        ui.add_space(10.0);

                                                        // ── Glassdoor ───────────────────
                                                        ui.horizontal(|ui| {
                                                            ui.label(RichText::new("Glassdoor").size(13.0).strong().color(INK));
                                                            ui.add_space(6.0);
                                                            ui.label(RichText::new("social login").size(11.0).color(INK_FAINT));
                                                        });
                                                        ui.add_space(4.0);
                                                        ui.label(
                                                            RichText::new(
                                                                "Glassdoor uses Google / Facebook sign-in — \
                                                                 headless credential login isn't supported. \
                                                                 Public salary data and reviews are scraped without login."
                                                            ).size(12.0).color(INK_MUT)
                                                        );

                                                        ui.add_space(12.0);
                                                        ui.separator();
                                                        ui.add_space(10.0);

                                                        // ── Indeed ──────────────────────
                                                        ui.horizontal(|ui| {
                                                            ui.label(RichText::new("Indeed").size(13.0).strong().color(INK));
                                                            ui.add_space(6.0);
                                                            ui.label(RichText::new("social login").size(11.0).color(INK_FAINT));
                                                        });
                                                        ui.add_space(4.0);
                                                        ui.label(
                                                            RichText::new(
                                                                "Indeed uses Google / Facebook sign-in — \
                                                                 headless credential login isn't supported. \
                                                                 Public job data and company reviews are scraped without login."
                                                            ).size(12.0).color(INK_MUT)
                                                        );

                                                    });
                                            });
                                    }
                                    // Notifications / Data & Privacy
                                    _ => {
                                        ui.label(RichText::new("Settings for this section coming soon.").size(12.0).color(INK_FAINT));
                                    }
                                }
                            });
                        });
                    });
                });
            });
    }
}

// ─── Report viewer modal ──────────────────────────────────────────────────────

impl InterPrepApp {
    pub fn draw_report_modal(&mut self, ctx: &egui::Context) {
        ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("rpt_bd")))
            .rect_filled(ctx.viewport_rect(), 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 190));

        let vp = ctx.viewport_rect();
        let modal_w = (vp.width()  - 80.0).min(820.0).max(480.0);
        let modal_h = (vp.height() - 60.0).min(720.0).max(400.0);

        // Clone before the closure so we can still mutate self (for close button) inside.
        let report_text = self.report_modal_text.clone();

        egui::Window::new("report_modal_win")
            .title_bar(false)
            .fixed_size([modal_w, modal_h])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .corner_radius(18.0)
                    .stroke(Stroke::new(0.5, BD))
                    .inner_margin(egui::vec2(0.0, 0.0))
            )
            .show(ctx, |ui| {
                ui.set_min_size(egui::vec2(modal_w, modal_h));

                // ── Header — plain Frame, no nested Panel ─────────────────────
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(0.5, BD))
                    .inner_margin(Margin { left: 24, right: 16, top: 14, bottom: 14 })
                    .show(ui, |ui| {
                        ui.set_min_width(modal_w - 40.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(ph::ARTICLE).size(16.0).color(MAGENTA));
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("Company Research Report")
                                    .size(14.5).strong().color(INK)
                            );
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(
                                    egui::Button::new(
                                        RichText::new(ph::X).size(12.0).color(INK_MUT)
                                    )
                                    .fill(SURFACE2)
                                    .corner_radius(12.0)
                                    .min_size(egui::vec2(28.0, 28.0))
                                    .stroke(Stroke::NONE)
                                ).clicked() {
                                    self.show_report_modal = false;
                                }
                            });
                        });
                    });

                // ── Scrollable content — ScrollArea directly after the header ─
                // No CentralPanel: that ID is global and conflicts with app.rs.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt("rpt_content_scroll")
                    .show(ui, |ui| {
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            ui.add_space(28.0);
                            ui.vertical(|ui| {
                                ui.set_max_width(modal_w - 56.0);
                                render_markdown(ui, &report_text);
                                ui.add_space(28.0);
                            });
                        });
                    });
            });
    }
}
