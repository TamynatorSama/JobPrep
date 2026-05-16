use eframe::egui::{self, StrokeKind, Color32, FontId, Sense, RichText, Stroke, Align};
use crate::gui::app::InterPrepApp;
use crate::gui::types::*;
use crate::gui::theme::*;
use egui_phosphor::regular as ph;

/// Pending mutation collected from the row kebab popup or the archived-row
/// restore button. The job list is borrowed immutably during rendering so the
/// action is applied after iteration ends.
enum JobAction {
    Archive(usize),
    Delete(usize),
    Unarchive(usize),
}

/// Renders a single row in the kebab popup. Returns the Response so callers
/// can detect clicks.
fn menu_item(ui: &mut egui::Ui, icon: &str, label: &str, fg: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(format!("{icon}  {label}")).size(12.5).color(fg)
        )
        .fill(Color32::TRANSPARENT)
        .corner_radius(6.0)
        .stroke(Stroke::NONE)
        .min_size(egui::vec2(ui.available_width(), 28.0)),
    )
}

// ─── Sidebar ─────────────────────────────────────────────────────────────────

impl InterPrepApp {
    pub fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        let w    = ui.available_width();
        let full = w > 100.0;

        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

        // Logo — black tile with white sparkle
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.add_space(if full { 16.0 } else { 12.0 });
            let (logo_rect, _) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), Sense::hover());
            if ui.is_rect_visible(logo_rect) {
                ui.painter().rect_filled(logo_rect, 8.0, INK);
                ui.painter().text(
                    logo_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    ph::SPARKLE,
                    FontId::proportional(14.0),
                    BG,
                );
            }
            if full {
                ui.add_space(10.0);
                ui.label(RichText::new("InterPrep").size(15.0).strong().color(INK));
            }
        });
        ui.add_space(20.0);

        // New Job button — white pill (design: bg #fff, color #0C0C0C, radius 100)
        ui.horizontal(|ui| {
            ui.add_space(if full { 8.0 } else { 8.0 });
            let btn_w = if full { w - 16.0 } else { 48.0 };
            let btn_label = if full {
                format!("{}  New Job", ph::PLUS)
            } else {
                ph::PLUS.to_string()
            };
            if ui.add(
                egui::Button::new(RichText::new(&btn_label).size(13.0).color(BG).strong())
                    .fill(INK)
                    .corner_radius(100.0)
                    .stroke(Stroke::NONE)
                    .min_size(egui::vec2(btn_w, 36.0)),
            ).clicked() {
                self.show_new_job = true;
            }
        });
        ui.add_space(8.0);

        // Nav — pill rows (radius 100), active = SURFACE fill + INK text
        let nav_items = [
            (ph::CALENDAR, "Timeline", Screen::Timeline),
            (ph::CHAT,     "Research", Screen::Chat),
        ];
        for (icon, label, target) in nav_items {
            let active = self.screen == target;
            ui.horizontal(|ui| {
                ui.add_space(if full { 8.0 } else { 4.0 });
                let btn = egui::Button::new(if full {
                    RichText::new(format!("{icon}   {label}"))
                        .size(13.0)
                        .color(if active { INK } else { INK_MUT })
                } else {
                    RichText::new(icon)
                        .size(16.0)
                        .color(if active { INK } else { INK_MUT })
                })
                .fill(if active { SURFACE } else { Color32::TRANSPARENT })
                .corner_radius(100.0)
                .stroke(Stroke::NONE)
                .min_size(egui::vec2(if full { w - 16.0 } else { 48.0 }, 34.0));
                if ui.add(btn).clicked() {
                    self.screen = target;
                }
            });
            ui.add_space(1.0);
        }

        // Job Research list (only in Research / full sidebar)
        if full && self.screen == Screen::Chat {
            ui.add_space(12.0);
            // Hairline divider above section header
            let (sep_r, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), Sense::hover());
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(sep_r.min.x, sep_r.min.y),
                    egui::vec2(w, 1.0),
                ),
                0.0, BD,
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(
                    RichText::new("JOB RESEARCH")
                        .size(10.5)
                        .color(INK_FAINT)
                        .strong()
                );
            });
            ui.add_space(4.0);

            let mut sel_job  = self.selected_job;
            let mut sel_chat = self.selected_chat;
            let mut toggle_expand: Option<usize> = None;
            // Pending mutation collected inside the loop and applied after
            // the scroll area ends, since the iteration holds &self.jobs.
            let mut pending_action: Option<JobAction> = None;
            // Hoist `show_archived` to a local so the ScrollArea closure can
            // toggle it without trying to mutably borrow `self` while still
            // immutably iterating `self.jobs`.
            let mut show_archived = self.show_archived;
            let archived_count = self.jobs.iter().filter(|j| j.archived).count();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(ui.available_height() - 50.0)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 1.0);
                    for job in self.jobs.iter().filter(|j| !j.archived) {
                        let jsel = sel_job == Some(job.id);
                        let row_w = ui.available_width();

                        // ── Job row ────────────────────────────────────
                        let (row_r, row_resp) =
                            ui.allocate_exact_size(egui::vec2(row_w, 36.0), Sense::click());

                        // Kebab interact — separate sub-rect so its clicks
                        // don't trigger row selection. Allocated *before* we
                        // check row_resp.clicked() so the kebab's interact
                        // takes click priority on overlap.
                        //
                        // The interact and the popup MUST use distinct ids:
                        // egui panics on id collision when the same id is
                        // registered as both an interactable widget and a
                        // popup `Area`.
                        let kebab_id = ui.id().with(("job_kebab", job.id));
                        let popup_id = ui.id().with(("job_kebab_popup", job.id));
                        let kebab_rect = egui::Rect::from_center_size(
                            egui::pos2(row_r.right() - 16.0, row_r.center().y),
                            egui::vec2(24.0, 24.0),
                        );
                        let kebab_resp = ui.interact(kebab_rect, kebab_id, Sense::click());
                        let popup_open = ui.memory(|m| m.is_popup_open(popup_id));
                        let show_kebab = row_resp.hovered() || kebab_resp.hovered() || popup_open;

                        if ui.is_rect_visible(row_r) {
                            if jsel && !job.sidebar_expanded || row_resp.hovered() {
                                ui.painter().rect_filled(row_r, 6.0, SURFACE2);
                            }
                            // Caret
                            let caret = if job.sidebar_expanded { ph::CARET_DOWN } else { ph::CARET_RIGHT };
                            ui.painter().text(
                                egui::pos2(row_r.left() + 16.0, row_r.center().y),
                                egui::Align2::CENTER_CENTER,
                                caret,
                                FontId::proportional(11.0),
                                INK_FAINT,
                            );
                            // Small avatar
                            let av_sz = 18.0;
                            let av_r = egui::Rect::from_min_size(
                                egui::pos2(row_r.left() + 28.0, row_r.center().y - av_sz * 0.5),
                                egui::vec2(av_sz, av_sz),
                            );
                            ui.painter().rect_filled(av_r, 5.0, job.avatar_color.linear_multiply(0.12));
                            ui.painter().rect_stroke(
                                av_r, 5.0,
                                Stroke::new(0.5, job.avatar_color.linear_multiply(0.25)),
                                StrokeKind::Inside,
                            );
                            ui.painter().text(
                                av_r.center(), egui::Align2::CENTER_CENTER,
                                &job.company.chars().next().unwrap_or('?').to_string(),
                                FontId::proportional(10.0), job.avatar_color,
                            );
                            // Company name — clip ends before the right-edge widget.
                            ui.painter().text(
                                egui::pos2(row_r.left() + 52.0, row_r.center().y),
                                egui::Align2::LEFT_CENTER,
                                &job.company,
                                FontId::proportional(12.5),
                                INK,
                            );
                            // Right-edge widget: status pill OR kebab on hover.
                            if show_kebab {
                                ui.painter().rect_filled(
                                    kebab_rect, 6.0,
                                    if kebab_resp.hovered() || popup_open { SURFACE3 } else { SURFACE },
                                );
                                ui.painter().text(
                                    kebab_rect.center(), egui::Align2::CENTER_CENTER,
                                    ph::DOTS_THREE_VERTICAL,
                                    FontId::proportional(12.0),
                                    INK_MUT,
                                );
                            } else {
                                let bl = job.status.label();
                                let bc = job.status.color();
                                let bw = bl.len() as f32 * 6.0 + 14.0;
                                let br = egui::Rect::from_min_size(
                                    egui::pos2(row_r.right() - bw - 8.0, row_r.center().y - 9.0),
                                    egui::vec2(bw, 18.0),
                                );
                                ui.painter().rect_filled(br, 9.0, bc.linear_multiply(0.12));
                                ui.painter().text(
                                    br.center(), egui::Align2::CENTER_CENTER,
                                    bl, FontId::proportional(10.0), bc,
                                );
                            }
                        }

                        // Kebab popup: Archive / Delete actions.
                        if kebab_resp.clicked() {
                            ui.memory_mut(|m| m.toggle_popup(popup_id));
                        }
                        egui::popup_below_widget(
                            ui, popup_id, &kebab_resp,
                            egui::PopupCloseBehavior::CloseOnClickOutside,
                            |ui| {
                                ui.set_min_width(150.0);
                                ui.spacing_mut().item_spacing = egui::vec2(0.0, 2.0);
                                if menu_item(ui, ph::ARCHIVE, "Archive", INK_MUT).clicked() {
                                    pending_action = Some(JobAction::Archive(job.id));
                                    ui.memory_mut(|m| m.close_popup(popup_id));
                                }
                                if menu_item(ui, ph::TRASH, "Delete", C_REJECTED).clicked() {
                                    pending_action = Some(JobAction::Delete(job.id));
                                    ui.memory_mut(|m| m.close_popup(popup_id));
                                }
                            },
                        );

                        // Suppress row click when the click lands on the kebab
                        // or while the popup is open (clicks dismiss the popup).
                        if !kebab_resp.hovered() && !popup_open && row_resp.clicked() {
                            if jsel {
                                toggle_expand = Some(job.id);
                            } else {
                                sel_job = Some(job.id);
                                if let Some(first) = job.chats.first() {
                                    sel_chat = Some(first.id);
                                }
                                toggle_expand = Some(job.id);
                            }
                        }

                        // ── Chat threads (if expanded) ─────────────────
                        if job.sidebar_expanded {
                            for thread in &job.chats {
                                let csel = sel_chat == Some(thread.id) && jsel;
                                let (tr, tresp) =
                                    ui.allocate_exact_size(egui::vec2(row_w, 30.0), Sense::click());
                                if ui.is_rect_visible(tr) {
                                    if csel || tresp.hovered() {
                                        ui.painter().rect_filled(tr, 6.0, SURFACE2);
                                    }
                                    ui.painter().text(
                                        egui::pos2(tr.left() + 52.0, tr.center().y),
                                        egui::Align2::CENTER_CENTER,
                                        ph::CHAT,
                                        FontId::proportional(11.0),
                                        if csel { INK } else { INK_FAINT },
                                    );
                                    let title_short: String = thread.title.chars().take(26).collect();
                                    ui.painter().text(
                                        egui::pos2(tr.left() + 66.0, tr.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        &title_short,
                                        FontId::proportional(12.0),
                                        if csel { INK } else { INK_MUT },
                                    );
                                }
                                if tresp.clicked() {
                                    sel_job  = Some(job.id);
                                    sel_chat = Some(thread.id);
                                }
                            }

                            // "+ New chat" row
                            let (nr, nresp) =
                                ui.allocate_exact_size(egui::vec2(row_w, 26.0), Sense::click());
                            if ui.is_rect_visible(nr) {
                                if nresp.hovered() {
                                    ui.painter().rect_filled(nr, 6.0, SURFACE2);
                                }
                                ui.painter().text(
                                    egui::pos2(nr.left() + 52.0, nr.center().y),
                                    egui::Align2::CENTER_CENTER,
                                    ph::PLUS,
                                    FontId::proportional(11.0),
                                    INK_FAINT,
                                );
                                ui.painter().text(
                                    egui::pos2(nr.left() + 66.0, nr.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    "New chat",
                                    FontId::proportional(11.5),
                                    INK_FAINT,
                                );
                            }
                            if nresp.clicked() {
                                sel_job = Some(job.id);
                                sel_chat = None; // deselect → shows empty chat screen
                            }
                        }
                    }

                    // ── Archived (N) disclosure (inside scroll area) ─────────
                    if archived_count > 0 {
                        ui.add_space(8.0);
                        let row_w = ui.available_width();
                        let (hdr_r, hdr_resp) =
                            ui.allocate_exact_size(egui::vec2(row_w, 28.0), Sense::click());
                        if ui.is_rect_visible(hdr_r) {
                            if hdr_resp.hovered() {
                                ui.painter().rect_filled(hdr_r, 6.0, SURFACE2);
                            }
                            let caret = if show_archived { ph::CARET_DOWN } else { ph::CARET_RIGHT };
                            ui.painter().text(
                                egui::pos2(hdr_r.left() + 16.0, hdr_r.center().y),
                                egui::Align2::CENTER_CENTER,
                                caret,
                                FontId::proportional(11.0),
                                INK_FAINT,
                            );
                            ui.painter().text(
                                egui::pos2(hdr_r.left() + 30.0, hdr_r.center().y),
                                egui::Align2::LEFT_CENTER,
                                format!("Archived ({archived_count})"),
                                FontId::proportional(11.5),
                                INK_FAINT,
                            );
                        }
                        if hdr_resp.clicked() {
                            show_archived = !show_archived;
                        }

                        if show_archived {
                            for job in self.jobs.iter().filter(|j| j.archived) {
                                let (ar, _ar_resp) = ui.allocate_exact_size(
                                    egui::vec2(row_w, 30.0), Sense::hover(),
                                );
                                if ui.is_rect_visible(ar) {
                                    let av_sz = 16.0;
                                    let av_r = egui::Rect::from_min_size(
                                        egui::pos2(ar.left() + 28.0, ar.center().y - av_sz * 0.5),
                                        egui::vec2(av_sz, av_sz),
                                    );
                                    ui.painter().rect_filled(av_r, 4.0, job.avatar_color.linear_multiply(0.10));
                                    ui.painter().text(
                                        av_r.center(), egui::Align2::CENTER_CENTER,
                                        &job.company.chars().next().unwrap_or('?').to_string(),
                                        FontId::proportional(9.0), job.avatar_color.linear_multiply(0.7),
                                    );
                                    ui.painter().text(
                                        egui::pos2(ar.left() + 50.0, ar.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        &job.company,
                                        FontId::proportional(12.0),
                                        INK_FAINT,
                                    );
                                }
                                let restore_rect = egui::Rect::from_center_size(
                                    egui::pos2(ar.right() - 16.0, ar.center().y),
                                    egui::vec2(24.0, 24.0),
                                );
                                let restore_id = ui.id().with(("restore", job.id));
                                let restore_resp = ui.interact(restore_rect, restore_id, Sense::click());
                                if ui.is_rect_visible(restore_rect) {
                                    ui.painter().rect_filled(
                                        restore_rect, 6.0,
                                        if restore_resp.hovered() { SURFACE2 } else { Color32::TRANSPARENT },
                                    );
                                    ui.painter().text(
                                        restore_rect.center(), egui::Align2::CENTER_CENTER,
                                        ph::ARROW_COUNTER_CLOCKWISE,
                                        FontId::proportional(11.0),
                                        if restore_resp.hovered() { INK } else { INK_MUT },
                                    );
                                }
                                if restore_resp.clicked() {
                                    pending_action = Some(JobAction::Unarchive(job.id));
                                }
                            }
                        }
                    }
                });

            self.selected_job   = sel_job;
            self.selected_chat  = sel_chat;
            self.show_archived  = show_archived;
            if let Some(jid) = toggle_expand {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == jid) {
                    job.sidebar_expanded = !job.sidebar_expanded;
                }
            }

            // Apply any kebab/restore action collected inside the loop.
            if let Some(act) = pending_action {
                match act {
                    JobAction::Archive(id)   => self.archive_job(id),
                    JobAction::Delete(id)    => self.delete_job(id),
                    JobAction::Unarchive(id) => self.unarchive_job(id),
                }
            }
        }

        // Settings (bottom)
        ui.with_layout(egui::Layout::bottom_up(Align::LEFT), |ui| {
            ui.add_space(8.0);
            // Settings row — design: full-pill row, gear + label, hover bg surface
            let row_h = 34.0;
            let (row_r, row_resp) = ui.allocate_exact_size(egui::vec2(w, row_h), Sense::click());
            if ui.is_rect_visible(row_r) {
                if row_resp.hovered() {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(row_r.min.x + 8.0, row_r.min.y),
                            egui::vec2(w - 16.0, row_h),
                        ),
                        100.0, SURFACE,
                    );
                }
                ui.painter().text(
                    egui::pos2(row_r.min.x + 22.0, row_r.center().y),
                    egui::Align2::CENTER_CENTER,
                    ph::GEAR,
                    FontId::proportional(15.0), INK_MUT,
                );
                if full {
                    ui.painter().text(
                        egui::pos2(row_r.min.x + 40.0, row_r.center().y),
                        egui::Align2::LEFT_CENTER,
                        "Settings",
                        FontId::proportional(13.0), INK_MUT,
                    );
                }
            }
            if row_resp.clicked() { self.show_settings = true; }
            // Separator line above
            ui.add_space(4.0);
            let (sep_r, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), Sense::hover());
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(sep_r.min.x + 8.0, sep_r.min.y),
                    egui::vec2(w - 16.0, 0.5),
                ),
                0.0, BD,
            );
        });
    }
}

