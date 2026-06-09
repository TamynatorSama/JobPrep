// Functions injected into pages via chrome.scripting (must be self-contained —
// no closure over outside scope). Loaded as globals in BOTH the popup
// (<script src>) and the service worker (importScripts) so the single-page and
// multi-page drivers share one implementation.

// Scrape a frame's form fields. Tags non-radio elements with data-ip-key.
function ipScrape() {
  const SKIP = new Set(["hidden", "submit", "button", "reset", "image", "file", "password"]);
  const clean = (s) => (s || "").replace(/\s+/g, " ").trim().slice(0, 200);
  const vis = (el) => {
    if (el.disabled || el.readOnly) return false;
    const st = getComputedStyle(el);
    if (st.display === "none" || st.visibility === "hidden" || st.opacity === "0") return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  };
  const labelFor = (el) => {
    if (el.id) { const l = document.querySelector(`label[for="${CSS.escape(el.id)}"]`); if (l && l.innerText.trim()) return clean(l.innerText); }
    const w = el.closest("label"); if (w && w.innerText.trim()) return clean(w.innerText);
    if (el.getAttribute("aria-label")) return clean(el.getAttribute("aria-label"));
    const lb = el.getAttribute("aria-labelledby");
    if (lb) { const t = lb.split(/\s+/).map((id) => document.getElementById(id)?.innerText || "").join(" "); if (t.trim()) return clean(t); }
    if (el.placeholder) return clean(el.placeholder);
    if (el.name) return clean(el.name.replace(/[_\-]+/g, " "));
    return "";
  };
  const optLabel = (el) => {
    if (el.id) { const l = document.querySelector(`label[for="${CSS.escape(el.id)}"]`); if (l && l.innerText.trim()) return clean(l.innerText); }
    const w = el.closest("label"); if (w && w.innerText.trim()) return clean(w.innerText);
    if (el.getAttribute("aria-label")) return clean(el.getAttribute("aria-label"));
    return clean(el.value);
  };
  const groupLabel = (g) => {
    const f = g[0];
    const fs = f.closest("fieldset"); const lg = fs && fs.querySelector("legend");
    if (lg && lg.innerText.trim()) return clean(lg.innerText);
    const lb = f.getAttribute("aria-labelledby");
    if (lb) { const t = lb.split(/\s+/).map((id) => document.getElementById(id)?.innerText || "").join(" "); if (t.trim()) return clean(t); }
    const grp = f.closest('[role="radiogroup"]'); if (grp && grp.getAttribute("aria-label")) return clean(grp.getAttribute("aria-label"));
    return clean(f.name || "");
  };

  const fields = []; const seen = new Set(); let i = 0;
  document.querySelectorAll("input, textarea, select").forEach((el) => {
    const tag = el.tagName.toLowerCase();
    const type = (el.type || "text").toLowerCase();
    if (tag === "input" && SKIP.has(type)) return;
    if (!vis(el)) return;
    if (type === "radio") {
      const name = el.name; if (!name || seen.has(name)) return; seen.add(name);
      const g = [...document.querySelectorAll(`input[type=radio][name="${CSS.escape(name)}"]`)];
      fields.push({ key: "f" + (i++), label: groupLabel(g) || name, type: "radio", options: g.map(optLabel), radioName: name });
      return;
    }
    let ft = "text"; let opts = [];
    if (tag === "textarea") ft = "textarea";
    else if (tag === "select") { ft = "select"; opts = [...el.options].map((o) => o.text.trim()).filter(Boolean); }
    else if (type === "checkbox") ft = "checkbox";
    else ft = type;
    const key = "f" + (i++);
    el.setAttribute("data-ip-key", key);
    fields.push({ key, label: labelFor(el), type: ft, options: opts });
  });
  return fields;
}

