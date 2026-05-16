use anyhow::{anyhow, bail, Context, Result};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use std::convert::TryInto;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use wasapi::{Device, DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError, WaveFormat};

pub const DEFAULT_POLL_TIMEOUT_MS: u32 = 100;
const BUFFER_DURATION_HNS: i64 = 200_000;

#[derive(Clone, Debug)]
pub struct RecorderConfig {
    pub system_device_name: Option<String>,
    pub mic_device_name: Option<String>,
    pub output_dir: PathBuf,
    pub system_file: String,
    pub mic_file: String,
    pub poll_timeout_ms: u32,
}

#[derive(Clone, Debug)]
pub struct AudioDeviceEntry {
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceCatalog {
    pub render_devices: Vec<AudioDeviceEntry>,
    pub capture_devices: Vec<AudioDeviceEntry>,
}

#[derive(Clone, Copy, Debug)]
pub enum CaptureSource {
    SystemAudio,
    Microphone,
}

impl CaptureSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemAudio => "system audio",
            Self::Microphone => "microphone",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::SystemAudio => "system",
            Self::Microphone => "mic",
        }
    }

    fn device_direction(self) -> Direction {
        match self {
            Self::SystemAudio => Direction::Render,
            Self::Microphone => Direction::Capture,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CaptureSummary {
    pub source: CaptureSource,
    pub device_name: String,
    pub output_path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames_written: u64,
}

impl CaptureSummary {
    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames_written as f64 / self.sample_rate as f64
        }
    }
}

#[derive(Debug)]
pub struct RecordingReport {
    pub system: CaptureSummary,
    pub microphone: CaptureSummary,
}

pub struct RecordingSession {
    stop: Arc<AtomicBool>,
    system_handle: Option<JoinHandle<Result<CaptureSummary>>>,
    mic_handle: Option<JoinHandle<Result<CaptureSummary>>>,
    started_at: Instant,
    duration: Option<Duration>,
}

#[derive(Debug)]
struct CaptureJob {
    source: CaptureSource,
    selected_device_name: Option<String>,
    output_path: PathBuf,
    poll_timeout_ms: u32,
}

struct ComGuard;

