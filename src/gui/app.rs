use std::sync::mpsc;
use eframe::egui::{self, Stroke};
use crate::gui::types::*;
use crate::gui::theme::*;
use crate::gui::utils::*;
use crate::backend_client::ChatEvent;
use crate::sidecar::PythonSidecar;
use crate::storage::ResumeStore;

// ─── App state ────────────────────────────────────────────────────────────────

pub struct InterPrepApp {
    pub jobs: Vec<Job>,
    pub selected_job:  Option<usize>,
    pub selected_chat: Option<usize>,
    pub sidebar_open:  bool,
    pub screen:        Screen,
    pub draft:         String,

    pub show_new_job: bool,
    /// Whether the "Archived" disclosure in the sidebar is currently expanded.
    pub show_archived: bool,
    pub nj_company:   String,
    pub nj_role:      String,
    pub nj_location:  String,
    pub nj_job_description: String,
    pub nj_status:    JobStatus,
    pub nj_notes:     String,
    pub nj_error:     Option<String>,
    /// Which master resume to tailor from. `None` = let the LLM auto-pick;
    /// `Some(id)` = force-use that resume only.
    pub nj_resume_id: Option<u64>,

    pub show_settings:    bool,
    /// Tracks the previous frame's `show_settings` so we can detect the
    /// open→closed transition and autosave credentials at that moment.
    settings_open_prev:   bool,
    pub settings_tab:     usize,
    pub gemini_api_key:   String,
    pub gemini_key_visible: bool,

    pub glassdoor_email:    String,
    pub glassdoor_password: String,
    pub glassdoor_pw_visible: bool,
    pub indeed_email:       String,
    pub indeed_password:    String,
    pub indeed_pw_visible:  bool,

    pub gantt_offset: i32,

    pub se_job:         Option<usize>,
    pub se_stage:       Option<usize>,
    pub se_outcome:     String,
    pub se_notes:       String,
    pub se_popover_pos: egui::Pos2,

    pub next_job_id:  usize,
    pub next_chat_id: usize,

    // ── Backend lifecycle ────────────────────────────────────────────────────
    /// Receives the sidecar once the background startup thread finishes.
    pub sidecar_rx: Option<mpsc::Receiver<Result<PythonSidecar, String>>>,
    pub sidecar:    Option<PythonSidecar>,
    pub backend_url: Option<String>,
    pub backend_status: BackendStatus,

    // ── Streaming state ──────────────────────────────────────────────────────
    pub chat_rx:          Option<mpsc::Receiver<ChatEvent>>,
    pub streaming_job_id: Option<usize>,
    pub streaming_chat_id: Option<usize>,

    // ── Report viewer ────────────────────────────────────────────────────────
    pub show_report_modal: bool,
    pub report_modal_text: String,

