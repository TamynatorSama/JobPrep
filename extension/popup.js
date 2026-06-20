// InterPrep Autofill — popup.
//
// Views: pairing -> jobs (hub) -> capture | actions(scan preview) ->
// progress(stepper) | done(summary), plus settings. Single-page "Scan & fill"
// runs here; multi-page hands off to the service worker (background.js), which
// survives navigation and drives the loop. Injected page functions
// (ipScrape/ipFill/ipScrapeJD/ipScrapeSteps/…) live in inject.js.

// Promise-style API namespace that works on both Chrome (chrome.* is already
// promisified in MV3) and Firefox (browser.*).
const browser = globalThis.browser || globalThis.chrome;

const $ = (id) => document.getElementById(id);
const show = (el, on) => el && el.classList.toggle("hidden", !on);
const LOWCONF = 0.7;

let pairing = null;
let lastFields = [];        // last scrape (single-page preview)
let lastAnswers = [];       // last /answer-fields result
let lastTabId = null;
let jobsById = {};
let activeJobId = "";        // job chosen for autofill ("" = master resume only)
let reviewMode = "single";   // "single" -> fill locally; "auto" -> hand back to SW
let currentView = "pairing";
let prevView = "jobs";
let multiPage = false;       // current page spans multiple steps
let awaitingStepPopup = false;
let lastSummary = null;
let singleNeeds = [];        // review items for the single-page path
let singleFilled = 0;

// Mirror of App.tsx STATUS_CONFIG so the picker shows the same badge.
const STATUS = {
  Applied:   { label: "Applied",   color: "#F59E0B", bg: "rgba(245,158,11,0.14)" },
  Screening: { label: "Screening", color: "#0099FF", bg: "rgba(0,153,255,0.14)" },
  Technical: { label: "Technical", color: "#a855f7", bg: "rgba(168,85,247,0.14)" },
  Offer:     { label: "Offer",     color: "#22c55e", bg: "rgba(34,197,94,0.14)" },
  Rejected:  { label: "Rejected",  color: "#EF4444", bg: "rgba(239,68,68,0.14)" },
};
const AV_COLORS = ["#6f4cff", "#0099ff", "#22c55e", "#F59E0B", "#EF4444", "#a855f7", "#06b6d4", "#ec4899"];
const initial = (s) => ((s || "?").trim().charAt(0).toUpperCase() || "?");
function avatarColor(s) { let h = 0; for (const c of String(s || "")) h = (h * 31 + c.charCodeAt(0)) >>> 0; return AV_COLORS[h % AV_COLORS.length]; }
function setAvatar(el, name, avatar, color) {
  if (!el) return;
  el.textContent = (avatar && avatar.trim()) || initial(name);
  el.style.background = color || avatarColor(name);
}
const ICON_CHECK = `<svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>`;
const ICON_X = `<svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6L6 18M6 6l12 12"/></svg>`;

// ── storage / pairing (promise-style; Firefox browser.* ignores callbacks) ────

function loadPairing() { return browser.storage.local.get("pairing").then((d) => (d && d.pairing) || null); }
function savePairing(p) { return browser.storage.local.set({ pairing: p }); }
function clearPairing() { return browser.storage.local.remove("pairing"); }

function decodePairingCode(code) {
  let decoded;
  try { decoded = atob(code.trim()); } catch { throw new Error("not a valid pairing code"); }
  const i = decoded.indexOf(":");
  if (i < 0) throw new Error("malformed pairing code");
  const port = parseInt(decoded.slice(0, i), 10);
  const token = decoded.slice(i + 1);
  if (!port || !token) throw new Error("malformed pairing code");
  return { port, token };
}

// ── backend ──────────────────────────────────────────────────────────────────

function api(path, opts = {}) {
  const headers = Object.assign({ "X-InterPrep-Token": pairing.token }, opts.headers || {});
  if (opts.body) headers["Content-Type"] = "application/json";
  return fetch(`http://127.0.0.1:${pairing.port}${path}`, { ...opts, headers });
}
async function ping() { try { return (await api("/config/ping")).ok; } catch { return false; } }

// ── view router ────────────────────────────────────────────────────────────

