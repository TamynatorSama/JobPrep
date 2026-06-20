// InterPrep Autofill — service worker. Drives the multi-page auto-fill loop.
//
// Lives here (not the popup) because the popup closes on navigation/focus loss.
// Guardrails: never clicks Submit/Apply/Finish, pauses when a field needs the
// user, and caps the number of pages.

// Promise-style API namespace that works on both Chrome (chrome.* is already
// promisified in MV3) and Firefox (browser.*).
const browser = globalThis.browser || globalThis.chrome;

// Chrome runs this as a service worker and pulls in the shared page functions
// here. Firefox runs it as an event page and loads inject.js via manifest
// background.scripts instead, so importScripts won't exist — skip it there.
if (typeof importScripts === "function") importScripts("inject.js"); // ipScrape / ipExpandSections / ipFill / ipAttach / ipFindNav / ipClickNext / ipFingerprint

const MAX_PAGES = 12;
const LOWCONF = 0.7;

let ctx = null;          // { tabId, jobId, resumeIds, page, fields, awaiting }
let pendingNeeds = null; // last {page, items} we paused on (for popup re-open)
let logBuf = [];         // recent progress lines, so a reopened popup can catch up
let stopFlag = false;
let loaded = false;      // whether we've restored from session storage yet

// ── helpers ──────────────────────────────────────────────────────────────────

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// Promise-style (no callback): Firefox's browser.* ignores callbacks.
function getPairing() { return browser.storage.local.get("pairing").then((d) => (d && d.pairing) || null); }

// Run state survives the popup closing AND the service worker being suspended
// (MV3 kills it after ~30s idle — e.g. while paused waiting for the user).
function persist() {
  browser.storage.session.set({ runState: { ctx, pendingNeeds, log: logBuf } }).catch(() => {});
}
async function ensureLoaded() {
  if (loaded) return;
  loaded = true;
  try {
    const d = await browser.storage.session.get("runState");
    if (d.runState) { ctx = d.runState.ctx; pendingNeeds = d.runState.pendingNeeds; logBuf = d.runState.log || []; }
  } catch { /* session storage unavailable — start fresh */ }
}

function emit(msg) {
  if (msg && msg.text) { logBuf.push(msg.text); if (logBuf.length > 60) logBuf.shift(); persist(); }
  browser.runtime.sendMessage(msg).catch(() => {});
}
function setBadge(text, color) {
  browser.action.setBadgeText({ text: text || "" });
  if (color) browser.action.setBadgeBackgroundColor({ color });
}

function api(pairing, path, opts = {}) {
  const headers = Object.assign({ "X-InterPrep-Token": pairing.token }, opts.headers || {});
  if (opts.body) headers["Content-Type"] = "application/json";
  return fetch(`http://127.0.0.1:${pairing.port}${path}`, { ...opts, headers });
}

async function exec(tabId, func, args, allFrames = true) {
  const target = allFrames ? { tabId, allFrames: true } : { tabId };
  try { return await browser.scripting.executeScript({ target, func, args }); }
  catch { return await browser.scripting.executeScript({ target: { tabId }, func, args }); }
}

async function scrapeAll(tabId) {
  const res = await exec(tabId, ipScrape);
  const fields = [];
  for (const r of res || []) {
    if (!r || !r.result) continue;
    for (const f of r.result) fields.push({ ...f, frameId: r.frameId, localKey: f.key, key: `${r.frameId}:${f.key}` });
  }
  return fields;
}

async function fillAll(tabId, answers, fields) {
  const byFrame = {};
  for (const a of answers) {
    if (a.needs_user || a.value === "" || a.value == null) continue;
    const f = fields.find((x) => x.key === a.key);
    if (!f) continue;
    (byFrame[f.frameId] ||= []).push({ key: f.localKey, value: a.value, type: f.type, radioName: f.radioName });
  }
  let filled = 0;
  for (const fid of Object.keys(byFrame)) {
    const r = await browser.scripting.executeScript({ target: { tabId, frameIds: [+fid] }, func: ipFill, args: [byFrame[fid]] });
    filled += (r && r[0] && r[0].result) || 0;
  }
  return filled;
}

async function attach(tabId, pairing, jobId) {
  let r;
  try { r = await api(pairing, `/store/resume-file/${jobId}`); } catch { return false; }
  if (!r.ok) return false;
  const { b64, filename, mime } = await r.json();
  const res = await exec(tabId, ipAttach, [b64, filename, mime]);
  return (res || []).reduce((s, x) => s + ((x && x.result) || 0), 0) > 0;
}

async function findNav(tabId) {
  const res = await exec(tabId, ipFindNav);
  let next = false, submit = false, label = "", frameId = null;
  for (const r of res || []) {
    if (!r || !r.result) continue;
    if (r.result.next && frameId == null) { next = true; frameId = r.frameId; label = r.result.label; }
    if (r.result.next) next = true;
    if (r.result.submit) submit = true;
  }
  return { next, submit, label, frameId };
}

