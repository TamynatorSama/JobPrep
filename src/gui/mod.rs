pub mod app;
pub mod theme;
pub mod types;
pub mod utils;
pub mod views;

use app::InterPrepApp;

use anyhow::Result;

// ─── Public API ──────────────────────────────────────────────────────────────

pub struct UiSeed {}
impl UiSeed {
    pub fn from_args(_args: &crate::Args) -> Self {
        Self {}
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

pub fn run_gui(_seed: UiSeed) -> Result<()> {
    // Start the Python sidecar in the background so the GUI opens immediately.
    // The app polls the channel and shows a "starting" status until ready.
    let (sidecar_tx, sidecar_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = crate::sidecar::PythonSidecar::start().map_err(|e| e.to_string());
        let _ = sidecar_tx.send(result);
    });

    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("InterPrep")
            .with_inner_size([1400.0, 880.0])
            .with_min_inner_size([1100.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "InterPrep",
        opts,
        Box::new(move |cc| Ok(Box::new(InterPrepApp::new(cc, sidecar_rx)))),
    )?;
    Ok(())
}