function showView(name) {
  currentView = name;
  for (const v of document.querySelectorAll(".view")) show(v, false);
  show($(`view-${name}`), true);

  const paired = name !== "pairing";
  show($("gear-btn"), paired && ["jobs", "actions", "progress", "capture"].includes(name));
  show($("back-btn"), ["actions", "progress", "capture", "settings", "done"].includes(name));

  const pill = $("ctx-pill");
  if (name === "capture") { pill.innerHTML = `<span class="dot"></span>Posting`; pill.className = "pill pill-green"; show(pill, true); }
  else if (name === "actions") { pill.textContent = "Form"; pill.className = "pill pill-blue"; show(pill, true); }
  else if (name === "progress") { pill.textContent = multiPage ? "Multi-page" : "Single page"; pill.className = "pill pill-purple"; show(pill, true); }
  else show(pill, false);

  const runView = name === "actions" || name === "progress";
  if (!runView) { show($("questions"), false); show($("log"), false); }
}

// ── injected helpers (page context) ──────────────────────────────────────────

async function activeTab() {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  return tab;
}

async function execBest(tabId, func, args) {
  const opts = args ? { func, args } : { func };
  try { return (await browser.scripting.executeScript({ target: { tabId, allFrames: true }, ...opts })).map((r) => r && r.result).filter(Boolean); }
  catch { return (await browser.scripting.executeScript({ target: { tabId }, ...opts })).map((r) => r && r.result).filter(Boolean); }
}

// Click "Add education / Add work experience / …" controls across all frames so
// their collapsed fields exist before we scrape. Best-effort; failures are fine.
async function expandFrames(tabId) {
  try { await browser.scripting.executeScript({ target: { tabId, allFrames: true }, func: ipExpandSections }); }
  catch { try { await browser.scripting.executeScript({ target: { tabId }, func: ipExpandSections }); } catch { /* ignore */ } }
}

async function scrapeAllFrames(tabId) {
  let res;
  try { res = await browser.scripting.executeScript({ target: { tabId, allFrames: true }, func: ipScrape }); }
  catch { res = await browser.scripting.executeScript({ target: { tabId }, func: ipScrape }); }
  const fields = [];
  for (const r of res || []) {
    if (!r || !r.result) continue;
    for (const f of r.result) fields.push({ ...f, frameId: r.frameId, localKey: f.key, key: `${r.frameId}:${f.key}` });
  }
  return fields;
}

async function fillFrames(tabId, answers) {
  const byFrame = {};
  for (const a of answers) {
    if (a.needs_user || a.value === "" || a.value == null) continue;
    const f = lastFields.find((x) => x.key === a.key);
    if (!f) continue;
    (byFrame[f.frameId] ||= []).push({ key: f.localKey, value: a.value, type: f.type, radioName: f.radioName });
  }
  let filled = 0;
  for (const fid of Object.keys(byFrame)) {
    const r = await browser.scripting.executeScript({ target: { tabId, frameIds: [parseInt(fid, 10)] }, func: ipFill, args: [byFrame[fid]] });
    filled += (r && r[0] && r[0].result) || 0;
  }
  return filled;
}

async function attachResume(tabId, jobId) {
  let r;
  try { r = await api(`/store/resume-file/${jobId}`); } catch { return { ok: false }; }
  if (!r.ok) return { ok: false, status: r.status };
  const { b64, filename, mime } = await r.json();
  const res = await execBest(tabId, ipAttach, [b64, filename, mime]);
  const count = res.reduce((s, x) => s + (x || 0), 0);
  return { ok: count > 0, count };
}

async function hasNextButton(tabId) {
  try { const res = await execBest(tabId, ipFindNav); return res.some((r) => r && r.next); } catch { return false; }
}
async function scrapeStepsPopup(tabId) {
  try {
    const res = await execBest(tabId, ipScrapeSteps);
    let best = null;
    for (const s of res) if (s && s.steps && (!best || s.steps.length > best.steps.length)) best = s;
    return best;
  } catch { return null; }
}
async function focusFirstBlank(tabId) {
  try { const res = await execBest(tabId, ipFocusFirstBlank); for (const r of res) if (r) return r; return ""; } catch { return ""; }
}

function labelOf(key) { return (lastFields.find((f) => f.key === key) || {}).label || key; }
function shortUrl(u) { try { const x = new URL(u); return (x.host + x.pathname).replace(/\/$/, ""); } catch { return u || ""; } }
const isReady = (a) => a && a.value && !a.needs_user && (a.source !== "model" || (a.confidence ?? 1) >= LOWCONF);
function timeSaved(filled, pages) {
  const secs = (filled || 0) * 12 + (pages || 0) * 20;
  const m = Math.round(secs / 60);
  return m < 1 ? "<1m" : `~${m}m`;
}

// ── scan preview (img3) ───────────────────────────────────────────────────

