use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug)]
pub enum ChatEvent {
    /// Token of the final response — appended to the bubble body.
    Token(String),
    /// Status update from the agent (tool call, search step, etc.).
    /// Rendered above the bubble in a collapsible "thinking" disclosure.
    Log(String),
    /// Application-tailoring scorecard (verbatim match %, hire/no-hire, …).
    /// JSON object kept as-is so the GUI can format it.
    Scorecard(serde_json::Value),
    /// Decoded .docx bytes for the tailored resume (saved to disk by the app).
    ResumeDocx(Vec<u8>),
    /// The plain-text tailored resume — handed to the company-research step
    /// as background context.
    TailoredResume(String),
    Done,
    Err(String),
}

/// (role, content) pairs of prior turns, oldest first. `role` is "user" or "assistant".
pub type ChatHistory = Vec<(String, String)>;

pub fn stream_chat(
    base_url: &str,
    message: String,
    job_context: String,
    history: ChatHistory,
    mode: &str,
    api_key: String,
    tx: mpsc::Sender<ChatEvent>,
) {
    let url = format!("{base_url}/chat/stream");
    let history_json: Vec<_> = history
        .into_iter()
        .map(|(role, content)| serde_json::json!({"role": role, "content": content}))
        .collect();
    spawn_sse(
        url,
        serde_json::json!({
            "message": message,
            "job_context": job_context,
            "history": history_json,
            "mode": mode,
            "api_key": api_key,
        }),
        tx,
    );
}

pub fn stream_company_research(
    base_url: &str,
    company: String,
    role: String,
    location: String,
    job_description: String,
    tailored_resume: String,
    api_key: String,
    glassdoor_email: String,
    glassdoor_password: String,
    indeed_email: String,
    indeed_password: String,
    tx: mpsc::Sender<ChatEvent>,
) {
    let url = format!("{base_url}/company-research/stream");
    spawn_sse(
        url,
        serde_json::json!({
            "company": company,
            "role": role,
            "location": location,
            "job_description": job_description,
            "tailored_resume": tailored_resume,
            "api_key": api_key,
            "glassdoor_email": glassdoor_email,
            "glassdoor_password": glassdoor_password,
            "indeed_email": indeed_email,
            "indeed_password": indeed_password,
        }),
        tx,
    );
}

pub fn stream_application_tailor(
    base_url: &str,
    company: String,
    role: String,
    location: String,
    job_description: String,
    master_resumes: Vec<(String, String)>,
    api_key: String,
    tx: mpsc::Sender<ChatEvent>,
) {
    let url = format!("{base_url}/application/tailor-resume");
    let resumes_json: Vec<_> = master_resumes
        .into_iter()
        .map(|(name, text)| serde_json::json!({ "name": name, "text": text }))
        .collect();
    spawn_sse(
        url,
        serde_json::json!({
            "company": company,
            "role": role,
            "location": location,
            "job_description": job_description,
            "master_resumes": resumes_json,
            "api_key": api_key,
        }),
        tx,
    );
}

pub fn stream_knockout_screen(
    base_url: &str,
    company: String,
    role: String,
    location: String,
    job_description: String,
    tailored_resume: String,
    api_key: String,
    tx: mpsc::Sender<ChatEvent>,
) {
    let url = format!("{base_url}/application/knockout-screen");
    spawn_sse(
        url,
        serde_json::json!({
            "company": company,
            "role": role,
            "location": location,
            "job_description": job_description,
            "tailored_resume": tailored_resume,
            "api_key": api_key,
        }),
        tx,
    );
}

pub fn stream_research(
    base_url: &str,
    company: String,
    role: String,
    job_description: String,
    api_key: String,
    tx: mpsc::Sender<ChatEvent>,
) {
    let url = format!("{base_url}/research/stream");
    spawn_sse(
        url,
        serde_json::json!({
            "company": company,
            "role": role,
            "job_description": job_description,
            "api_key": api_key,
        }),
        tx,
    );
}

fn spawn_sse(url: String, body: serde_json::Value, tx: mpsc::Sender<ChatEvent>) {
    std::thread::spawn(move || {
        let client = match reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(ChatEvent::Err(e.to_string()));
                return;
            }
        };

        let resp = match client.post(&url).json(&body).send() {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(ChatEvent::Err(format!("Could not reach backend: {e}")));
                return;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            let detail = if body.trim().is_empty() {
                status.to_string()
            } else {
                format!("{status}: {body}")
            };
            let _ = tx.send(ChatEvent::Err(format!("Backend returned {detail}")));
            return;
        }

        let reader = BufReader::new(resp);
        let mut terminal_event = false;
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    let _ = tx.send(ChatEvent::Err(format!("Backend stream read failed: {e}")));
                    terminal_event = true;
                    break;
                }
            };
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            match val["type"].as_str() {
                Some("token") => {
                    if let Some(content) = val["content"].as_str() {
                        if tx.send(ChatEvent::Token(content.to_string())).is_err() {
                            break;
                        }
                    }
                }
                Some("stage") => {
                    if let Some(content) = val["content"].as_str() {
                        if tx.send(ChatEvent::Log(content.to_string())).is_err() {
                            break;
                        }
                    }
                }
                Some("scorecard") => {
                    let card = val["content"].clone();
                    if tx.send(ChatEvent::Scorecard(card)).is_err() {
                        break;
                    }
                }
                Some("resume_docx") => {
                    use base64::Engine;
                    if let Some(b64) = val["content"].as_str() {
                        match base64::engine::general_purpose::STANDARD.decode(b64) {
                            Ok(bytes) => {
                                if tx.send(ChatEvent::ResumeDocx(bytes)).is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(ChatEvent::Err(
                                    format!("Failed to decode resume.docx: {e}"),
                                ));
                            }
                        }
                    }
                }
                Some("tailored_resume") => {
                    if let Some(content) = val["content"].as_str() {
                        if tx.send(ChatEvent::TailoredResume(content.to_string())).is_err() {
                            break;
                        }
                    }
                }
                Some("done") => {
                    let _ = tx.send(ChatEvent::Done);
                    terminal_event = true;
                    break;
                }
                Some("error") => {
                    let msg = val["content"].as_str().unwrap_or("Unknown error");
                    let _ = tx.send(ChatEvent::Err(msg.to_string()));
                    terminal_event = true;
                    break;
                }
                _ => {}
            }
        }
        if !terminal_event {
            let _ = tx.send(ChatEvent::Err("Backend stream ended before completion.".to_owned()));
        }
    });
}
