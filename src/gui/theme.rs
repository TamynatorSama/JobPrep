use eframe::egui::{self, Color32, Stroke};

// ─── Design tokens (from InterPrep Framer spec — dark-only canvas) ───────────

pub const BG:        Color32 = Color32::from_rgb(0x0C, 0x0C, 0x0C); // canvas
pub const SURFACE:   Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1A); // surface-1
pub const SURFACE2:  Color32 = Color32::from_rgb(0x24, 0x24, 0x24); // surface-2 (featured)
pub const SURFACE3:  Color32 = Color32::from_rgb(0x2E, 0x2E, 0x2E); // hover/active
pub const INK:       Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
pub const INK_MUT:   Color32 = Color32::from_rgb(0x99, 0x99, 0x99); // ink-muted
pub const INK_FAINT: Color32 = Color32::from_rgb(0x66, 0x66, 0x66); // ink-tertiary
pub const ACCENT:    Color32 = Color32::from_rgb(0x00, 0x99, 0xFF); // accent-blue
pub const BD:        Color32 = Color32::from_rgb(0x1F, 0x1F, 0x1F); // hairline

// Status colors — dark-canvas appropriate (colored ink on tinted chips)
pub const C_APPLIED:   Color32 = Color32::from_rgb(0xF5, 0x9E, 0x0B); // amber-500
pub const C_SCREENING: Color32 = ACCENT;                              // accent-blue
pub const C_TECHNICAL: Color32 = Color32::from_rgb(0xA8, 0x55, 0xF7); // purple-500
pub const C_OFFER:     Color32 = Color32::from_rgb(0x22, 0xC5, 0x5E); // green-500
pub const C_REJECTED:  Color32 = Color32::from_rgb(0xEF, 0x44, 0x44); // red-500
pub const C_INDIGO:    Color32 = Color32::from_rgb(0x63, 0x5B, 0xFF); // sample avatars
pub const MAGENTA:     Color32 = Color32::from_rgb(0xEC, 0x48, 0x99); // pink-500 (gradient spotlight)

// Gantt stage track colors — match the design's STAGE_COLORS array exactly
pub const STAGE_COLS: [Color32; 6] = [
    C_APPLIED,                                        // 0 Applied   — amber
    ACCENT,                                           // 1 Screen    — accent blue
    C_TECHNICAL,                                      // 2 Tech 1    — purple
    C_TECHNICAL,                                      // 3 Tech 2    — purple
    MAGENTA,                                          // 4 Final     — pink
    C_OFFER,                                          // 5 Offer     — green
];
pub const STAGES: [&str; 6] = ["Applied", "Screen", "Technical 1", "Technical 2", "Final", "Offer"];


// ─── Theme ────────────────────────────────────────────────────────────────────

pub fn apply_theme(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill                        = BG;
    v.window_fill                       = SURFACE;
    v.extreme_bg_color                  = BG;
    v.faint_bg_color                    = SURFACE2;
    v.selection.bg_fill                 = ACCENT.linear_multiply(0.3);
    v.selection.stroke                  = Stroke::new(1.0, ACCENT);
    v.widgets.inactive.bg_fill          = SURFACE;
    v.widgets.inactive.bg_stroke        = Stroke::new(0.5, BD);
    v.widgets.hovered.bg_fill           = SURFACE2;
    v.widgets.hovered.bg_stroke         = Stroke::new(0.5, BD);
    v.widgets.active.bg_fill            = SURFACE3;
    v.widgets.active.bg_stroke          = Stroke::new(0.5, BD);
    v.widgets.noninteractive.bg_fill    = SURFACE;
    v.widgets.noninteractive.bg_stroke  = Stroke::new(0.5, BD);
    v.hyperlink_color                   = ACCENT;
    v.override_text_color               = Some(INK_MUT);
    ctx.set_visuals(v);

    let mut s = (*ctx.global_style()).clone();
    s.spacing.item_spacing   = egui::vec2(8.0, 6.0);
    s.spacing.button_padding = egui::vec2(12.0, 7.0);
    ctx.set_global_style(s);
}