async function clickNext(tabId, frameId) {
  const r = await browser.scripting.executeScript({ target: { tabId, frameIds: [frameId] }, func: ipClickNext });
  return (r && r[0] && r[0].result) || "";
}

async function fingerprint(tabId) {
  const res = await exec(tabId, ipFingerprint);
  return (res || []).map((r) => (r && r.result) || "").join("||");
}

async function waitForChange(tabId, before, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  await sleep(800); // give navigation/SPA a moment to start
  while (Date.now() < deadline) {
    let fp;
    try { fp = await fingerprint(tabId); } catch { fp = before; }
    if (fp && fp !== before) { await sleep(700); return true; } // let it settle
    await sleep(600);
  }
  return false;
}

function cleanup() {
  ctx = null; pendingNeeds = null; stopFlag = false;
  browser.storage.session.remove("runState").catch(() => {});
  setTimeout(() => setBadge("", null), 5000);
}

// Best-effort multi-step progress nav, picking the richest stepper across frames.
async function scrapeSteps(tabId) {
  try {
    const res = await exec(tabId, ipScrapeSteps);
    let best = null;
    for (const r of res || []) {
      const s = r && r.result;
      if (s && s.steps && (!best || s.steps.length > best.steps.length)) best = s;
    }
    return best;
  } catch { return null; }
}

// Run-summary bookkeeping (drives the "Application autofilled" done screen).
function addNeed(item) {
  ctx.needsLog ||= [];
  if (!ctx.needsLog.some((x) => x.label === item.label)) ctx.needsLog.push({ label: item.label, reason: item.reason || "" });
}
function resolveNeed(label) { if (ctx.needsLog) ctx.needsLog = ctx.needsLog.filter((x) => x.label !== label); }

function finishRun(text) {
  if (!ctx) { emit({ type: "done", text }); return; }
  emit({ type: "done", text });
  emit({ type: "summary", filled: ctx.filledTotal || 0, pages: ctx.page || 0, needs: ctx.needsLog || [], jobId: ctx.jobId || "" });
  cleanup();
}

// ── the loop ─────────────────────────────────────────────────────────────────

async function loop() {
  const pairing = await getPairing();
  if (!pairing) { emit({ type: "error", text: "Not paired." }); cleanup(); return; }

  while (ctx && ctx.page <= MAX_PAGES) {
    if (stopFlag) { emit({ type: "stopped", text: "Stopped." }); cleanup(); return; }
    emit({ type: "progress", text: `Page ${ctx.page}: scanning…` });

    emit({ type: "step", page: ctx.page, steps: await scrapeSteps(ctx.tabId) });

    try { await exec(ctx.tabId, ipExpandSections); } catch { /* reveal collapsed sections — best-effort */ }
    const fields = await scrapeAll(ctx.tabId);
    ctx.fields = fields;

    if (fields.length) {
      let answers = [];
      try {
        const resp = await api(pairing, "/autofill/answer-fields", {
          method: "POST",
          body: JSON.stringify({
            job_id: ctx.jobId, resume_ids: ctx.resumeIds,
            fields: fields.map((f) => ({ key: f.key, label: f.label, type: f.type, options: f.options || [] })),
          }),
        });
        if (!resp.ok) { emit({ type: "error", text: `Backend HTTP ${resp.status}.` }); cleanup(); return; }
        answers = (await resp.json()).answers;
      } catch { emit({ type: "error", text: "Backend unreachable — is InterPrep open?" }); cleanup(); return; }

      const filled = await fillAll(ctx.tabId, answers, fields);
      ctx.filledTotal = (ctx.filledTotal || 0) + filled;
      emit({ type: "progress", text: `Page ${ctx.page}: filled ${filled} field(s).` });
      if (ctx.jobId) await attach(ctx.tabId, pairing, ctx.jobId);

      const needs = answers.filter((a) =>
        a.needs_user || a.value === "" || a.value == null || (a.source === "model" && (a.confidence ?? 1) < LOWCONF)
      );
      if (needs.length) {
        ctx.awaiting = true;
        setBadge(String(needs.length), "#d9534f");
        pendingNeeds = {
          page: ctx.page,
          items: needs.map((a) => ({
            key: a.key,
            label: (fields.find((f) => f.key === a.key) || {}).label || a.key,
            value: (!a.needs_user && a.value) || "",
            reason: a.reason || "",
            guess: !a.needs_user && !!a.value,
          })),
        };
        for (const it of pendingNeeds.items) addNeed(it);
        persist();
        emit({ type: "needs", page: pendingNeeds.page, items: pendingNeeds.items });
        return; // wait for resumeAnswers
      }
    } else {
      emit({ type: "progress", text: `Page ${ctx.page}: no fillable fields.` });
    }

    if (!(await proceed(pairing))) return; // step-pause or terminal (proceed finishes)
    ctx.page++;
  }
  finishRun(`Reached page limit (${MAX_PAGES}) — stopping.`);
}