// Fill a frame. items = [{key(localKey), value, type, radioName?}]. Returns count.
function ipFill(items) {
  const fire = (el) => { el.dispatchEvent(new Event("input", { bubbles: true })); el.dispatchEvent(new Event("change", { bubbles: true })); };
  const setNative = (el, v) => {
    const p = el.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const s = Object.getOwnPropertyDescriptor(p, "value")?.set;
    if (s) s.call(el, v); else el.value = v;
    fire(el);
  };
  const eq = (a, b) => String(a).trim().toLowerCase() === String(b).trim().toLowerCase();
  const inc = (a, b) => String(a).trim().toLowerCase().includes(String(b).trim().toLowerCase());
  let filled = 0;
  for (const it of items) {
    const v = it.value;
    if (it.radioName) {
      const g = [...document.querySelectorAll(`input[type=radio][name="${CSS.escape(it.radioName)}"]`)];
      const lab = (r) => { const l = r.id && document.querySelector(`label[for="${CSS.escape(r.id)}"]`); return (l ? l.innerText : r.value) || ""; };
      const m = g.find((r) => eq(lab(r), v)) || g.find((r) => inc(lab(r), v));
      if (m) { m.checked = true; fire(m); filled++; }
      continue;
    }
    const el = document.querySelector(`[data-ip-key="${it.key}"]`);
    if (!el) continue;
    const tag = el.tagName.toLowerCase();
    if (tag === "select") {
      const o = [...el.options].find((x) => eq(x.text, v)) || [...el.options].find((x) => inc(x.text, v));
      if (o) { el.value = o.value; fire(el); filled++; }
      continue;
    }
    if ((el.type || "").toLowerCase() === "checkbox") { el.checked = /^(true|yes|1|on)$/i.test(String(v)); fire(el); filled++; continue; }
    setNative(el, v); filled++;
  }
  return filled;
}

// Attach a resume (base64) to the best file-upload input. Returns 1 / 0.
function ipAttach(b64, filename, mime) {
  const inputs = [...document.querySelectorAll('input[type=file]')];
  if (!inputs.length) return 0;
  const score = (el) => {
    const s = ((el.name || "") + (el.id || "") + (el.getAttribute("aria-label") || "")).toLowerCase();
    const acc = (el.accept || "").toLowerCase();
    let n = 0;
    if (/resume|cv|attach/.test(s)) n += 2;
    if (/pdf|word|doc|officedocument/.test(acc)) n += 1;
    return n;
  };
  inputs.sort((a, b) => score(b) - score(a));
  const bin = atob(b64);
  const arr = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
  const file = new File([arr], filename, { type: mime });
  const target = inputs[0];
  try {
    const dt = new DataTransfer();
    dt.items.add(file);
    target.files = dt.files;
    target.dispatchEvent(new Event("input", { bubbles: true }));
    target.dispatchEvent(new Event("change", { bubbles: true }));
    return 1;
  } catch { return 0; }
}

// Report navigation buttons in this frame WITHOUT clicking.
// Returns { next: bool, submit: bool, label }.
function ipFindNav() {
  const NEXT = /\b(next|continue|save\s*(and|&)\s*continue|proceed|review|go on)\b/i;
  const SUBMIT = /\b(submit|apply|finish|complete application|send application)\b/i;
  const BACK = /\b(back|previous|cancel|return)\b/i;
  const vis = (el) => {
    if (el.disabled) return false;
    const st = getComputedStyle(el);
    if (st.display === "none" || st.visibility === "hidden") return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  };
  const txt = (el) => (el.innerText || el.value || el.getAttribute("aria-label") || "").trim();
  const cands = [...document.querySelectorAll('button, a[role=button], input[type=submit], input[type=button], [role=button]')].filter(vis);
  let next = false, submit = false, label = "";
  for (const c of cands) {
    const t = txt(c);
    if (!t) continue;
    if (NEXT.test(t) && !SUBMIT.test(t) && !BACK.test(t)) { next = true; label = t.slice(0, 40); }
    else if (SUBMIT.test(t) && !BACK.test(t)) submit = true;
  }
  return { next, submit, label };
}

// Click the Next/Continue button in this frame. Returns the clicked label or "".
function ipClickNext() {
  const NEXT = /\b(next|continue|save\s*(and|&)\s*continue|proceed|review|go on)\b/i;
  const SUBMIT = /\b(submit|apply|finish|complete application|send application)\b/i;
  const BACK = /\b(back|previous|cancel|return)\b/i;
  const vis = (el) => {
    if (el.disabled) return false;
    const st = getComputedStyle(el);
    if (st.display === "none" || st.visibility === "hidden") return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  };
  const txt = (el) => (el.innerText || el.value || el.getAttribute("aria-label") || "").trim();
  const cands = [...document.querySelectorAll('button, a[role=button], input[type=submit], input[type=button], [role=button]')].filter(vis);
  // Last match tends to be the primary bottom-right action.
  let target = null, label = "";
  for (const c of cands) {
    const t = txt(c);
    if (t && NEXT.test(t) && !SUBMIT.test(t) && !BACK.test(t)) { target = c; label = t.slice(0, 40); }
  }
  if (!target) return "";
  target.click();
  return label;
}

