use eframe::egui::{self, RichText, Color32, Sense};
use crate::gui::types::*;
use crate::gui::theme::*;

// ─── Markdown renderer ───────────────────────────────────────────────────────

pub fn render_markdown(ui: &mut egui::Ui, text: &str) {
    let mut in_code_block = false;
    // Streamed model output sometimes contains runs of blank lines that
    // otherwise compound into big visual gaps. Collapse any run of empty
    // lines to a single small spacer.
    let mut last_was_blank = false;

    for line in text.lines() {
        // Code fence toggle
        if line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            if !in_code_block { ui.add_space(2.0); }
            continue;
        }
        if in_code_block {
            ui.label(
                RichText::new(line)
                    .size(12.5)
                    .color(INK_MUT)
                    .family(egui::FontFamily::Monospace),
            );
            continue;
        }

        // H1
        if let Some(h) = line.strip_prefix("# ") {
            ui.add_space(10.0);
            ui.label(RichText::new(h).size(19.0).strong().color(INK));
            md_rule(ui);
            ui.add_space(4.0);
        // H2
        } else if let Some(h) = line.strip_prefix("## ") {
            ui.add_space(8.0);
            ui.label(RichText::new(h).size(16.5).strong().color(INK));
            md_rule(ui);
            ui.add_space(3.0);
        // H3
        } else if let Some(h) = line.strip_prefix("### ") {
            ui.add_space(7.0);
            ui.label(RichText::new(h).size(14.5).strong().color(INK));
            ui.add_space(1.0);
        // H4
        } else if let Some(h) = line.strip_prefix("#### ") {
            ui.add_space(4.0);
            ui.label(RichText::new(h).size(13.0).strong().color(INK_MUT));
        // Horizontal rule
        } else if is_hr(line) {
            ui.add_space(6.0);
            md_rule(ui);
            ui.add_space(6.0);
        // Dash / asterisk bullets
        } else if let Some(content) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            md_bullet(ui, content);
        // Unicode bullet (used in existing seed messages)
        } else if let Some(content) = line.strip_prefix("• ") {
            md_bullet(ui, content);
        // Numbered list
        } else if is_numbered_list(line) {
            if let Some(pos) = line.find(". ") {
                let num     = &line[..pos];
                let content = &line[pos + 2..];
                let total_w = ui.available_width();
                let indent  = 12.0_f32;
                // Marker width grows with digit count ("1." vs "12.").
                let marker_w  = 6.0 + (num.len() as f32 * 8.5);
                let gap       = 6.0_f32;
                let content_w = (total_w - indent - marker_w - gap).max(80.0);

                ui.horizontal_top(|ui| {
                    ui.add_space(indent);
                    ui.label(
                        RichText::new(format!("{num}."))
                            .size(13.5)
                            .strong()
                            .color(MAGENTA),
                    );
                    ui.add_space(gap);
                    ui.allocate_ui_with_layout(
                        egui::vec2(content_w, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| { render_rich_line(ui, content, 13.5); },
                    );
                });
            }
        // Empty line — collapse consecutive blanks into a single 5px gap.
        } else if line.trim().is_empty() {
            if !last_was_blank {
                ui.add_space(5.0);
            }
            last_was_blank = true;
            continue;
        // Regular text (may contain inline bold)
        } else {
            render_rich_line(ui, line, 14.0);
        }
        last_was_blank = false;
    }
}

fn is_hr(line: &str) -> bool {
    let t = line.trim();
    t == "---" || t == "***" || t == "___"
        || (t.len() >= 3 && t.chars().all(|c| c == '-'))
}

fn is_numbered_list(line: &str) -> bool {
    line.find(". ").map_or(false, |pos| {
        !line[..pos].is_empty() && line[..pos].chars().all(|c| c.is_ascii_digit())
    })
}

fn md_rule(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 1.0),
        Sense::hover(),
    );
    ui.painter().rect_filled(r, 0.0, BD);
}

fn md_bullet(ui: &mut egui::Ui, content: &str) {
    // Explicit width math so the content ui has a real width constraint to
    // wrap against. `ui.vertical` inside `ui.horizontal_top` was allocating
    // unbounded extra height because egui couldn't infer the correct width.
    let total_w   = ui.available_width();
    let indent    = 12.0_f32;
    let marker_w  = 14.0_f32;
    let gap       = 6.0_f32;
    let content_w = (total_w - indent - marker_w - gap).max(80.0);

    ui.horizontal_top(|ui| {
        ui.add_space(indent);
        ui.label(RichText::new("•").size(13.5).color(MAGENTA));
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            egui::vec2(content_w, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| { render_rich_line(ui, content, 13.5); },
        );
    });
}