impl Drop for ComGuard {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

impl RecordingSession {
    pub fn start(config: RecorderConfig, duration: Option<Duration>) -> Result<Self> {
        fs::create_dir_all(&config.output_dir).with_context(|| {
            format!(
                "failed to create output directory '{}'",
                config.output_dir.display()
            )
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let system_job = CaptureJob {
            source: CaptureSource::SystemAudio,
            selected_device_name: config.system_device_name.clone(),
            output_path: config.output_dir.join(&config.system_file),
            poll_timeout_ms: config.poll_timeout_ms,
        };
        let mic_job = CaptureJob {
            source: CaptureSource::Microphone,
            selected_device_name: config.mic_device_name.clone(),
            output_path: config.output_dir.join(&config.mic_file),
            poll_timeout_ms: config.poll_timeout_ms,
        };

        let system_handle = spawn_capture_worker(system_job, Arc::clone(&stop))?;
        let mic_handle = match spawn_capture_worker(mic_job, Arc::clone(&stop)) {
            Ok(handle) => handle,
            Err(err) => {
                stop.store(true, Ordering::SeqCst);
                let _ = join_worker(system_handle, CaptureSource::SystemAudio);
                return Err(err);
            }
        };

        Ok(Self {
            stop,
            system_handle: Some(system_handle),
            mic_handle: Some(mic_handle),
            started_at: Instant::now(),
            duration,
        })
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn is_finished(&self) -> bool {
        self.system_handle
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && self.mic_handle.as_ref().is_some_and(JoinHandle::is_finished)
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn should_auto_stop(&self) -> bool {
        self.duration
            .map(|duration| self.elapsed() >= duration)
            .unwrap_or(false)
    }

    pub fn finish(mut self) -> Result<RecordingReport> {
        self.request_stop();
        let system = join_worker(
            self.system_handle
                .take()
                .ok_or_else(|| anyhow!("system audio worker missing"))?,
            CaptureSource::SystemAudio,
        )?;
        let microphone = join_worker(
            self.mic_handle
                .take()
                .ok_or_else(|| anyhow!("microphone worker missing"))?,
            CaptureSource::Microphone,
        )?;

        Ok(RecordingReport { system, microphone })
    }
}

pub fn query_device_catalog() -> Result<DeviceCatalog> {
    let handle = thread::Builder::new()
        .name("query-devices".to_owned())
        .spawn(query_device_catalog_inner)
        .context("failed to spawn device query worker")?;

    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(anyhow!("device query worker panicked")),
    }
}

pub fn print_device_catalog(catalog: &DeviceCatalog) {
    println!("Render devices (system audio loopback targets):");
    print_devices_for_humans(&catalog.render_devices);
    println!();
    println!("Capture devices (microphones / line-in):");
    print_devices_for_humans(&catalog.capture_devices);
}

pub fn print_capture_summary(summary: &CaptureSummary) {
    println!(
        "{}: device='{}', file='{}', sample_rate={}Hz, channels={}, frames={}, duration={:.2}s",
        summary.source.label(),
        summary.device_name,
        summary.output_path.display(),
        summary.sample_rate,
        summary.channels,
        summary.frames_written,
        summary.duration_seconds()
    );
}

fn print_devices_for_humans(devices: &[AudioDeviceEntry]) {
    if devices.is_empty() {
        println!("  (no active devices found)");
        return;
    }

    for device in devices {
        let marker = if device.is_default { "default" } else { "       " };
        println!("  [{marker}] {}", device.name);
    }
}

fn collect_devices(enumerator: &DeviceEnumerator, direction: Direction) -> Result<Vec<AudioDeviceEntry>> {
    let default_id = enumerator
        .get_default_device(&direction)
        .ok()
        .and_then(|device| device.get_id().ok());
    let collection = enumerator
        .get_device_collection(&direction)
        .context("failed to enumerate devices")?;

    let mut devices = Vec::new();
    for device in &collection {
        let device = device.context("failed to inspect a device")?;
        let id = device.get_id().context("failed to read device id")?;
        let name = device
            .get_friendlyname()
            .context("failed to read device friendly name")?;
        devices.push(AudioDeviceEntry {
            is_default: default_id.as_deref() == Some(id.as_str()),
            name,
        });
    }

    Ok(devices)
}

fn query_device_catalog_inner() -> Result<DeviceCatalog> {
    let _com = init_mta()?;
    let enumerator = DeviceEnumerator::new().context("failed to create WASAPI device enumerator")?;
    Ok(DeviceCatalog {
        render_devices: collect_devices(&enumerator, Direction::Render)?,
        capture_devices: collect_devices(&enumerator, Direction::Capture)?,
    })
}

fn init_mta() -> Result<ComGuard> {
    wasapi::initialize_mta()
        .ok()
        .context("failed to initialize COM in the current thread")?;
    Ok(ComGuard)
}

fn spawn_capture_worker(
    job: CaptureJob,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<Result<CaptureSummary>>> {
    let thread_name = format!("capture-{}", job.source.short_label());
    thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || {
            let result = record_source(job, Arc::clone(&stop));
            if result.is_err() {
                stop.store(true, Ordering::SeqCst);
            }
            result
        })
        .with_context(|| format!("failed to spawn '{thread_name}' worker thread"))
}

fn join_worker(
    handle: JoinHandle<Result<CaptureSummary>>,
    source: CaptureSource,
) -> Result<CaptureSummary> {
    match handle.join() {
        Ok(result) => result.with_context(|| format!("{} capture failed", source.label())),
        Err(_) => Err(anyhow!("{} capture thread panicked", source.label())),
    }
}

fn record_source(job: CaptureJob, stop: Arc<AtomicBool>) -> Result<CaptureSummary> {
    let _com = init_mta()?;
    let enumerator = DeviceEnumerator::new().context("failed to create WASAPI device enumerator")?;
    let device = resolve_device(&enumerator, job.source, job.selected_device_name.as_deref())?;
    let device_name = device
        .get_friendlyname()
        .with_context(|| format!("failed to read {} device name", job.source.label()))?;

    let mut audio_client = device
        .get_iaudioclient()
        .with_context(|| format!("failed to create audio client for {device_name}"))?;
    let wave_format = audio_client
        .get_mixformat()
        .with_context(|| format!("failed to get mix format for {device_name}"))?;

    let wav_spec = make_wav_spec(&wave_format)?;
    let mut writer = WavWriter::create(&job.output_path, wav_spec).with_context(|| {
        format!(
            "failed to create output file '{}'",
            job.output_path.display()
        )
    })?;

    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: BUFFER_DURATION_HNS,
    };
    audio_client
        .initialize_client(&wave_format, &Direction::Capture, &mode)
        .with_context(|| format!("failed to initialize {} stream", job.source.label()))?;

    let event_handle = audio_client
        .set_get_eventhandle()
        .with_context(|| format!("failed to create event handle for {}", job.source.label()))?;
    let capture_client = audio_client
        .get_audiocaptureclient()
        .with_context(|| format!("failed to acquire capture client for {}", job.source.label()))?;

    let bytes_per_frame = wave_format.get_blockalign() as usize;
    let max_frames_per_buffer = audio_client
        .get_buffer_size()
        .with_context(|| format!("failed to read buffer size for {}", job.source.label()))?
        .max(1) as usize;
    let mut scratch = vec![0_u8; max_frames_per_buffer * bytes_per_frame];
    let mut frames_written = 0_u64;

    audio_client
        .start_stream()
        .with_context(|| format!("failed to start {} stream", job.source.label()))?;

    while !stop.load(Ordering::SeqCst) {
        match event_handle.wait_for_event(job.poll_timeout_ms) {
            Ok(()) => {}
            Err(WasapiError::EventTimeout) => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed while waiting on {}", job.source.label()))
            }
        }

        loop {
            let packet_frames = capture_client
                .get_next_packet_size()
                .with_context(|| format!("failed to query {} packet size", job.source.label()))?
                .unwrap_or(0);
            if packet_frames == 0 {
                break;
            }

            let packet_bytes = packet_frames as usize * bytes_per_frame;
            if scratch.len() < packet_bytes {
                scratch.resize(packet_bytes, 0);
            }

            let (frames_read, info) = capture_client
                .read_from_device(&mut scratch[..packet_bytes])
                .with_context(|| format!("failed to read {} data", job.source.label()))?;
            let used_bytes = frames_read as usize * bytes_per_frame;

            write_packet_as_f32(
                &mut writer,
                &wave_format,
                &scratch[..used_bytes],
                info.flags.silent,
            )?;
            frames_written += u64::from(frames_read);
        }
    }

    audio_client.stop_stream().ok();
    writer.finalize().context("failed to finalize WAV file")?;

    Ok(CaptureSummary {
        source: job.source,
        device_name,
        output_path: job.output_path,
        sample_rate: wave_format.get_samplespersec(),
        channels: wave_format.get_nchannels(),
        frames_written,
    })
}

fn resolve_device(
    enumerator: &DeviceEnumerator,
    source: CaptureSource,
    selected_name: Option<&str>,
) -> Result<Device> {
    if let Some(name) = selected_name {
        let collection = enumerator
            .get_device_collection(&source.device_direction())
            .with_context(|| format!("failed to enumerate {} devices", source.label()))?;
        return collection.get_device_with_name(name).with_context(|| {
            format!(
                "could not find {} device named '{}'",
                source.label(),
                name
            )
        });
    }

    enumerator
        .get_default_device(&source.device_direction())
        .with_context(|| format!("failed to get the default {} device", source.label()))
}

fn make_wav_spec(format: &WaveFormat) -> Result<WavSpec> {
    let _sample_type = format
        .get_subformat()
        .context("failed to inspect device sample format")?;

    Ok(WavSpec {
        channels: format.get_nchannels(),
        sample_rate: format.get_samplespersec(),
        bits_per_sample: 32,
        sample_format: WavSampleFormat::Float,
    })
}

fn write_packet_as_f32(
    writer: &mut WavWriter<std::io::BufWriter<std::fs::File>>,
    format: &WaveFormat,
    bytes: &[u8],
    silent: bool,
) -> Result<()> {
    let bits_per_sample = format.get_bitspersample();
    let bytes_per_sample = usize::from(bits_per_sample / 8);
    if bytes_per_sample == 0 {
        bail!("unsupported sample width: {bits_per_sample} bits");
    }
    if bytes.len() % bytes_per_sample != 0 {
        bail!(
            "packet size {} is not aligned to the sample width {}",
            bytes.len(),
            bytes_per_sample
        );
    }

    if silent {
        for _ in 0..(bytes.len() / bytes_per_sample) {
            writer.write_sample(0.0_f32)?;
        }
        return Ok(());
    }

    let sample_type = format
        .get_subformat()
        .context("failed to inspect packet sample format")?;
    for chunk in bytes.chunks_exact(bytes_per_sample) {
        writer.write_sample(decode_sample_to_f32(sample_type, bits_per_sample, chunk)?)?;
    }

    Ok(())
}

fn decode_sample_to_f32(sample_type: SampleType, bits_per_sample: u16, chunk: &[u8]) -> Result<f32> {
    match sample_type {
        SampleType::Float => decode_float_sample(chunk, bits_per_sample),
        SampleType::Int => decode_int_sample(chunk, bits_per_sample),
    }
}

fn decode_float_sample(chunk: &[u8], bits_per_sample: u16) -> Result<f32> {
    match bits_per_sample {
        32 => {
            let bytes: [u8; 4] = chunk
                .try_into()
                .map_err(|_| anyhow!("invalid 32-bit float sample"))?;
            Ok(f32::from_le_bytes(bytes))
        }
        _ => bail!("unsupported floating-point sample width: {bits_per_sample} bits"),
    }
}

fn decode_int_sample(chunk: &[u8], bits_per_sample: u16) -> Result<f32> {
    let sample = match bits_per_sample {
        8 => {
            let unsigned = chunk[0] as f32;
            return Ok(((unsigned - 128.0) / 128.0).clamp(-1.0, 1.0));
        }
        16 => {
            let bytes: [u8; 2] = chunk
                .try_into()
                .map_err(|_| anyhow!("invalid 16-bit integer sample"))?;
            i16::from_le_bytes(bytes) as i64
        }
        24 => decode_i24_le(chunk)? as i64,
        32 => {
            let bytes: [u8; 4] = chunk
                .try_into()
                .map_err(|_| anyhow!("invalid 32-bit integer sample"))?;
            i32::from_le_bytes(bytes) as i64
        }
        _ => bail!("unsupported PCM sample width: {bits_per_sample} bits"),
    };

    let full_scale = (1_i64 << (bits_per_sample - 1)) as f32;
    Ok(((sample as f32) / full_scale).clamp(-1.0, 1.0))
}

fn decode_i24_le(chunk: &[u8]) -> Result<i32> {
    let bytes: [u8; 3] = chunk
        .try_into()
        .map_err(|_| anyhow!("invalid 24-bit integer sample"))?;
    let raw = (bytes[0] as i32) | ((bytes[1] as i32) << 8) | ((bytes[2] as i32) << 16);
    let signed = if raw & 0x80_0000 != 0 {
        raw | !0xFF_FFFF
    } else {
        raw
    };
    Ok(signed)
}