function setProfileCard() {
  const j = jobsById[activeJobId];
  const name = (j && j.candidateName) || "";
  setAvatar($("prof-avatar"), name || "You", "", "#6f4cff");
  $("prof-name").textContent = name || "Your profile";
  $("prof-sub").textContent = (j && j.hasTailoredResume) ? "CV attached" : "Master resume";
}
function setJobCard() {
  const j = jobsById[activeJobId];
  if (j) { setAvatar($("job-avatar"), j.company, j.avatar, j.avatarColor); $("job-name").textContent = j.company || "Target job"; }
  else { setAvatar($("job-avatar"), "", "∅", "#2a2e37"); $("job-name").textContent = "No specific job"; }
}

function mappedRowEl(row) {
  const d = document.createElement("div");
  d.className = "mapped-row";
  const icon = document.createElement("span");
  icon.className = "mrow-icon " + (row.ready ? "ok" : "skip");
  icon.innerHTML = row.ready ? ICON_CHECK : ICON_X;
  const label = document.createElement("span");
  label.className = "mrow-label";
  label.textContent = row.label;
  const val = document.createElement("span");
  val.className = "mrow-value" + (row.ready ? "" : " skip");
  val.textContent = row.value;
  d.append(icon, label, val);
  return d;
}

async function previewFill() {
  reviewMode = "single";
  show($("questions"), false);
  const tab = await activeTab();
  lastTabId = tab.id;
  $("act-url").textContent = shortUrl(tab.url);
  setProfileCard(); setJobCard();
  $("form-kind").textContent = "scanning…"; $("form-kind").className = "pill pill-muted";
  $("mapped").innerHTML = ""; $("ready-count").textContent = ""; $("ctx-note").textContent = "";
  $("autofill-btn").disabled = true; $("autofill-label").textContent = "Autofill";
  $("run-status").textContent = "Scanning page…";

  await expandFrames(tab.id); // reveal collapsed Education / Experience / … sections
  lastFields = await scrapeAllFrames(tab.id);
  if (!lastFields.length) {
    $("run-status").textContent = "No fillable fields found — open the form, then reopen the popup.";
    $("form-kind").textContent = "no form"; $("form-kind").className = "pill pill-muted";
    return;
  }
  $("run-status").textContent = "";
  multiPage = await hasNextButton(tab.id);
  show($("ctx-pill"), true);
  $("ctx-pill").className = "pill pill-blue"; $("ctx-pill").textContent = "Form";
  $("form-kind").textContent = multiPage ? "Multi-page" : "Single page";
  $("form-kind").className = "pill " + (multiPage ? "pill-purple" : "pill-muted");

  $("run-status").textContent = `Found ${lastFields.length} fields. Asking InterPrep…`;
  let answers = [];
  try {
    const resp = await api("/autofill/answer-fields", {
      method: "POST",
      body: JSON.stringify({
        job_id: activeJobId || "", resume_ids: selectedResumeIds(),
        fields: lastFields.map((f) => ({ key: f.key, label: f.label, type: f.type, options: f.options || [] })),
      }),
    });
    if (!resp.ok) { $("run-status").textContent = `Backend error (HTTP ${resp.status}).`; return; }
    answers = (await resp.json()).answers;
  } catch { $("run-status").textContent = "Backend unreachable — is InterPrep open?"; return; }
  lastAnswers = answers;
  $("run-status").textContent = "";
  renderMapped(answers);
}

function renderMapped(answers) {
  const box = $("mapped");
  box.innerHTML = "";
  const j = jobsById[activeJobId];
  const hasResume = !!(j && j.hasTailoredResume);
  let ready = 0;

  if (hasResume) { ready++; box.appendChild(mappedRowEl({ label: "Resume", value: "📄 Tailored résumé (PDF)", ready: true })); }
  for (const a of answers) {
    const r = isReady(a);
    if (r) ready++;
    box.appendChild(mappedRowEl({ label: labelOf(a.key), value: r ? a.value : "Skipped — needs you", ready: r }));
  }

  const total = lastFields.length + (hasResume ? 1 : 0);
  $("ready-count").textContent = `${ready} of ${total} ready`;
  $("ready-count").className = "ready ok";
  $("ctx-note").textContent = j
    ? `Using ${j.hasTailoredResume ? "tailored resume" : "master resume"}${j.hasResearch ? " + company research" : ""}.`
    : "Using the checked master resumes only (set them in ⚙ Settings).";

  $("autofill-btn").disabled = false;
  $("autofill-label").textContent = `Autofill ${ready} field${ready !== 1 ? "s" : ""}`;
}