// After a page is cleanly filled: in step mode, pause for the user to click
// "Fill step & continue"; otherwise advance automatically.
async function proceed(pairing) {
  if (stopFlag) { emit({ type: "stopped", text: "Stopped." }); cleanup(); return false; }
  if (ctx.stepMode) {
    ctx.awaitingStep = true;
    persist();
    setBadge("›", "#8b6bff");
    emit({ type: "stepPaused", page: ctx.page, filled: ctx.filledTotal || 0, steps: await scrapeSteps(ctx.tabId) });
    return false;
  }
  return await advanceAndContinue(pairing);
}

// Click Next + wait. Returns "next" | "submit" | "nochange" | "end".
async function advance(pairing) {
  const nav = await findNav(ctx.tabId);
  if (nav.next && nav.frameId != null) {
    const before = await fingerprint(ctx.tabId);
    const label = await clickNext(ctx.tabId, nav.frameId);
    emit({ type: "progress", text: `Clicked “${label || nav.label || "Next"}” — waiting…` });
    return (await waitForChange(ctx.tabId, before, 12000)) ? "next" : "nochange";
  }
  return nav.submit ? "submit" : "end";
}

// Drive advance() + map terminal states to a finished run. Returns true only when
// the page advanced and the loop should continue to the next page.
async function advanceAndContinue(pairing) {
  const r = await advance(pairing);
  if (r === "next") return true;
  if (r === "submit") { setBadge("✓", "#5cb85c"); finishRun("Reached the Submit step — review everything and submit it yourself."); }
  else if (r === "nochange") { finishRun("Page didn’t change after Next (validation error?) — stopping."); }
  else { finishRun("No Next button — looks complete or single-page."); }
  return false;
}

// Resume after the user supplies the answers we paused on.
async function resumeWith(answers) {
  if (!ctx || !ctx.awaiting) return;
  const pairing = await getPairing();
  ctx.awaiting = false;
  pendingNeeds = null;
  persist();
  setBadge("", null);
  const n = await fillAll(ctx.tabId, answers.map((a) => ({ ...a, needs_user: false })), ctx.fields);
  ctx.filledTotal = (ctx.filledTotal || 0) + n;
  for (const a of answers) {
    const label = (ctx.fields.find((f) => f.key === a.key) || {}).label || a.key;
    if (a.value) resolveNeed(label);
    api(pairing, "/autofill/remember", { method: "POST", body: JSON.stringify({ label, value: a.value }) }).catch(() => {});
  }
  if (await proceed(pairing)) { ctx.page++; loop(); }
}

// Resume after the user clicks "Fill step & continue" in step mode.
async function resumeStep() {
  if (!ctx || !ctx.awaitingStep) return;
  const pairing = await getPairing();
  ctx.awaitingStep = false;
  persist();
  setBadge("", null);
  if (await advanceAndContinue(pairing)) { ctx.page++; loop(); }
}

// ── messages from the popup ──────────────────────────────────────────────────

browser.runtime.onMessage.addListener((msg, _sender, send) => {
  if (msg.type === "startAuto") {
    stopFlag = false;
    logBuf = [];
    pendingNeeds = null;
    ctx = {
      tabId: msg.tabId, jobId: msg.jobId || "", resumeIds: msg.resumeIds || [],
      page: 1, awaiting: false, awaitingStep: false, fields: [],
      stepMode: !!msg.stepMode, filledTotal: 0, needsLog: [],
    };
    persist();
    send({ ok: true });
    loop();
    return true;
  }
  if (msg.type === "stop") {
    stopFlag = true;
    if (ctx) { ctx.awaiting = false; ctx.awaitingStep = false; }
    cleanup();
    setBadge("", null);
    send({ ok: true });
    return true;
  }
  if (msg.type === "resumeAnswers") {
    send({ ok: true });
    ensureLoaded().then(() => resumeWith(msg.answers || []));
    return true;
  }
  if (msg.type === "nextStep") {
    send({ ok: true });
    ensureLoaded().then(() => resumeStep());
    return true;
  }
  if (msg.type === "status") {
    ensureLoaded().then(() => send({
      running: !!ctx,
      awaiting: !!ctx?.awaiting,
      awaitingStep: !!ctx?.awaitingStep,
      stepMode: !!ctx?.stepMode,
      page: ctx?.page || 0,
      filled: ctx?.filledTotal || 0,
      tabId: ctx?.tabId,
      needs: ctx?.awaiting ? (pendingNeeds?.items || null) : null,
      needsPage: pendingNeeds?.page || 0,
      log: logBuf,
    }));
    return true; // async response
  }
  return false;
});