/// Renders a line with inline `**bold**` markers as a SINGLE wrapping label
/// built from a `LayoutJob`. One label = one element to lay out, so there's
/// no row-allocation surprise from `with_main_wrap`.
fn render_rich_line(ui: &mut egui::Ui, line: &str, size: f32) {
    use egui::text::{LayoutJob, TextFormat};

    if !line.contains("**") {
        ui.add(egui::Label::new(RichText::new(line).size(size).color(INK)).wrap());
        return;
    }

    let mut job = LayoutJob::default();
    let normal = TextFormat {
        font_id: egui::FontId::proportional(size),
        color:   INK,
        ..Default::default()
    };
    // egui's "strong" style is brighter rather than thicker; use INK directly
    // and rely on visual contrast against INK_MUT body text.
    let bold = TextFormat {
        font_id: egui::FontId::proportional(size),
        color:   INK,
        ..Default::default()
    };

    for (i, part) in line.split("**").enumerate() {
        if part.is_empty() {
            continue;
        }
        let fmt = if i % 2 == 1 { bold.clone() } else { normal.clone() };
        job.append(part, 0.0, fmt);
    }
    ui.add(egui::Label::new(job).wrap());
}

// ─── Date / day-of-week helpers ──────────────────────────────────────────────
// Today is May 3, 2026 (Sunday).

pub fn day_to_month_day(offset: i32) -> (u8, u8, &'static str) {
    let may_day = 3 + offset;
    if may_day >= 1 && may_day <= 31      { (5, may_day as u8, "May") }
    else if may_day < 1 && may_day > -30  { let d = (30 + may_day) as u8; (4, d, "Apr") }
    else if may_day <= -30                { let d = (31 + (may_day + 30)) as u8; (3, d, "Mar") }
    else if may_day <= 31 + 30            { (6, (may_day - 31) as u8, "Jun") }
    else if may_day <= 31 + 30 + 31       { (7, (may_day - 61) as u8, "Jul") }
    else                                  { (8, (may_day - 92) as u8, "Aug") }
}

pub fn day_of_week_letter(day_offset: i32) -> &'static str {
    // May 3, 2026 is a Sunday → offset 0 maps to index 0 ("S").
    let dow = ((day_offset % 7) + 7) % 7;
    ["S", "M", "T", "W", "T", "F", "S"][dow as usize]
}

// ─── Sample data ─────────────────────────────────────────────────────────────

pub fn sample_jobs() -> Vec<Job> {
    vec![
        Job {
            id: 1,
            company: "Stripe".to_owned(),
            role: "Software Engineer".to_owned(),
            location: "Remote US".to_owned(),

            job_description: "Senior Software Engineer role focused on payments infrastructure, distributed systems, APIs, and reliability.".to_owned(),
            research_status: "Sample".to_owned(),
            status: JobStatus::Technical,
            current_stage: 2,
            stage_notes: vec![
                StageNote::new(-75, "Submitted",   "Applied via Stripe careers portal."),
                StageNote::new(-13, "Passed",      "30-min call with recruiter Sarah."),
                StageNote::new(-2,  "In progress", "Technical screen scheduled — systems design focus."),
            ],
            chats: vec![
                ChatThread {
                    id: 1,
                    title: "System Design Prep".to_owned(),

                    messages: vec![
                        ChatMsg::ai("Ready to help you prepare for the Stripe Systems Design interview.\n\n• Review distributed systems fundamentals\n• Study Stripe's payment architecture\n• Practice designing a rate limiter or payment ledger\n\nWhat aspect would you like to focus on first?"),
                        ChatMsg::user("Can you explain how Stripe handles idempotency keys?"),
                        ChatMsg::ai("Great question. Stripe uses idempotency keys to safely retry requests.\n\n**How it works:**\n\n• Client generates a unique key per logical operation\n• Server stores the key + result for ~24 hours\n• Duplicate requests return the cached response\n\nThis prevents double-charges when network errors cause retries."),
                    ],
                },
                ChatThread {
                    id: 2,
                    title: "Behavioral Questions".to_owned(),

                    messages: vec![
                        ChatMsg::ai("Let's work on your behavioural responses using the STAR format.\n\nTell me about a time you:\n• Led a technical project under pressure\n• Disagreed with a team decision\n• Improved a critical system's reliability\n\nWhich would you like to prepare first?"),
                    ],
                },
            ],
            avatar_color: C_TECHNICAL,
            sidebar_expanded: true,
            archived: false,
            tailored_resume:  None,
            resume_docx_path: None,
            scorecard:        None,
        },
        Job {
            id: 2,
            company: "Anthropic".to_owned(),
            role: "ML Engineer".to_owned(),
            location: "San Francisco, CA".to_owned(),

            job_description: "ML Engineer role focused on model evaluation, safety, interpretability, and production machine learning systems.".to_owned(),
            research_status: "Sample".to_owned(),
            status: JobStatus::Applied,
            current_stage: 0,
            stage_notes: vec![
                StageNote::new(-2, "Submitted", "Applied through referral from Sarah."),
            ],
            chats: vec![
                ChatThread {
                    id: 3,
                    title: "Job Description Analysis".to_owned(),

                    messages: vec![
                        ChatMsg::ai("I've analysed the Anthropic ML Engineer, Alignment role.\n\n**Key requirements:**\n\n• Strong background in ML research (PyTorch, JAX)\n• Experience with RLHF or constitutional AI\n• Published research or open-source contributions\n\n**Your angle:**\n\nHighlight any work on model evaluation, safety testing, or interpretability."),
                    ],
                },
            ],
            avatar_color: Color32::from_rgb(217, 119, 87),
            sidebar_expanded: false,
            archived: false,
            tailored_resume:  None,
            resume_docx_path: None,
            scorecard:        None,
        },
    ]
}