async function onAutofill() {
  if (multiPage) { showView("progress"); await enterProgressIdle(); return; }
  await doSingleFill();
}

async function doSingleFill() {
  $("autofill-btn").disabled = true;
  $("run-status").textContent = "Filling…";
  try {
    const tab = await activeTab();
    const ready = lastAnswers.filter(isReady);
    let filled = await fillFrames(tab.id, ready);
    const j = jobsById[activeJobId];
    if (activeJobId && j?.hasTailoredResume) { const at = await attachResume(tab.id, activeJobId); if (at.ok) filled++; }

    singleFilled = filled;
    singleNeeds = lastAnswers.filter((a) => !isReady(a)).map((a) => ({
      key: a.key, label: labelOf(a.key),
      value: (!a.needs_user && a.value) || "", reason: a.reason || "", guess: !a.needs_user && !!a.value,
    }));

    if (singleNeeds.length) {
      reviewMode = "single";
      $("run-status").textContent = `Filled ${filled} field(s). ${singleNeeds.length} to review.`;
      renderReview(singleNeeds);
    } else {
      showDone({ filled, pages: 1, needs: [], jobId: activeJobId });
    }
  } catch (e) {
    $("run-status").textContent = "Error: " + e.message;
    $("autofill-btn").disabled = false;
  }
}

// ── multi-page stepper (img4) ───────────────────────────────────────────────

function stepRowEl(num, label, state) {
  const d = document.createElement("div");
  d.className = "step-row " + state;
  const n = document.createElement("span");
  n.className = "step-num";
  n.innerHTML = state === "done" ? ICON_CHECK : String(num);
  const l = document.createElement("span");
  l.className = "step-label";
  l.textContent = label;
  const s = document.createElement("span");
  s.className = "step-state";
  s.textContent = state === "done" ? "Done" : state === "current" ? "Filling" : "";
  d.append(n, l, s);
  return d;
}

function setProgressBar(frac) { $("progress-bar").style.width = `${Math.max(0, Math.min(1, frac)) * 100}%`; }

function renderSteps(info, page) {
  const box = $("steps");
  box.innerHTML = "";
  if (info && info.steps && info.steps.length) {
    $("prog-step").textContent = info.current >= 0 ? `Step ${info.current + 1} of ${info.total}` : `${info.total} steps`;
    const done = info.steps.filter((s) => s.state === "done").length;
    const cur = info.steps.some((s) => s.state === "current") ? 0.5 : 0;
    setProgressBar((done + cur) / info.total);
    info.steps.forEach((s, i) => box.appendChild(stepRowEl(i + 1, s.label, s.state)));
  } else {
    $("prog-step").textContent = page ? `Page ${page}` : "";
    setProgressBar(page ? 0.3 : 0);
    box.appendChild(stepRowEl(page || 1, "Filling this page", "current"));
  }
}

async function enterProgressIdle() {
  const j = jobsById[activeJobId];
  $("prog-title").textContent = j ? `${j.company} application` : "Application";
  $("prog-status").textContent = "Ready — fill one step, or all at once.";
  $("log").innerHTML = "";
  setProgRunning(false);
  renderSteps(await scrapeStepsPopup(lastTabId), 1);
}

function setProgRunning(on) {
  show($("stop-btn"), on);
  $("all-btn").disabled = on;
  $("step-btn").disabled = on;          // re-enabled on stepPaused
  if (on) show($("log"), true);
}

async function startAuto(stepMode) {
  reviewMode = "auto";
  awaitingStepPopup = false;
  lastSummary = null;
  show($("questions"), false);
  $("log").innerHTML = "";
  setProgRunning(true);
  const tab = await activeTab();
  lastTabId = tab.id;
  appendLog(stepMode ? "Filling this step…" : "Auto-filling all steps…");
  $("prog-status").textContent = stepMode ? "Filling this step…" : "Auto-filling all steps…";
  browser.runtime.sendMessage({
    type: "startAuto", tabId: tab.id, jobId: activeJobId || "",
    resumeIds: selectedResumeIds(), stepMode: !!stepMode,
  }).catch((e) => appendLog("Could not start: " + e.message));
}

function onStepBtn() {
  if (awaitingStepPopup) {
    browser.runtime.sendMessage({ type: "nextStep" }).catch(() => {});
    awaitingStepPopup = false;
    $("step-btn").disabled = true;
    $("prog-status").textContent = "Continuing…";
  } else {
    startAuto(true);
  }
}

