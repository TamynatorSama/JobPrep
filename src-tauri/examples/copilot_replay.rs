//! Phase 2 gate measurement: replay WAV questions through the copilot's real
//! live-listening pipeline (Resampler16k → Silero → Tracker → Controller with
//! the sidecar's streaming Whisper) in real time, start each answer exactly as
//! the app does, and time "interviewer stops → first answer token".
//!
//! Driven by `backend/bench/latency_harness.py --pipeline`, which spawns the
//! sidecar, synthesizes the questions, and pipes this config on stdin (it
//! carries API keys, so it never touches disk):
//!   {"url": "...", "llm": {...}, "job_context": "...", "skip_llm": false,
//!    "items": [{"wav": "path", "text": "...", "speech_end_s": 3.21}]}
//! One JSON line per item goes to stdout. Not measured: WASAPI's own capture
//! buffering (~10–20 ms).

use std::io::{BufRead, BufReader, Read};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use interprep_lib::copilot_listen::{Controller, Ev, FramePump, HttpAsr, Sink};
use interprep_lib::vad::{Resampler16k, SR};
use serde_json::{json, Value};

#[derive(Default)]
struct Log {
    events: Vec<(Instant, String, Value)>,
    answers: Vec<Answer>,
}

struct Answer {
    stream_id: String,
    started: Instant,
    first_token: Option<Instant>,
    done: Option<Instant>,
    text: String,
    cancelled: bool,
    error: String,
}

struct ReplaySink {
    url: String,
    llm: Value,
    job_context: String,
    skip_llm: bool,
    log: Arc<Mutex<Log>>,
}

impl Sink for ReplaySink {
    fn emit(&self, event: &str, payload: Value) {
        self.log.lock().unwrap().events.push((Instant::now(), event.to_string(), payload));
    }

    fn start_answer(&self, question: &str, stream_id: &str) {
        let started = Instant::now();
        {
            let mut log = self.log.lock().unwrap();
            log.answers.push(Answer {
                stream_id: stream_id.to_string(),
                started,
                first_token: None,
                done: None,
                text: String::new(),
                cancelled: false,
                error: String::new(),
            });
        }
        if self.skip_llm {
            return;
        }
        let (url, log, sid) = (self.url.clone(), Arc::clone(&self.log), stream_id.to_string());
        let body = json!({
            "message": question, "job_context": self.job_context, "history": [],
            "mode": "copilot", "llm": self.llm, "documents": [],
        });
        std::thread::spawn(move || {
            let update = |f: &mut dyn FnMut(&mut Answer)| {
                let mut l = log.lock().unwrap();
                if let Some(a) = l.answers.iter_mut().find(|a| a.stream_id == sid) {
                    f(a);
                }
            };
            let resp = reqwest::blocking::Client::new()
                .post(format!("{url}/chat/stream"))
                .json(&body)
                .timeout(Duration::from_secs(120))
                .send();
            let resp = match resp {
                Ok(r) => r,
                Err(e) => return update(&mut |a| a.error = e.to_string()),
            };
            for line in BufReader::new(resp).lines() {
                let Ok(line) = line else { break };
                let cancelled = log.lock().unwrap().answers.iter().any(|a| a.stream_id == sid && a.cancelled);
                if cancelled {
                    return; // drop the connection, like backend_client::cancel_stream
                }
                let Some(data) = line.strip_prefix("data: ") else { continue };
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                match v["type"].as_str() {
                    Some("token") => {
                        let tok = v["content"].as_str().unwrap_or("").to_string();
                        update(&mut |a| {
                            a.first_token.get_or_insert_with(Instant::now);
                            a.text.push_str(&tok);
                        });
                    }
                    Some("error") => {
                        let msg = v["content"].as_str().unwrap_or("error").to_string();
                        return update(&mut |a| a.error = msg.clone());
                    }
                    Some("done") => break,
                    _ => {}
                }
            }
            update(&mut |a| a.done = Some(Instant::now()));
        });
    }

    fn cancel_answer(&self, stream_id: &str) {
        let mut log = self.log.lock().unwrap();
        if let Some(a) = log.answers.iter_mut().find(|a| a.stream_id == stream_id) {
            a.cancelled = true;
        }
    }
}

