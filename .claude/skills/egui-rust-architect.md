---
name: egui-rust-architect
description: >
  Triggers when the user requests building, scaffolding, refactoring, styling, or optimizing
  a Rust desktop application using the egui immediate-mode GUI framework. Use this skill for
  any task involving egui, eframe, or Rust GUI development — including creating new apps from
  scratch, adding features to existing egui projects, fixing layout issues, integrating async
  backends, theming, or improving performance. Also triggers for requests mentioning "Rust
  desktop app", "Rust GUI", "immediate-mode UI", "egui widget", "egui layout", "eframe app",
  or any combination of Rust with graphical interface work. Use this even when the user doesn't
  say "egui" explicitly but describes a native Rust desktop interface. Do NOT use for web-only
  frontend work (React, HTML/CSS), retained-mode Rust GUIs (iced, Dioxus, Tauri with web views),
  or CLI-only tools.
---

# egui Rust Architect

Build, refactor, and style high-performance Rust desktop applications with egui.

## Core Mental Model: Immediate Mode

egui is an **immediate-mode** GUI. Unlike React or the web DOM, there is no persistent widget
tree. The entire UI is rebuilt every frame inside a single `update()` function. This has
critical implications that override any web-dev instincts:

- **Widgets are transient.** Never store `egui::Button`, `egui::Window`, or any widget struct
  in the app state. They're constructed inline during `update()` and discarded after the frame.
- **State lives in the app struct.** Every value that persists between frames (text input
  buffers, toggle states, scroll positions, selected tabs) must be a field on the main app
  struct or a sub-struct it owns.
- **Data flow is read-then-write.** Read `&self` fields to decide what to draw. Mutate
  `&mut self` inside interaction closures (`if ui.button("Go").clicked() { self.count += 1; }`).
- **No event listeners or callbacks.** Interaction checks are inline boolean tests, not
  registered handlers.

If you catch yourself writing code that "attaches" a handler to a widget or stores a widget
for later — stop. That's retained-mode thinking. Rethink using the pattern above.

## Project Structure

Every egui project must use this layout. A monolithic `main.rs` is never acceptable for
anything beyond a throwaway prototype.

```
my-app/
├── Cargo.toml
├── assets/
│   ├── icon.png
│   └── fonts/              # Custom .ttf/.otf files
├── src/
│   ├── main.rs             # Entry point only: NativeOptions + run_native
│   ├── app.rs              # App struct, eframe::App impl, top-level routing
│   ├── ui/
│   │   ├── mod.rs
│   │   ├── sidebar.rs      # Each complex view or widget gets its own module
│   │   ├── dashboard.rs
│   │   └── theme.rs        # Visuals, colors, fonts, spacing constants
│   └── core/
│       ├── mod.rs
│       └── ...             # Business logic — zero egui imports allowed here
```

**Why this split matters:** `src/core/` contains pure Rust logic (data processing, network
calls, domain types). It must never import `egui` or `eframe`. This keeps business logic
testable and prevents UI concerns from leaking into algorithms. `src/ui/` holds everything
visual. `src/app.rs` bridges them.

### main.rs Template

```rust
use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 768.0])
            .with_min_inner_size([400.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native(
        "My App",
        options,
        Box::new(|cc| Ok(Box::new(my_app::app::MyApp::new(cc)))),
    )
}
```

### app.rs Template

```rust
use eframe::egui;

pub struct MyApp {
    // All persistent state goes here
    pub current_tab: Tab,
    pub search_query: String,
}

#[derive(Default, PartialEq)]
pub enum Tab {
    #[default]
    Dashboard,
    Settings,
}

impl MyApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // Load fonts, configure styles here via cc.egui_ctx
        Self {
            current_tab: Tab::default(),
            search_query: String::new(),
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.current_tab, Tab::Dashboard, "Dashboard");
                ui.selectable_value(&mut self.current_tab, Tab::Settings, "Settings");
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.current_tab {
                Tab::Dashboard => crate::ui::dashboard::show(ui, self),
                Tab::Settings => crate::ui::settings::show(ui, self),
            }
        });
    }
}
```

### Cargo.toml Essentials