function stopAuto() {
  browser.runtime.sendMessage({ type: "stop" }).catch(() => {});
  setProgRunning(false);
  appendLog("Stop requested.");
}

function appendLog(text) {
  if (!text) return;
  show($("log"), true);
  const d = document.createElement("div");
  d.textContent = text;
  $("log").prepend(d);
}

// Messages pushed by the service worker.
browser.runtime.onMessage.addListener((m) => {
  if (m.type === "progress") { appendLog(m.text); $("prog-status").textContent = m.text; }
  else if (m.type === "step") { renderSteps(m.steps, m.page); }
  else if (m.type === "needs") {
    reviewMode = "auto";
    appendLog(`Page ${m.page}: ${m.items.length} field(s) need you.`);
    $("prog-status").textContent = `Page ${m.page}: review ${m.items.length} field(s), then continue.`;
    renderReview(m.items);
    show($("stop-btn"), true);
  } else if (m.type === "stepPaused") {
    awaitingStepPopup = true;
    renderSteps(m.steps, m.page);
    $("prog-status").textContent = `Step filled (${m.filled} fields so far) — continue when ready.`;
    $("step-btn").disabled = false;
    show($("stop-btn"), true);
  } else if (m.type === "summary") {
    lastSummary = { filled: m.filled, pages: m.pages, needs: m.needs || [], jobId: m.jobId };
    showDone(lastSummary);
  } else if (m.type === "done" || m.type === "stopped" || m.type === "error") {
    appendLog(m.text || m.type);
    $("prog-status").textContent = m.text || m.type;
    setProgRunning(false);
  }
});

// ── done summary (img5) ─────────────────────────────────────────────────────

function showDone(s) {
  const j = jobsById[s.jobId] || jobsById[activeJobId];
  showView("done");
  $("done-sub").textContent = j
    ? `${j.company}${j.role ? " · " + j.role : ""}. Review everything before you submit.`
    : "Review everything before you submit.";
  $("stat-fields").textContent = s.filled || 0;
  $("stat-pages").textContent = s.pages || 0;
  $("stat-time").textContent = timeSaved(s.filled, s.pages);

  const needs = s.needs || [];
  if (needs.length) {
    show($("eyes-card"), true);
    $("eyes-title").textContent = `${needs.length} field${needs.length !== 1 ? "s" : ""} need your eyes`;
    $("eyes-body").textContent = needs.slice(0, 4).map((n) => n.label).join(", ") + " — left blank for you to review.";
    $("jump-btn").disabled = false;
  } else {
    show($("eyes-card"), false);
    $("jump-btn").disabled = true;
  }

  const jobId = s.jobId || activeJobId;
  $("timeline-label").textContent = j ? `Log to ${j.company} timeline` : "Log to timeline";
  $("timeline-btn").disabled = !jobId;
  $("done-status").textContent = "";
}

async function onJumpBlank() {
  try {
    const tab = await activeTab();
    const label = await focusFirstBlank(tab.id);
    $("done-status").textContent = label ? `Jumped to “${label}”.` : "No blank field found on this page.";
  } catch (e) { $("done-status").textContent = "Couldn't jump: " + e.message; }
}

async function onLogTimeline() {
  const jobId = (lastSummary && lastSummary.jobId) || activeJobId;
  if (!jobId) { $("done-status").textContent = "Pick a job first to log to its timeline."; return; }
  $("done-status").textContent = "Logging…";
  try {
    const r = await api("/inbox/timeline", {
      method: "POST",
      body: JSON.stringify({ job_id: jobId, status: "Applied", note: "Autofilled via the browser extension." }),
    });
    if (!r.ok) { $("done-status").textContent = `Log failed (HTTP ${r.status}).`; return; }
    $("done-status").textContent = "Logged ✓ — InterPrep will record it on this job.";
    $("timeline-btn").disabled = true;
  } catch (e) { $("done-status").textContent = "Log error: " + e.message; }
}

// ── review prompts (shared by single + auto) ─────────────────────────────────