// Scrape a job description page. Prefers schema.org JobPosting JSON-LD (most
// reliable), falls back to og/meta + the largest description-ish block.
// Returns { company, role, location, job_description, url, employment_type,
// work_mode, salary, posted } — the chip fields are "" when not in the JSON-LD.
function ipScrapeJD() {
  const clean = (s) => (s || "").replace(/\s+/g, " ").trim();
  const htmlToText = (html) => { const d = document.createElement("div"); d.innerHTML = html; return (d.textContent || "").replace(/\n{3,}/g, "\n\n").trim(); };

  // "FULL_TIME" -> "Full-time", etc. Falls back to a title-cased raw value.
  const fmtEmployment = (v) => {
    const one = Array.isArray(v) ? v[0] : v;
    const map = { FULL_TIME: "Full-time", PART_TIME: "Part-time", CONTRACTOR: "Contract", TEMPORARY: "Temporary", INTERN: "Internship", VOLUNTEER: "Volunteer", PER_DIEM: "Per diem", OTHER: "" };
    const key = String(one || "").toUpperCase().replace(/[\s-]+/g, "_");
    if (key in map) return map[key];
    return clean(String(one || "")).replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
  };
  // baseSalary (MonetaryAmount) -> "$180k–$240k/yr".
  const fmtSalary = (bs) => {
    if (!bs || typeof bs !== "object") return "";
    const cur = bs.currency || bs.salaryCurrency || "";
    const sym = { USD: "$", GBP: "£", EUR: "€", CAD: "C$", AUD: "A$" }[cur] || (cur ? cur + " " : "");
    const val = bs.value || bs;
    const unit = String(val.unitText || "").toUpperCase();
    const suffix = { YEAR: "/yr", HOUR: "/hr", MONTH: "/mo", WEEK: "/wk", DAY: "/day" }[unit] || "";
    const k = (n) => { const x = Number(n); if (!x) return ""; return x >= 1000 ? Math.round(x / 1000) + "k" : String(x); };
    const min = k(val.minValue), max = k(val.maxValue), one = k(val.value);
    let body = "";
    if (min && max) body = `${min}–${max}`;
    else if (one) body = one;
    else if (min) body = min + "+";
    if (!body) return "";
    return `${sym}${body}${suffix}`;
  };

  let company = "", role = "", place = "", desc = "";
  let employmentType = "", workMode = "", salary = "", posted = "";

  const findJP = (node) => {
    if (!node || typeof node !== "object") return null;
    if (Array.isArray(node)) { for (const n of node) { const r = findJP(n); if (r) return r; } return null; }
    const t = node["@type"];
    if (t === "JobPosting" || (Array.isArray(t) && t.includes("JobPosting"))) return node;
    if (node["@graph"]) return findJP(node["@graph"]);
    return null;
  };
  for (const b of document.querySelectorAll('script[type="application/ld+json"]')) {
    let data; try { data = JSON.parse(b.textContent); } catch { continue; }
    const jp = findJP(data);
    if (!jp) continue;
    role = clean(jp.title || "");
    const org = jp.hiringOrganization;
    company = clean((org && (org.name || (typeof org === "string" ? org : ""))) || "");
    const loc = Array.isArray(jp.jobLocation) ? jp.jobLocation[0] : jp.jobLocation;
    const addr = (loc && loc.address) || {};
    place = clean([addr.addressLocality, addr.addressRegion, addr.addressCountry].filter(Boolean).join(", ")) || clean(jp.jobLocationType || "");
    desc = jp.description ? htmlToText(jp.description) : "";
    employmentType = fmtEmployment(jp.employmentType);
    workMode = String(jp.jobLocationType || "").toUpperCase() === "TELECOMMUTE" ? "Remote" : "";
    salary = fmtSalary(jp.baseSalary);
    posted = clean(jp.datePosted || ""); // ISO date — popup renders the relative form
    break;
  }

  const meta = (sel) => document.querySelector(sel)?.content || "";
  if (!role) role = clean(meta('meta[property="og:title"]') || document.title);
  if (!company) company = clean(meta('meta[property="og:site_name"]')) || clean(window.location.hostname.replace(/^www\./, "").split(".")[0]);
  if (!desc) {
    const cand = document.querySelector('[class*="job-description" i], [class*="description" i], [id*="description" i], article, main, [role=main]');
    desc = clean(cand ? cand.innerText : document.body.innerText);
  }
  desc = desc.slice(0, 20000);

  return {
    company, role, location: place, job_description: desc, url: window.location.href,
    employment_type: employmentType, work_mode: workMode, salary, posted,
  };
}