    // ── Resume library (Settings → Resume) ──────────────────────────────────
    pub resume_store: ResumeStore,
    /// True when the "Add a resume" inline form is open.
    pub adding_resume:    bool,
    pub new_resume_name:  String,
    pub new_resume_text:  String,
    pub resume_form_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackendStatus {
    Starting,
    Ready,
    Failed(String),
}

fn take_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

// ─── Constructor ─────────────────────────────────────────────────────────────

impl InterPrepApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        sidecar_rx: mpsc::Receiver<Result<PythonSidecar, String>>,
    ) -> Self {
        apply_theme(&cc.egui_ctx);
        // Load API key / scraper logins from the OS keychain (Windows
        // Credential Manager). Missing entries return empty strings.
        let creds = crate::credentials::Credentials::load();
        // Load persisted jobs from %LOCALAPPDATA%\InterPrep\jobs.json, falling
        // back to the seed sample on first run or unreadable file.
        let jobs = crate::jobs_store::load().unwrap_or_else(sample_jobs);

        // Pick first non-archived job as the initial selection; if there are
        // none (all archived or empty list), leave nothing selected.
        let initial_job  = jobs.iter().find(|j| !j.archived).map(|j| j.id);
        let initial_chat = initial_job
            .and_then(|jid| jobs.iter().find(|j| j.id == jid))
            .and_then(|j| j.chats.first())
            .map(|c| c.id);

        // Seed id allocators *past* the largest persisted id so we don't reuse.
        let next_job_id  = jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        let next_chat_id = jobs.iter()
            .flat_map(|j| j.chats.iter().map(|c| c.id))
            .max().unwrap_or(0) + 1;

        Self {
            jobs,
            selected_job:  initial_job,
            selected_chat: initial_chat,
            sidebar_open:  true,
            screen:        Screen::Chat,
            draft:         String::new(),
            show_new_job:  false,
            show_archived: false,
            nj_company:    String::new(),
            nj_role:       String::new(),
            nj_location:   String::new(),
            nj_job_description: String::new(),
            nj_status:     JobStatus::Applied,
            nj_notes:      String::new(),
            nj_error:      None,
            nj_resume_id:  None,
            show_settings:     false,
            settings_open_prev: false,
            settings_tab:      0,
            gemini_api_key:    creds.gemini_api_key,
            gemini_key_visible: false,
            glassdoor_email:    creds.glassdoor_email,
            glassdoor_password: creds.glassdoor_password,
            glassdoor_pw_visible: false,
            indeed_email:       creds.indeed_email,
            indeed_password:    creds.indeed_password,
            indeed_pw_visible:  false,
            gantt_offset:  -14,
            se_job:        None,
            se_stage:      None,
            se_outcome:    String::new(),
            se_notes:      String::new(),
            se_popover_pos: egui::Pos2::ZERO,
            next_job_id,
            next_chat_id,
            sidecar_rx:    Some(sidecar_rx),
            sidecar:       None,
            backend_url:   None,
            backend_status: BackendStatus::Starting,
            chat_rx:          None,
            streaming_job_id: None,
            streaming_chat_id: None,
            show_report_modal: false,
            report_modal_text: String::new(),
            resume_store:      ResumeStore::load(),
            adding_resume:     false,
            new_resume_name:   String::new(),
            new_resume_text:   String::new(),
            resume_form_error: None,
        }
    }

    pub fn selected_job(&self) -> Option<&Job> {
        let id = self.selected_job?;
        self.jobs.iter().find(|j| j.id == id)
    }

    pub fn selected_thread(&self) -> Option<&ChatThread> {
        let job = self.selected_job()?;
        let cid = self.selected_chat?;
        job.chats.iter().find(|c| c.id == cid)
    }

    /// Writes the current jobs list to `%LOCALAPPDATA%\InterPrep\jobs.json`.
    /// Called after any mutation that should survive a restart.
    pub fn save_jobs(&self) {
        crate::jobs_store::save(&self.jobs);
    }

    /// Marks a job as archived (hidden from the main sidebar list).
    pub fn archive_job(&mut self, job_id: usize) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.archived = true;
            job.sidebar_expanded = false;
        }
        // If we just archived the currently-selected job, pick the next
        // non-archived job so the workspace doesn't show a stale header.
        if self.selected_job == Some(job_id) {
            let next = self.jobs.iter().find(|j| !j.archived).map(|j| j.id);
            self.selected_job  = next;
            self.selected_chat = next
                .and_then(|id| self.jobs.iter().find(|j| j.id == id))
                .and_then(|j| j.chats.first())
                .map(|c| c.id);
        }
        self.save_jobs();
    }

    /// Restores an archived job back to the main list.
    pub fn unarchive_job(&mut self, job_id: usize) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.archived = false;
        }
        self.save_jobs();
    }

    /// Permanently removes a job and all of its threads.
    pub fn delete_job(&mut self, job_id: usize) {
        self.jobs.retain(|j| j.id != job_id);
        if self.selected_job == Some(job_id) {
            let next = self.jobs.iter().find(|j| !j.archived).map(|j| j.id);
            self.selected_job  = next;
            self.selected_chat = next
                .and_then(|id| self.jobs.iter().find(|j| j.id == id))
                .and_then(|j| j.chats.first())
                .map(|c| c.id);
        }
        self.save_jobs();
    }

    /// Persists the current credential fields to the OS keychain. Cheap; fine
    /// to call on every settings-modal close and on app exit.
    pub fn save_credentials(&self) {
        crate::credentials::Credentials {
            gemini_api_key:    self.gemini_api_key.clone(),
            glassdoor_email:   self.glassdoor_email.clone(),
            glassdoor_password:self.glassdoor_password.clone(),
            indeed_email:      self.indeed_email.clone(),
            indeed_password:   self.indeed_password.clone(),
        }.save();
    }

    pub fn selected_thread_mut(&mut self) -> Option<&mut ChatThread> {
        let job_id  = self.selected_job?;
        let chat_id = self.selected_chat?;
        let job = self.jobs.iter_mut().find(|j| j.id == job_id)?;
        job.chats.iter_mut().find(|c| c.id == chat_id)
    }

    fn job_context_for(&self, job_id: usize, include_research_report: bool) -> String {
        let Some(job) = self.jobs.iter().find(|j| j.id == job_id) else {
            return String::new();
        };

        let mut ctx = format!("Company: {}\nRole: {}", job.company, job.role);
        if !job.location.trim().is_empty() {
            ctx.push_str(&format!("\nLocation: {}", job.location));
        }
        if !job.job_description.trim().is_empty() {
            ctx.push_str("\n\nJob Description:\n");
            ctx.push_str(&take_chars(&job.job_description, 3000));
        }
        if include_research_report {
            if let Some(report) = self.company_research_report_for(job) {
                ctx.push_str("\n\nCompany Research Report:\n");
                ctx.push_str(&take_chars(&report, 8000));
            }
        }
        ctx
    }

    fn company_research_report_for(&self, job: &Job) -> Option<String> {
        crate::storage::load_company_research_report(job.id).or_else(|| {
            job.chats
                .iter()
                .rev()
                .find(|t| t.title == "Company Research")
                .and_then(|thread| {
                    thread
                        .messages
                        .iter()
                        .rev()
                        .find(|m| {
                            m.role == MsgRole::Ai
                                && !m.streaming
                                && !m.text.trim().is_empty()
                        })
                        .map(|m| m.text.clone())
                })
        })
    }

    // ─── Backend polling ─────────────────────────────────────────────────────

    pub fn poll_backend(&mut self, ctx: &egui::Context) {
        // Check if the sidecar startup thread finished
        if let Some(sidecar_result) = self.sidecar_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.sidecar_rx = None;
            match sidecar_result {
                Ok(sidecar) => {
                    self.backend_url = Some(sidecar.base_url());
                    self.sidecar = Some(sidecar);
                    self.backend_status = BackendStatus::Ready;
                }
                Err(e) => {
                    self.backend_status = BackendStatus::Failed(e);
                }
            }
        }

        // Still starting — keep polling
        if self.sidecar_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Drain streaming chat events
        let events: Vec<ChatEvent> = self
            .chat_rx
            .as_ref()
            .map(|rx| std::iter::from_fn(|| rx.try_recv().ok()).collect())
            .unwrap_or_default();

        let mut stream_done = false;
        let mut should_save_jobs = false;
        // After consuming Done, if the just-finished thread was Application
        // Prep, automatically start the Company Research step on the same job.
        let mut chain_company_research_for: Option<usize> = None;

        for event in events {
            let (jid, cid) = match (self.streaming_job_id, self.streaming_chat_id) {
                (Some(j), Some(c)) => (j, c),
                _ => { stream_done = true; break; }
            };
            match event {
                ChatEvent::Token(tok) => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == jid) {
                        if let Some(thread) = job.chats.iter_mut().find(|c| c.id == cid) {
                            if let Some(msg) = thread.messages.last_mut() {
                                if msg.streaming {
                                    msg.text.push_str(&tok);
                                }
                            }
                        }
                    }
                    ctx.request_repaint();
                }
                ChatEvent::Log(line) => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == jid) {
                        if let Some(thread) = job.chats.iter_mut().find(|c| c.id == cid) {
                            if let Some(msg) = thread.messages.last_mut() {
                                if msg.streaming {
                                    msg.logs.push(line);
                                }
                            }
                        }
                    }
                    ctx.request_repaint();
                }
                ChatEvent::Scorecard(card) => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == jid) {
                        job.scorecard = Some(card);
                    }
                    should_save_jobs = true;
                    ctx.request_repaint();
                }
                ChatEvent::ResumeDocx(bytes) => {
                    if let Some(path) = crate::storage::save_resume_docx(jid, &bytes) {
                        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == jid) {
                            job.resume_docx_path = Some(path.to_string_lossy().into_owned());
                        }
                        should_save_jobs = true;
                    } else {
                        eprintln!("resume.docx save failed for job {jid}");
                    }
                    ctx.request_repaint();
                }
                ChatEvent::TailoredResume(text) => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == jid) {
                        job.tailored_resume = Some(text);
                    }
                    should_save_jobs = true;
                }
                ChatEvent::Done => {
                    let mut report_to_save = None;
                    let mut finished_app_prep = false;
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == jid) {
                        if let Some(thread) = job.chats.iter_mut().find(|c| c.id == cid) {
                            let is_company_research = thread.title == "Company Research";
                            let is_app_prep         = thread.title == "Application Prep";
                            if let Some(msg) = thread.messages.last_mut() {
                                msg.streaming = false;
                                if is_company_research && !msg.text.trim().is_empty() {
                                    report_to_save = Some(msg.text.clone());
                                }
                            }
                            if is_company_research {
                                job.research_status = "Ready".to_owned();
                            } else if is_app_prep {
                                job.research_status = "Resume tailored".to_owned();
                                finished_app_prep = true;
                            } else if job.research_status == "Analyzing…" {
                                job.research_status = "Analysis ready".to_owned();
                            }
                        }
                    }
                    if let Some(report) = report_to_save {
                        if crate::storage::save_company_research_report(jid, &report).is_none() {
                            eprintln!("company research report save failed for job {jid}");
                        }
                    }
                    if finished_app_prep {
                        chain_company_research_for = Some(jid);
                    }
                    should_save_jobs = true;
                    stream_done = true;
                }
                ChatEvent::Err(e) => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == jid) {
                        if let Some(thread) = job.chats.iter_mut().find(|c| c.id == cid) {
                            let is_company_research = thread.title == "Company Research";
                            let is_app_prep         = thread.title == "Application Prep";
                            if let Some(msg) = thread.messages.last_mut() {
                                if msg.streaming {
                                    msg.text = format!("**Error:** {e}");
                                    msg.streaming = false;
                                }
                            }
                            if is_company_research {
                                job.research_status = "Research failed".to_owned();
                            } else if is_app_prep {
                                job.research_status = "Tailor failed".to_owned();
                            } else if job.research_status == "Analyzing…" {
                                job.research_status = "Analysis failed".to_owned();
                            }
                        }
                    }
                    should_save_jobs = true;
                    stream_done = true;
                }
            }
        }

        if should_save_jobs {
            self.save_jobs();
        }

        if stream_done {
            self.chat_rx = None;
            self.streaming_job_id = None;
            self.streaming_chat_id = None;
        }

        // Chain: Application Prep → Company Research (uses the tailored resume).
        if let Some(jid) = chain_company_research_for {
            self.start_company_research(jid);
        }

        // Keep repainting while a stream is active so tokens appear promptly
        if self.chat_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    // ─── Chat actions ─────────────────────────────────────────────────────────

    pub fn send_message(&mut self) {
        let text = self.draft.trim().to_owned();
        if text.is_empty() { return; }
        self.draft.clear();

        let Some(job_id) = self.selected_job else { return; };

        if self.selected_chat.is_none() {
            let chat_id = self.next_chat_id;
            self.next_chat_id += 1;
            let thread = ChatThread {
                id: chat_id,
                title: text.chars().take(40).collect(),
                messages: Vec::new(),
            };
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                job.chats.push(thread);
            }
            self.selected_chat = Some(chat_id);
        }

        let chat_id = match self.selected_chat {
            Some(c) => c,
            None => return,
        };

        // Build job context for the system prompt, including the saved company
        // dossier when one exists.
        let job_context = self.job_context_for(job_id, true);

        // Snapshot prior conversation BEFORE we push the new user message,
        // so `history` doesn't include the message we're about to send.
        let (history, mode) = self
            .selected_thread()
            .map(|t| (Self::build_history(t), Self::mode_for_thread(t)))
            .unwrap_or_default();

        if let Some(thread) = self.selected_thread_mut() {
            thread.messages.push(ChatMsg::user(&text));
            thread.messages.push(ChatMsg::ai_streaming());
        }

        if let Some(url) = &self.backend_url {
            let (tx, rx) = mpsc::channel();
            crate::backend_client::stream_chat(
                url,
                text,
                job_context,
                history,
                &mode,
                self.gemini_api_key.clone(),
                tx,
            );
            self.chat_rx = Some(rx);
            self.streaming_job_id = Some(job_id);
            self.streaming_chat_id = Some(chat_id);
        } else {
            // Backend not ready yet — show a status message
            let status_msg = match &self.backend_status {
                BackendStatus::Starting => "Backend is still starting up, please wait a moment and try again.",
                BackendStatus::Failed(_) => "Backend failed to start. Set your INTERPREP_BACKEND_DIR or check Python installation.",
                BackendStatus::Ready => "Backend ready but URL missing — this is a bug.",
            };
            if let Some(thread) = self.selected_thread_mut() {
                if let Some(msg) = thread.messages.last_mut() {
                    if msg.streaming {
                        msg.text = status_msg.to_owned();
                        msg.streaming = false;
                    }
                }
            }
        }
    }

    /// Returns `("user"|"assistant", content)` pairs for every completed
    /// message in `thread`, in chronological order. Filters out empties
    /// and still-streaming messages.
    fn build_history(thread: &ChatThread) -> crate::backend_client::ChatHistory {
        thread
            .messages
            .iter()
            .filter(|m| !m.streaming && !m.text.trim().is_empty())
            .map(|m| {
                let role = match m.role {
                    MsgRole::User => "user",
                    MsgRole::Ai   => "assistant",
                };
                (role.to_owned(), m.text.clone())
            })
            .collect()
    }

    /// "interviewer" for mock-interview threads, "coach" everywhere else.
    fn mode_for_thread(thread: &ChatThread) -> String {
        if thread.title == "Mock Interview" { "interviewer".to_owned() } else { "coach".to_owned() }
    }

    pub fn start_typed_thread(&mut self, title: &str) {
        let Some(job_id) = self.selected_job else { return; };
        let chat_id = self.next_chat_id;
        self.next_chat_id += 1;

        let thread = ChatThread {
            id: chat_id,
            title: title.to_owned(),
            messages: vec![ChatMsg::ai_streaming()],
        };

        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.chats.push(thread);
        }
        self.selected_chat = Some(chat_id);

        // Kickoff prompt — Mock Interview gets a directive that tells the
        // interviewer to BEGIN, since the system prompt does all the heavy
        // lifting. Other typed threads get a softer kickoff.
        let prompt = match title {
            "Mock Interview" => {
                "Begin the interview now. Open with a brief greeting and your \
                 first question — ground it in something specific from my resume \
                 or the JD. Single question, no preamble.".to_owned()
            }
            "Interview Questions" => {
                "Generate the 8–12 most likely interview questions for this role \
                 at this company, ordered by stage (recruiter screen → technical \
                 → behavioral → final). For each, explain WHY they ask it and \
                 give a 2–3 sentence answer outline drawing on my resume.".to_owned()
            }
            other => format!(
                "Start a {other} session for me. Begin immediately with the first \
                 question or scenario, grounded in my resume and the JD."
            ),
        };
        let job_context = self.job_context_for(job_id, true);
        let mode = if title == "Mock Interview" { "interviewer" } else { "coach" };

        if let Some(url) = &self.backend_url {
            let (tx, rx) = mpsc::channel();
            crate::backend_client::stream_chat(
                url,
                prompt,
                job_context,
                Vec::new(), // first turn — no history yet
                mode,
                self.gemini_api_key.clone(),
                tx,
            );
            self.chat_rx = Some(rx);
            self.streaming_job_id = Some(job_id);
            self.streaming_chat_id = Some(chat_id);
        } else {
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                if let Some(thread) = job.chats.iter_mut().find(|c| c.id == chat_id) {
                    if let Some(msg) = thread.messages.last_mut() {
                        msg.text = "Backend not ready. Please wait for startup to complete.".to_owned();
                        msg.streaming = false;
                    }
                }
            }
        }
    }

    pub fn create_job(&mut self) {
        let company = self.nj_company.trim().to_owned();
        let role    = self.nj_role.trim().to_owned();
        if company.is_empty() || role.is_empty() {
            self.nj_error = Some("Company and role are required.".to_owned());
            return;
        }
        let palette = [C_INDIGO, C_OFFER, C_APPLIED, C_REJECTED, MAGENTA];
        let av  = palette[self.next_job_id % palette.len()];
        let cid = self.next_chat_id;
        self.next_chat_id += 1;
        let job = Job {
            id: self.next_job_id,
            company,
            role,
            location: self.nj_location.trim().to_owned(),
            job_description: self.nj_job_description.trim().to_owned(),
            research_status: if self.nj_job_description.trim().is_empty() {
                "No JD added".to_owned()
            } else {
                "Queued".to_owned()
            },
            status: self.nj_status,
            current_stage: 0,
            stage_notes: vec![StageNote::new(0, "Submitted", &self.nj_notes)],
            chats: vec![ChatThread {
                id: cid,
                title: "General Prep".to_owned(),
                messages: vec![ChatMsg::ai(
                    "Ready to help you prepare. Share the job description or ask me anything.",
                )],
            }],
            avatar_color: av,
            sidebar_expanded: true,
            archived: false,
            tailored_resume:  None,
            resume_docx_path: None,
            scorecard:        None,
        };
        self.next_job_id += 1;
        let jid = job.id;
        self.jobs.push(job);
        self.selected_job  = Some(jid);
        self.selected_chat = Some(cid);
        self.screen = Screen::Chat;
        // Persist immediately so the new job survives a crash before
        // the user closes the app or settings modal.
        self.save_jobs();
        // Flow: tailor a resume + cover letter FIRST, then company research.
        // Application-Prep produces the tailored resume text that
        // start_company_research consumes (passed via Job.tailored_resume).
        self.start_application_prep(jid);
        self.show_new_job = false;
        self.nj_company.clear();
        self.nj_role.clear();
        self.nj_location.clear();
        self.nj_job_description.clear();
        self.nj_notes.clear();
        self.nj_error = None;
        self.nj_resume_id = None;
    }

    /// Step 1 of the on-create flow. Sends every master resume + the JD to
    /// the backend, which uses Gemini 2.5 Pro to pick the closest match and
    /// return a tailored `.docx` + cover letter + scorecard.
    /// When this finishes, `poll_backend` chain-starts `start_company_research`.
    pub fn start_application_prep(&mut self, job_id: usize) {
        let (company, role, location, job_description) = match self.jobs.iter().find(|j| j.id == job_id) {
            Some(j) => (j.company.clone(), j.role.clone(), j.location.clone(), j.job_description.clone()),
            None    => return,
        };

        // Snapshot master resumes as (name, text) pairs. Honour the per-job
        // picker — if the user chose a specific resume in the New Job modal,
        // send only that one (the LLM doesn't get to override).
        let master_resumes: Vec<(String, String)> = self
            .resume_store
            .items()
            .iter()
            .filter(|r| match self.nj_resume_id {
                Some(forced_id) => r.id == forced_id,
                None            => true,
            })
            .map(|r| (r.name.clone(), r.text.clone()))
            .collect();

        let thread_id = self.next_chat_id;
        self.next_chat_id += 1;
        let thread = ChatThread {
            id: thread_id,
            title: "Application Prep".to_owned(),
            messages: vec![ChatMsg::ai_streaming()],
        };
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.chats.push(thread);
            job.research_status = "Tailoring resume…".to_owned();
        }
        self.selected_chat = Some(thread_id);

        if master_resumes.is_empty() {
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                job.research_status = "No resume on file".to_owned();
                if let Some(t) = job.chats.iter_mut().find(|c| c.id == thread_id) {
                    if let Some(m) = t.messages.last_mut() {
                        m.text = "**Error:** add a resume in Settings → Resume before creating a job.".to_owned();
                        m.streaming = false;
                    }
                }
            }
            return;
        }

        if let Some(url) = &self.backend_url {
            let (tx, rx) = mpsc::channel();
            crate::backend_client::stream_application_tailor(
                url,
                company,
                role,
                location,
                job_description,
                master_resumes,
                self.gemini_api_key.clone(),
                tx,
            );
            self.chat_rx = Some(rx);
            self.streaming_job_id  = Some(job_id);
            self.streaming_chat_id = Some(thread_id);
        } else if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.research_status = "Backend not ready".to_owned();
            if let Some(t) = job.chats.iter_mut().find(|c| c.id == thread_id) {
                if let Some(m) = t.messages.last_mut() {
                    m.text = "Backend is not ready yet. Try again in a moment.".to_owned();
                    m.streaming = false;
                }
            }
        }
    }

    /// Simulates a recruiter knockout-screen for a job using the tailored
    /// resume produced by `start_application_prep`. Streams the questions +
    /// suggested answers into a new "Knockout Screen" thread.
    pub fn start_knockout_screen(&mut self, job_id: usize) {
        let job = match self.jobs.iter().find(|j| j.id == job_id) {
            Some(j) => j,
            None => return,
        };

        let resume = job.tailored_resume.clone().unwrap_or_default();
        if resume.trim().is_empty() {
            // Nudge the user to run Application Prep first instead of silently failing
            if let Some(job_mut) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                if let Some(t) = job_mut.chats.iter_mut().find(|c| c.title == "Application Prep") {
                    t.messages.push(ChatMsg::ai(
                        "**Knockout screen unavailable.** Run Application Prep first — the screen \
                         needs the tailored resume as input.",
                    ));
                }
            }
            return;
        }

        let company         = job.company.clone();
        let role            = job.role.clone();
        let location        = job.location.clone();
        let job_description = job.job_description.clone();

        let thread_id = self.next_chat_id;
        self.next_chat_id += 1;
        let thread = ChatThread {
            id: thread_id,
            title: "Knockout Screen".to_owned(),
            messages: vec![ChatMsg::ai_streaming()],
        };
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.chats.push(thread);
        }
        self.selected_chat = Some(thread_id);

        if let Some(url) = &self.backend_url {
            let (tx, rx) = mpsc::channel();
            crate::backend_client::stream_knockout_screen(
                url,
                company,
                role,
                location,
                job_description,
                resume,
                self.gemini_api_key.clone(),
                tx,
            );
            self.chat_rx = Some(rx);
            self.streaming_job_id  = Some(job_id);
            self.streaming_chat_id = Some(thread_id);
        } else if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            if let Some(t) = job.chats.iter_mut().find(|c| c.id == thread_id) {
                if let Some(m) = t.messages.last_mut() {
                    m.text = "Backend is not ready yet. Try again in a moment.".to_owned();
                    m.streaming = false;
                }
            }
        }
    }

    pub fn start_company_research(&mut self, job_id: usize) {
        let job = match self.jobs.iter().find(|j| j.id == job_id) {
            Some(j) => j,
            None => return,
        };
        let company         = job.company.clone();
        let role            = job.role.clone();
        let location        = job.location.clone();
        let job_description = job.job_description.clone();
        let tailored_resume = job.tailored_resume.clone().unwrap_or_default();

        // Create a dedicated "Company Research" thread
        let thread_id = self.next_chat_id;
        self.next_chat_id += 1;
        let thread = crate::gui::types::ChatThread {
            id: thread_id,
            title: "Company Research".to_owned(),
            messages: vec![crate::gui::types::ChatMsg::ai_streaming()],
        };
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
            job.chats.push(thread);
            job.research_status = "Researching…".to_owned();
        }
        // Intentionally NOT switching `selected_chat`. Company Research is
        // chain-started after Application Prep finishes, and yanking the user
        // away from the cover-letter + scorecard + Open-Resume buttons they
        // just got is jarring. The new thread is reachable via the sidebar.

        if let Some(url) = &self.backend_url {
            let (tx, rx) = mpsc::channel();
            crate::backend_client::stream_company_research(
                url,
                company,
                role,
                location,
                job_description,
                tailored_resume,
                self.gemini_api_key.clone(),
                self.glassdoor_email.clone(),
                self.glassdoor_password.clone(),
                self.indeed_email.clone(),
                self.indeed_password.clone(),
                tx,
            );
            self.chat_rx = Some(rx);
            self.streaming_job_id  = Some(job_id);
            self.streaming_chat_id = Some(thread_id);
        } else {
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                job.research_status = "Backend not ready".to_owned();
                if let Some(t) = job.chats.iter_mut().find(|c| c.id == thread_id) {
                    if let Some(m) = t.messages.last_mut() {
                        m.text = "Backend is not ready yet. Try again in a moment.".to_owned();
                        m.streaming = false;
                    }
                }
            }
        }
    }

    pub fn start_research_for_job(&mut self, job_id: usize) {
        let job = match self.jobs.iter_mut().find(|j| j.id == job_id) {
            Some(j) => j,
            None => return,
        };
        let jd = job.job_description.trim().to_owned();
        if jd.len() < 30 {
            job.research_status = "Add a fuller JD first".to_owned();
            return;
        }
        job.research_status = "Analyzing…".to_owned();
        let company = job.company.clone();
        let role    = job.role.clone();

        // Find or create a "Research" thread
        let thread_id = if let Some(t) = job.chats.first_mut() {
            t.messages.push(ChatMsg::ai_streaming());
            t.id
        } else {
            let tid = self.next_chat_id;
            self.next_chat_id += 1;
            self.jobs.iter_mut().find(|j| j.id == job_id).unwrap().chats.push(ChatThread {
                id: tid,
                title: "Research".to_owned(),
                messages: vec![ChatMsg::ai_streaming()],
            });
            tid
        };

        if let Some(url) = &self.backend_url {
            let (tx, rx) = mpsc::channel();
            crate::backend_client::stream_research(
                url,
                company,
                role,
                jd,
                self.gemini_api_key.clone(),
                tx,
            );
            self.chat_rx = Some(rx);
            self.streaming_job_id = Some(job_id);
            self.streaming_chat_id = Some(thread_id);

            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                job.research_status = "Analyzing…".to_owned();
            }
        } else {
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) {
                job.research_status = "Backend not ready".to_owned();
                if let Some(thread) = job.chats.iter_mut().find(|c| c.id == thread_id) {
                    if let Some(msg) = thread.messages.last_mut() {
                        if msg.streaming {
                            msg.text = "Backend not ready. Wait for startup or check Python installation.".to_owned();
                            msg.streaming = false;
                        }
                    }
                }
            }
        }
    }
}