function renderReview(items) {
  const list = $("q-list");
  list.innerHTML = "";
  if (!items.length) { show($("questions"), false); return; }
  for (const it of items) {
    const wrap = document.createElement("div");
    wrap.className = "q";
    const label = document.createElement("div");
    label.className = "q-label";
    label.textContent = it.label || it.key;
    const reason = document.createElement("div");
    reason.className = "q-reason";
    reason.textContent = (it.guess ? "Guess — confirm or edit. " : "") + (it.reason || "Needs your answer");
    const input = document.createElement("input");
    input.type = "text";
    input.dataset.key = it.key;
    input.value = it.value || "";
    input.placeholder = "your answer";
    wrap.append(label, reason, input);
    list.appendChild(wrap);
  }
  show($("questions"), true);
  $("apply-btn").textContent = reviewMode === "auto" ? "Apply & continue" : "Apply & finish";
}

async function applyAnswers() {
  const inputs = [...document.querySelectorAll("#q-list input[data-key]")];
  const answers = inputs.filter((i) => i.value.trim()).map((i) => ({ key: i.dataset.key, value: i.value.trim(), needs_user: false }));

  if (reviewMode === "auto") {
    browser.runtime.sendMessage({ type: "resumeAnswers", answers }).catch(() => {});
    show($("questions"), false);
    $("prog-status").textContent = "Continuing…";
    return;
  }

  // single-page path → fill, remember, then show the done summary.
  try {
    if (lastTabId != null && answers.length) await fillFrames(lastTabId, answers);
    await Promise.all(answers.map((a) =>
      api("/autofill/remember", { method: "POST", body: JSON.stringify({ label: labelOf(a.key), value: a.value }) }).catch(() => {})
    ));
    const answeredKeys = new Set(answers.map((a) => a.key));
    const blank = singleNeeds.filter((n) => !answeredKeys.has(n.key)).map((n) => ({ label: n.label, reason: n.reason }));
    show($("questions"), false);
    showDone({ filled: (singleFilled || 0) + answers.length, pages: 1, needs: blank, jobId: activeJobId });
  } catch (e) {
    $("run-status").textContent = "Error applying: " + e.message;
  }
}

// ── capture a JD → InterPrep inbox ───────────────────────────────────────────

let lastCapture = null;
let pageCache = null;

function postedLabel(p) {
  if (!p) return "";
  if (/^\d{4}-\d{2}-\d{2}/.test(p)) {
    const days = Math.floor((Date.now() - new Date(p).getTime()) / 86400000);
    if (isNaN(days)) return "";
    if (days <= 0) return "Posted today";
    if (days === 1) return "Posted 1d ago";
    if (days < 30) return `Posted ${days}d ago`;
    return `Posted ${Math.floor(days / 30)}mo ago`;
  }
  return /posted/i.test(p) ? p : `Posted ${p}`;
}
function chipEl(text) { const s = document.createElement("span"); s.className = "chip"; s.textContent = text; return s; }

function fillCapture(obj, source) {
  const company = obj.company || "", role = obj.role || "", location = obj.location || "";
  $("cap-company").value = company; $("cap-role").value = role; $("cap-location").value = location;
  setAvatar($("cap-avatar"), company, "", "");
  $("cap-title").textContent = role || "Untitled role";
  $("cap-sub").textContent = [company, location].filter(Boolean).join(" · ");

  const chips = $("cap-chips"); chips.innerHTML = "";
  for (const c of [obj.employment_type, obj.work_mode, postedLabel(obj.posted), obj.salary]) if (c) chips.appendChild(chipEl(c));

  const reqs = Array.isArray(obj.requirements) ? obj.requirements.filter(Boolean) : [];
  const reqBox = $("cap-reqs"); reqBox.innerHTML = "";
  for (const r of reqs) reqBox.appendChild(chipEl(r));
  show($("cap-req-wrap"), reqs.length > 0);
  show($("cap-ai-badge"), source === "ai" && reqs.length > 0);

  lastCapture = {
    company, role, location, job_description: obj.job_description || "",
    url: obj.url || (pageCache && pageCache.url) || "",
    employment_type: obj.employment_type || "", work_mode: obj.work_mode || "",
    salary: obj.salary || "", posted: obj.posted || "", requirements: reqs,
  };
  $("cap-jd").textContent = `${source === "ai" ? "Analyzed with AI." : "Captured from page."} JD ${lastCapture.job_description.length.toLocaleString()} chars.`;
}