```toml
[dependencies]
eframe = { version = "0.31", default-features = false, features = [
    "default_fonts", "glow"   # Use "wgpu" if you need advanced rendering
] }
egui_extras = { version = "0.31", features = ["all_loaders"] }
egui_flex = "0.3"            # Always include for responsive layouts
serde = { version = "1", features = ["derive"] }  # For state persistence
image = { version = "0.25", default-features = false, features = ["png"] }

[profile.release]
opt-level = 2
lto = "thin"
# IMPORTANT: Always test in release mode for real perf numbers.
# Debug mode distorts egui performance dramatically.
```

Add `egui_taffy` instead of or alongside `egui_flex` when CSS Grid is needed.

## Responsive Layouts (Mandatory)

Every layout must use `egui_flex` (or `egui_taffy` for grid) rather than manual coordinate
math. Hard-coded positions break on different screen sizes and DPI scales.

### egui_flex Basics

```rust
use egui_flex::{Flex, FlexItem};

// Horizontal bar with items that grow proportionally
Flex::horizontal().show(ui, |flex| {
    flex.add(FlexItem::new().grow(1.0), |ui| {
        ui.label("Left section");
    });
    flex.add(FlexItem::new().grow(2.0), |ui| {
        ui.label("Center section (2x wider)");
    });
    flex.add(FlexItem::new().grow(1.0), |ui| {
        ui.label("Right section");
    });
});

// Wrap items to next row when space runs out
Flex::horizontal().wrap(true).show(ui, |flex| {
    for item in &self.items {
        flex.add(FlexItem::new().basis(200.0).grow(1.0), |ui| {
            ui.label(&item.name);
        });
    }
});
```

`egui_flex` caches widget sizes from the previous frame to resolve layout, so there's a
one-frame delay — it's visually imperceptible. `grow`, `basis`, and `wrap` work like their
CSS flexbox counterparts.

### egui_taffy for CSS Grid

When a prompt calls for a true grid (data tables, tile grids, multi-column dashboards),
use `egui_taffy` which brings the full `taffy` layout engine (CSS Block, Flexbox, Grid):

```rust
use egui_taffy::TaffyPass;
use taffy::prelude::*;

TaffyPass::new(ui, taffy::NodeId::new(0))
    .style(Style {
        display: Display::Grid,
        grid_template_columns: vec![fr(1.0), fr(2.0), fr(1.0)],
        gap: Size { width: length(8.0), height: length(8.0) },
        ..Default::default()
    })
    .show(|tpass| {
        tpass.add_child(Style::default(), |ui| { ui.label("Col 1"); });
        tpass.add_child(Style::default(), |ui| { ui.label("Col 2"); });
        tpass.add_child(Style::default(), |ui| { ui.label("Col 3"); });
    });
```

**Decision heuristic:** Use `egui_flex` for row/column flows with wrapping. Use `egui_taffy`
when you need explicit grid tracks, row/column spans, or CSS Grid-level control.

## Performance: Reactive Mode by Default

egui can repaint continuously (like a game engine) or reactively (only on user input).
**Always default to reactive mode** — it drops idle CPU usage to near-zero.

### Repaint Rules

| Situation | What to do |
|---|---|
| App is idle, waiting for user input | Do nothing — reactive mode handles it |
| Background task completed | Call `ctx.request_repaint()` from the channel callback |
| Animation is actively playing | Call `ctx.request_repaint()` conditionally while animating |
| Periodic polling needed | Use `ctx.request_repaint_after(Duration::from_millis(500))` |

**Never** put an unconditional `ctx.request_repaint()` in the `update()` body — it defeats
reactive mode and pins the CPU.

## Async Integration

Network calls, file I/O, and database queries must never run on the UI thread. The pattern
is always: spawn work on a background thread, communicate results back via channels.

Read `references/async-patterns.md` for the full Tokio integration architecture, channel
setup, and `egui-async` crate usage. The short version:

```rust
// In app struct:
pub tx: std::sync::mpsc::Sender<Request>,
pub rx: std::sync::mpsc::Receiver<Response>,

// In update(), non-blocking poll:
if let Ok(response) = self.rx.try_recv() {
    self.data = response.payload;
}

// Background thread sends result + wakes UI:
let ctx = ctx.clone();
std::thread::spawn(move || {
    let result = do_heavy_work();
    tx.send(Response { payload: result }).ok();
    ctx.request_repaint(); // Wake the UI
});
```

**Never** use `Arc<Mutex<T>>` for shared UI state if the background task holds the lock
for more than a few microseconds — the UI thread will block waiting for it. Channels are
the safe default.

