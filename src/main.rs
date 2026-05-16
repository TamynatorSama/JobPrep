mod backend_client;
mod credentials;
mod gui;
mod jobs_store;
mod recorder;
mod sidecar;
mod storage;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(
    name = "windows-dual-audio-recorder",
    version,
    about = "Record Windows system audio and microphone audio into separate WAV files."
)]
struct Args {
    #[arg(long, help = "Launch the desktop UI.")]
    gui: bool,

    #[arg(long, help = "List active render and capture devices, then exit.")]
    list_devices: bool,

    #[arg(
        long,
        default_value_t = 10,
        help = "Recording length in seconds for headless mode. Use 0 to record until Ctrl+C."
    )]
    seconds: u64,

    #[arg(long, default_value = "captures", help = "Folder for output WAV files.")]
    out_dir: PathBuf,

    #[arg(
        long,
        default_value = "system_audio.wav",
        help = "Filename for captured system audio."
    )]
    system_file: String,

    #[arg(
        long,
        default_value = "microphone.wav",
        help = "Filename for captured microphone audio."
    )]
    mic_file: String,

    #[arg(
        long,
        help = "Friendly name of the playback device to loop back. Defaults to the current Windows default playback device."
    )]
    system_device: Option<String>,

    #[arg(
        long,
        help = "Friendly name of the microphone/capture device. Defaults to the current Windows default capture device."
    )]
    mic_device: Option<String>,

    #[arg(
        long,
        default_value_t = recorder::DEFAULT_POLL_TIMEOUT_MS,
        help = "How long each capture thread waits before checking whether recording should stop."
    )]
    poll_timeout_ms: u32,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let raw_args: Vec<_> = std::env::args_os().skip(1).collect();
    let args = Args::parse();

    if raw_args.is_empty() || args.gui {
        return gui::run_gui(gui::UiSeed::from_args(&args));
    }

    if args.list_devices {
        let catalog = recorder::query_device_catalog()?;
        recorder::print_device_catalog(&catalog);
        return Ok(());
    }

    let stop = Arc::new(AtomicBool::new(false));
    install_ctrlc_handler(Arc::clone(&stop))?;

    let duration = duration_from_seconds(args.seconds);
    let config = recorder::RecorderConfig {
        system_device_name: args.system_device.clone(),
        mic_device_name: args.mic_device.clone(),
        output_dir: args.out_dir.clone(),
        system_file: args.system_file.clone(),
        mic_file: args.mic_file.clone(),
        poll_timeout_ms: args.poll_timeout_ms,
    };

    println!(
        "Recording system audio -> {}",
        args.out_dir.join(&args.system_file).display()
    );
    println!(
        "Recording microphone -> {}",
        args.out_dir.join(&args.mic_file).display()
    );
    if args.seconds == 0 {
        println!("Press Ctrl+C to stop recording.");
    } else {
        println!("Recording for {} second(s)...", args.seconds);
    }

    let session = recorder::RecordingSession::start(config, duration)?;

    while !session.is_finished() {
        if stop.load(Ordering::SeqCst) || session.should_auto_stop() {
            session.request_stop();
        }
        thread::sleep(Duration::from_millis(200));
    }

    let report = session.finish()?;
    println!();
    recorder::print_capture_summary(&report.system);
    recorder::print_capture_summary(&report.microphone);

    Ok(())
}

fn install_ctrlc_handler(stop: Arc<AtomicBool>) -> Result<()> {
    ctrlc::set_handler(move || {
        stop.store(true, Ordering::SeqCst);
    })
    .context("failed to install Ctrl+C handler")
}

fn duration_from_seconds(seconds: u64) -> Option<Duration> {
    if seconds == 0 {
        None
    } else {
        Some(Duration::from_secs(seconds))
    }
}