fn read_wav_16k(path: &str) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect(),
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
    };
    let mono: Vec<f32> = raw.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    let mut out = Vec::new();
    Resampler16k::new(spec.sample_rate).push(&mono, &mut out);
    out
}

/// Signed milliseconds from `a` to `b`.
fn ms(a: Instant, b: Instant) -> Option<i64> {
    Some(if b >= a { (b - a).as_millis() as i64 } else { -((a - b).as_millis() as i64) })
}

fn main() {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).expect("stdin");
    let cfg: Value = serde_json::from_str(&raw).expect("config json");
    let url = cfg["url"].as_str().expect("url").to_string();
    let log = Arc::new(Mutex::new(Log::default()));
    let sink = ReplaySink {
        url: url.clone(),
        llm: cfg["llm"].clone(),
        job_context: cfg["job_context"].as_str().unwrap_or("").to_string(),
        skip_llm: cfg["skip_llm"].as_bool().unwrap_or(false),
        log: Arc::clone(&log),
    };

    let (tx, rx) = mpsc::channel::<Ev>();
    let ctl = std::thread::spawn(move || {
        let mut c = Controller::new(sink, HttpAsr::new(url));
        while let Ok(ev) = rx.recv() {
            c.handle(ev);
        }
        c.shutdown();
    });

    let mut pump = FramePump::new().expect("silero");
    let block = (SR as usize) / 100; // 10 ms, like a WASAPI period
    for (i, item) in cfg["items"].as_array().expect("items").iter().enumerate() {
        let audio = read_wav_16k(item["wav"].as_str().unwrap());
        let speech_end_s = item["speech_end_s"].as_f64().unwrap();
        let (ev_mark, ans_mark) = {
            let l = log.lock().unwrap();
            (l.events.len(), l.answers.len())
        };
        let t0 = Instant::now();
        for (k, chunk) in audio.chunks(block).enumerate() {
            let due = t0 + Duration::from_secs_f64((k + 1) as f64 * block as f64 / SR as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            pump.push(chunk, false, |ev| {
                let _ = tx.send(ev);
            })
            .expect("vad");
        }
        // Let the answer finish (or give up) before the next question.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let settled = {
                let l = log.lock().unwrap();
                l.answers[ans_mark..].iter().all(|a| a.cancelled || a.done.is_some() || !a.error.is_empty())
                    && l.events[ev_mark..].iter().any(|(_, e, _)| e == "copilot:commit")
            };
            if settled || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let stop = t0 + Duration::from_secs_f64(speech_end_s);
        let l = log.lock().unwrap();
        let evs = &l.events[ev_mark..];
        let questions: Vec<&(Instant, String, Value)> = evs.iter().filter(|(_, e, _)| e == "copilot:question").collect();
        let last_q = questions.last();
        let answers = &l.answers[ans_mark..];
        let kept = answers.iter().rev().find(|a| !a.cancelled);
        let partials = evs.iter().filter(|(_, e, _)| e == "copilot:partial").count();
        let row = json!({
            "i": i,
            "text": item["text"],
            "transcript": last_q.map(|(_, _, p)| p["text"].clone()).unwrap_or(Value::Null),
            "attempts": questions.len(),
            "restarts": evs.iter().filter(|(_, e, _)| e == "copilot:resume").count(),
            "speculative": last_q.map(|(_, _, p)| p["speculative"].clone()).unwrap_or(Value::Null),
            "asr_wait_ms": last_q.map(|(_, _, p)| p["asrMs"].clone()).unwrap_or(Value::Null),
            "partials": partials,
            // Interviewer stops → answer request sent (endpoint + transcript).
            "endpoint_ms": last_q.and_then(|(t, _, _)| ms(stop, *t)),
            "ttft_ms": kept.and_then(|a| a.first_token.and_then(|f| ms(a.started, f))),
            "stop_to_first_token_ms": kept.and_then(|a| a.first_token.and_then(|f| ms(stop, f))),
            "total_ms": kept.and_then(|a| a.done.and_then(|d| ms(a.started, d))),
            "answer_words": kept.map(|a| a.text.split_whitespace().count()),
            "answer": kept.map(|a| a.text.clone()),
            "error": kept.map(|a| a.error.clone()).unwrap_or_default(),
        });
        println!("{row}");
    }
    drop(tx);
    let _ = ctl.join();
}