// Grab page text (+ a head block) for LLM-based JD extraction. Capped so the
// request stays small. Prefers the main/article region but falls back to body.
function ipPageText() {
  const meta = (sel) => document.querySelector(sel)?.content || "";
  const head = [
    document.title,
    meta('meta[name="description"]'),
    meta('meta[property="og:title"]'),
    meta('meta[property="og:site_name"]'),
  ].filter(Boolean).join(" | ");
  const main = document.querySelector("main, [role=main], article") || document.body;
  const body = (main.innerText || document.body.innerText || "").replace(/\n{3,}/g, "\n\n").trim();
  const text = (head ? head + "\n\n" : "") + body;
  return { title: document.title, url: window.location.href, text: text.slice(0, 30000) };
}

// A cheap fingerprint of the page state, to detect when a Next click advanced.
function ipFingerprint() {
  const inputs = document.querySelectorAll("input, textarea, select");
  const labels = [...document.querySelectorAll("label, legend, h1, h2, h3")].slice(0, 12).map((e) => (e.innerText || "").trim()).join("|");
  return location.href + "#" + inputs.length + "#" + labels.length + "#" + labels.slice(0, 200);
}

// Best-effort scrape of a multi-step "wizard" progress nav (Workday, generic
// step lists). Returns { steps:[{label,state}], current, total } or null when no
// recognizable stepper is found — the popup then falls back to a page counter.
// state is "done" | "current" | "pending".
function ipScrapeSteps() {
  const clean = (s) => (s || "").replace(/\s+/g, " ").trim().slice(0, 40);
  const sels = [
    '[data-automation-id="progressBar"]',
    'nav[aria-label*="progress" i]', 'nav[aria-label*="step" i]',
    '[role="navigation"][aria-label*="step" i]',
    'ol[class*="step" i]', 'ul[class*="step" i]',
    '[class*="wizard" i]', '[class*="stepper" i]',
    '[class*="progress" i] ol', '[class*="progress" i] ul',
  ];
  let best = null;
  for (const sel of sels) {
    let containers; try { containers = document.querySelectorAll(sel); } catch { continue; }
    for (const c of containers) {
      const items = [...c.querySelectorAll('li, [role="listitem"], [data-automation-id*="progressBarStep" i]')];
      const steps = [];
      let current = -1;
      for (const it of items) {
        const label = clean(it.getAttribute("aria-label") || it.innerText);
        if (!label) continue;
        const cls = String((it.className && it.className.baseVal) || it.className || "");
        const isCurrent = !!it.getAttribute("aria-current") || /\b(active|current|filling|in[-\s]?progress|selected)\b/i.test(cls);
        const isDone = /\b(done|complete|completed|visited|finished|past)\b/i.test(cls) || !!it.querySelector('[class*="check" i], svg');
        let state = "pending";
        if (isCurrent) { state = "current"; current = steps.length; }
        else if (isDone) state = "done";
        steps.push({ label, state });
      }
      if (steps.length >= 2 && steps.length <= 12 && (!best || steps.length > best.steps.length)) {
        best = { steps, current, total: steps.length };
      }
    }
  }
  return best;
}

// Scroll to + focus the first visible empty text/select field (the "jump to
// first blank" action on the done summary). Returns the field's label or "".
function ipFocusFirstBlank() {
  const SKIP = new Set(["hidden", "submit", "button", "reset", "image", "file", "password", "checkbox", "radio"]);
  const vis = (el) => {
    const st = getComputedStyle(el);
    if (st.display === "none" || st.visibility === "hidden") return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  };
  for (const el of document.querySelectorAll("input, textarea, select")) {
    const tag = el.tagName.toLowerCase();
    const type = (el.type || "text").toLowerCase();
    if (tag === "input" && SKIP.has(type)) continue;
    if (el.disabled || el.readOnly || !vis(el)) continue;
    const empty = tag === "select" ? (!el.value || el.selectedIndex <= 0) : !String(el.value || "").trim();
    if (!empty) continue;
    try { el.scrollIntoView({ behavior: "smooth", block: "center" }); } catch { /* older */ }
    try { el.focus({ preventScroll: true }); } catch { el.focus(); }
    const prev = el.style.outline;
    el.style.outline = "2px solid #8b6bff";
    setTimeout(() => { el.style.outline = prev; }, 2200);
    return ((el.getAttribute("aria-label") || el.name || el.id || "field") + "").slice(0, 60);
  }
  return "";
}