## Theming and Visual Polish

Read `references/theming.md` for the full guide on colors, typography, shadows, and
animations. The essentials:

### Dark Theme Foundation

```rust
impl MyApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();

        // Modern rounded corners
        style.visuals.window_rounding = egui::Rounding::same(8.0);
        style.visuals.widgets.noninteractive.rounding = egui::Rounding::same(6.0);
        style.visuals.widgets.inactive.rounding = egui::Rounding::same(6.0);
        style.visuals.widgets.hovered.rounding = egui::Rounding::same(6.0);
        style.visuals.widgets.active.rounding = egui::Rounding::same(6.0);

        // Subtle spacing
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);

        cc.egui_ctx.set_style(style);
        Self::default()
    }
}
```

### Animated Transitions

Use `ctx.animate_bool_responsive(id, state)` for smooth hover/toggle effects instead of
instant boolean snaps:

```rust
let anim_t = ui.ctx().animate_bool_responsive(
    ui.id().with("toggle"),
    self.is_on,
);
let bg_color = egui::lerp(
    egui::Rgba::from(Color32::from_rgb(60, 60, 60))..=
    egui::Rgba::from(Color32::from_rgb(80, 160, 80)),
    anim_t,
);
```

### Custom Fonts

```rust
let mut fonts = egui::FontDefinitions::default();
fonts.font_data.insert(
    "my_font".to_owned(),
    std::sync::Arc::new(egui::FontData::from_static(
        include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    )),
);
fonts.families
    .entry(egui::FontFamily::Proportional)
    .or_default()
    .insert(0, "my_font".to_owned());
cc.egui_ctx.set_fonts(fonts);
```

Use `include_bytes!()` for all assets — it embeds them in the binary and works across
native and WASM targets without file path headaches.

## Common Anti-Patterns to Avoid

These are the mistakes that trip up LLMs most often with egui:

| Anti-pattern | Why it's wrong | Correct approach |
|---|---|---|
| Storing `egui::Window` in the app struct | Widgets are transient, not persistent objects | Store window *state* (open: bool, pos: Pos2) and construct the window in `update()` |
| `#[tokio::main]` on the entry point | Conflicts with eframe's event loop on the main thread | Spawn a Tokio runtime on a background thread |
| `.await` inside `update()` | Blocks the UI thread, freezes the app | Use channels + `try_recv()` |
| Unconditional `ctx.request_repaint()` | Pins CPU at 100%, kills battery | Only call when state actually changed |
| Manual pixel coordinates for layout | Breaks on different DPI/screen sizes | Use `egui_flex` or `egui_taffy` |
| All code in `main.rs` | Unmaintainable past ~200 lines | Use the project structure above |
| `Arc<Mutex<T>>` for UI state | Lock contention freezes rendering | Use channels for cross-thread data |

## Development Workflow

### Native Hot Reload

```bash
cargo install cargo-watch
cargo watch -x run
```

### WASM Deployment

```bash
cargo install trunk
trunk serve --open  # Hot-reloading browser dev server
```

For WASM, add to `Cargo.toml`:
```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
wasm-bindgen-futures = "0.4"
web-sys = "0.3"
```

## Decision Flowchart

When generating or modifying egui code, follow this sequence:

1. **New project?** → Scaffold the full directory structure above. No exceptions.
2. **Layout needed?** → Use `egui_flex`. Upgrade to `egui_taffy` if grid is required.
3. **Network/IO?** → Background thread + channels. Read `references/async-patterns.md`.
4. **Styling?** → Start with `Visuals::dark()`, override rounding/shadows. Read `references/theming.md`.
5. **Animation?** → `animate_bool_responsive` + `egui::lerp`. Call `request_repaint()` only while animating.
6. **New widget?** → Own file in `src/ui/`, expose a `pub fn show(ui: &mut egui::Ui, state: &mut T)`.

## Reference Files

For deep dives, read these before generating complex code in each domain:

- `references/async-patterns.md` — Tokio runtime setup, channel architectures, `egui-async` and `egui_mobius` crate patterns
- `references/theming.md` — Color systems, `egui_colors`, `drafftink_widgets`, shadow/depth, typography, pill buttons, accessibility contrast
- `references/layout-advanced.md` — Advanced `egui_taffy` grid recipes, nested flex, scroll regions, responsive breakpoints