// ─── eframe::App ─────────────────────────────────────────────────────────────

impl eframe::App for InterPrepApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_backend(ui.ctx());

        ui.painter().rect_filled(ui.max_rect(), 0.0, BG);

        let sidebar_w = if self.sidebar_open { 280.0 } else { 64.0 };

        egui::Panel::left("sidebar")
            .exact_size(sidebar_w)
            .resizable(false)
            .frame(egui::Frame::new().fill(BG).stroke(Stroke::new(1.0, BD)))
            .show_inside(ui, |ui| { self.draw_sidebar(ui); });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show_inside(ui, |ui| {
                match self.screen {
                    Screen::Chat     => self.draw_chat_workspace(ui),
                    Screen::Timeline => self.draw_timeline_workspace(ui),
                }
            });

        if self.show_new_job    { self.draw_new_job_modal(ui.ctx()); }
        if self.show_settings   { self.draw_settings_modal(ui.ctx()); }
        if self.show_report_modal { self.draw_report_modal(ui.ctx()); }

        // Autosave credentials when the settings modal transitions
        // open → closed (X click or backdrop click).
        if self.settings_open_prev && !self.show_settings {
            self.save_credentials();
        }
        self.settings_open_prev = self.show_settings;
    }

    /// Final flush on app exit — covers the case where the user just closes
    /// the window without ever opening the settings modal in this session.
    fn on_exit(&mut self) {
        self.save_credentials();
        self.save_jobs();
    }
}