async function captureJD() {
  const st = $("capture-status");
  st.textContent = "Reading this page…";
  lastCapture = null; $("cap-chips").innerHTML = ""; show($("cap-req-wrap"), false);
  try {
    const tab = await activeTab();
    $("cap-url").textContent = shortUrl(tab.url);
    const texts = await execBest(tab.id, ipPageText);
    pageCache = texts.sort((a, b) => (b.text || "").length - (a.text || "").length)[0] || null;

    const cands = await execBest(tab.id, ipScrapeJD);
    const cap = cands.sort((a, b) => (b.job_description || "").length - (a.job_description || "").length)[0];
    const complete = cap && cap.company && cap.role && (cap.job_description || "").length > 200;

    if (complete) { fillCapture(cap, "page"); st.textContent = ""; }
    else { st.textContent = "Page unclear — analyzing with AI…"; await aiExtract(); }
  } catch (e) { st.textContent = "Capture error: " + e.message; }
}

async function aiExtract() {
  const st = $("capture-status");
  if (!pageCache || !pageCache.text) {
    try { const tab = await activeTab(); const t = await execBest(tab.id, ipPageText); pageCache = t.sort((a, b) => (b.text || "").length - (a.text || "").length)[0] || null; }
    catch { /* ignore */ }
  }
  if (!pageCache) { st.textContent = "Couldn't read the page."; return; }
  st.textContent = "Analyzing with AI…";
  try {
    const r = await api("/inbox/extract", { method: "POST", body: JSON.stringify({ url: pageCache.url, title: pageCache.title, page_text: pageCache.text }) });
    if (!r.ok) { const detail = (await r.json().catch(() => ({}))).detail || `HTTP ${r.status}`; st.textContent = "AI extraction failed: " + detail; return; }
    const d = await r.json(); d.url = pageCache.url;
    fillCapture(d, "ai"); st.textContent = "";
  } catch (e) { st.textContent = "AI error: " + e.message; }
}

let sendingJD = false;
async function sendJD() {
  if (sendingJD) return;                       // in-flight — ignore double-clicks
  if (!lastCapture) { $("capture-status").textContent = "Nothing captured yet."; return; }
  const st = $("capture-status");
  const btn = $("capture-btn");
  sendingJD = true; btn.disabled = true;
  st.textContent = "Saving…";
  try {
    const r = await api("/inbox/job", {
      method: "POST",
      body: JSON.stringify({
        company: $("cap-company").value.trim(), role: $("cap-role").value.trim(), location: $("cap-location").value.trim(),
        url: lastCapture.url, job_description: lastCapture.job_description,
        employment_type: lastCapture.employment_type, work_mode: lastCapture.work_mode,
        salary: lastCapture.salary, posted: lastCapture.posted, requirements: lastCapture.requirements,
      }),
    });
    if (!r.ok) { st.textContent = `Save failed (HTTP ${r.status}).`; return; }
    st.textContent = "Saved ✓ — InterPrep will create the job and start research.";
    lastCapture = null;
    setTimeout(async () => { await populateJobs(); showView("jobs"); }, 1200);
  } catch (e) { st.textContent = "Save error: " + e.message; }
  finally { sendingJD = false; btn.disabled = false; }
}

// ── job picker (hub) ─────────────────────────────────────────────────────────

function statusBadge(statusKey) {
  const s = STATUS[statusKey] || STATUS.Applied;
  const b = document.createElement("span");
  b.className = "status-badge"; b.textContent = s.label; b.style.color = s.color; b.style.background = s.bg;
  return b;
}

function jobCardEl(j) {
  const card = document.createElement("div");
  card.className = "job-card" + (j.id === activeJobId ? " selected" : "");
  const av = document.createElement("span");
  av.className = "avatar";
  setAvatar(av, j.company, j.avatar, j.avatarColor);
  const meta = document.createElement("div");
  meta.className = "job-meta";
  const company = document.createElement("div"); company.className = "job-company"; company.textContent = j.company || "Untitled";
  const role = document.createElement("div"); role.className = "job-role"; role.textContent = j.role || "";
  meta.append(company, role);
  const check = document.createElement("span");
  check.className = "check"; check.innerHTML = ICON_CHECK;
  card.append(av, meta, statusBadge(j.status), check);
  card.addEventListener("click", () => selectJob(j.id));
  return card;
}

async function populateJobs() {
  const list = $("job-list");
  list.innerHTML = "";
  jobsById = {};
  let jobs = [];
  try { jobs = await (await api("/store/jobs")).json(); } catch { /* offline — leave empty */ }
  const active = (jobs || []).filter((j) => !j.archived);
  if (!active.length) list.innerHTML = `<div class="empty">No tracked jobs yet — capture one from a job posting below.</div>`;
  for (const j of active) { jobsById[j.id] = j; list.appendChild(jobCardEl(j)); }

  const none = document.createElement("div");
  none.className = "job-card";
  none.innerHTML = `<span class="avatar" style="background:#2a2e37">∅</span>
    <div class="job-meta"><div class="job-company">No specific job</div>
    <div class="job-role">Fill with master resume only</div></div>`;
  none.addEventListener("click", () => selectJob(""));
  list.appendChild(none);
}

