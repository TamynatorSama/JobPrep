// Build guard for the stealth copilot overlay (src/copilot).
//
// The overlay window is hidden from screen capture with
// SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE), but anything the browser
// draws as its OWN popup window — native `title=` tooltips, <select> and
// <datalist> dropdowns — is a separate top-level window the cloak doesn't
// cover, so it would show up on a screen share. Fail the build if one sneaks
// back in. Use `aria-label` for accessible names (it never renders).
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const ROOT = join(import.meta.dirname, "..", "src", "copilot");
const RULES = [
  { re: /\stitle=/, why: "native tooltip — use aria-label instead" },
  { re: /<select[\s>]/, why: "native dropdown popup" },
  { re: /<datalist[\s>]/, why: "native suggestion popup" },
];

const files = [];
const walk = (dir) => {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p);
    else if (/\.(tsx|jsx|ts|js)$/.test(name)) files.push(p);
  }
};
walk(ROOT);

const hits = [];
for (const file of files) {
  readFileSync(file, "utf8").split(/\r?\n/).forEach((line, i) => {
    for (const { re, why } of RULES) {
      if (re.test(line)) hits.push(`${relative(process.cwd(), file)}:${i + 1}: ${why}\n    ${line.trim()}`);
    }
  });
}

if (hits.length) {
  console.error(`Copilot stealth check failed — ${hits.length} popup source(s) in src/copilot:\n`);
  console.error(hits.join("\n"));
  process.exit(1);
}
console.log(`Copilot stealth check passed (${files.length} file(s)).`);