function selectJob(id) {
  activeJobId = id || "";
  showView("actions");
  previewFill();
}

// ── resumes (settings) ───────────────────────────────────────────────────────

async function populateResumes() {
  const box = $("resumes");
  box.innerHTML = "";
  try {
    const resumes = await (await api("/store/resumes")).json();
    if (!resumes.length) { box.innerHTML = `<span class="muted">No resumes saved.</span>`; return; }
    for (const r of resumes) {
      const lab = document.createElement("label");
      const cb = document.createElement("input");
      cb.type = "checkbox"; cb.value = r.id; cb.checked = false; cb.dataset.resume = "1";
      lab.appendChild(cb);
      lab.appendChild(document.createTextNode(r.name || `Resume ${r.id}`));
      box.appendChild(lab);
    }
  } catch { box.innerHTML = `<span class="muted">Could not load resumes.</span>`; }
}
function selectedResumeIds() { return [...document.querySelectorAll('#resumes input[data-resume]:checked')].map((c) => parseInt(c.value, 10)); }

// ── boot ─────────────────────────────────────────────────────────────────────

async function enterMain() {
  $("status").textContent = "Connected"; $("status").className = "pill pill-muted ok";
  await populateJobs();
  await populateResumes();
  // Reattach to an in-progress multi-page run if the popup was reopened.
  browser.runtime.sendMessage({ type: "status" }).then((s) => {
    if (!s || !s.running) { showView("jobs"); return; }
    reviewMode = "auto"; multiPage = true;
    lastTabId = s.tabId ?? lastTabId;
    showView("progress");
    $("prog-title").textContent = "Application";
    setProgRunning(true);
    renderSteps(null, s.page);
    if (s.log && s.log.length) { $("log").innerHTML = ""; for (const line of s.log) appendLog(line); }
    if (s.awaiting && s.needs && s.needs.length) {
      $("prog-status").textContent = `Page ${s.needsPage}: review ${s.needs.length} field(s), then continue.`;
      renderReview(s.needs);
      show($("stop-btn"), true);
    } else if (s.awaitingStep) {
      awaitingStepPopup = true;
      $("step-btn").disabled = false;
      $("prog-status").textContent = `Step filled (${s.filled} fields so far) — continue when ready.`;
      show($("stop-btn"), true);
    } else {
      appendLog("Auto-fill in progress…");
    }
  }).catch(() => showView("jobs"));
}

function enterPairing(msg) {
  $("status").textContent = msg || "not paired"; $("status").className = "pill pill-muted bad";
  showView("pairing");
}

async function init() {
  pairing = await loadPairing();
  if (!pairing) { enterPairing("not paired"); return; }
  if (await ping()) await enterMain();
  else enterPairing("not reachable");
}

// ── wiring ─────────────────────────────────────────────────────────────────

$("pair-btn").addEventListener("click", async () => {
  $("pair-err").textContent = "";
  try {
    pairing = decodePairingCode($("pairing-input").value);
    if (!(await ping())) throw new Error("paired, but backend not reachable — open InterPrep");
    await savePairing(pairing);
    await enterMain();
  } catch (e) { pairing = null; $("pair-err").textContent = e.message; }
});

$("gear-btn").addEventListener("click", () => { prevView = currentView; showView("settings"); });
$("back-btn").addEventListener("click", () => {
  if (currentView === "settings") showView(["settings", "pairing"].includes(prevView) ? "jobs" : prevView);
  else showView("jobs");
});
$("create-job-btn").addEventListener("click", () => { showView("capture"); captureJD(); });
$("ai-extract-btn").addEventListener("click", aiExtract);
$("capture-btn").addEventListener("click", sendJD);
$("autofill-btn").addEventListener("click", onAutofill);
$("step-btn").addEventListener("click", onStepBtn);
$("all-btn").addEventListener("click", () => startAuto(false));
$("stop-btn").addEventListener("click", stopAuto);
$("apply-btn").addEventListener("click", applyAnswers);
$("jump-btn").addEventListener("click", onJumpBlank);
$("timeline-btn").addEventListener("click", onLogTimeline);
$("unpair-btn").addEventListener("click", async () => { await clearPairing(); pairing = null; enterPairing("unpaired"); });

init();
