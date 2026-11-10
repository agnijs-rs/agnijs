#!/usr/bin/env node
// Nightly Miri run on a local machine (docs/miri-local.md).
//
// Fetches origin/main into a clean checkout outside the repository, runs the
// full-size Miri collector suite there (AGNIJS_MIRI_FULL=1, no cfg!(miri)
// shrinking), writes the log to a file and reports through the `gh` CLI:
//
//   failure -> open (or comment on) the issue "Miri failure on main", label `ci`
//   success -> a weekly comment on the issue "Miri status" (at most once per 7 days)
//
// One process at a time, bounded by an overall timeout, exit code 1 on failure.
//
// Usage: node tools/miri-local.mjs [options]
//   --dry-run            print what would be posted; touch nothing on GitHub
//   --skip-run           do not fetch or run Miri; judge the existing log file
//   --repo <owner/name>  default agnijs-rs/agnijs
//   --dir <path>         clean checkout, default C:/Users/Jason/.t3/projects/agnijs-miri-run
//   --out <path>         log and state directory, default <dir>-out
//   --timeout-min <n>    overall timeout in minutes, default 180
//   --include-ignored    also run the tests that are #[ignore]d under Miri (very slow)

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const args = process.argv.slice(2);
const flag = (n) => args.includes(n);
const opt = (n, d) => {
  const i = args.indexOf(n);
  return i >= 0 && i + 1 < args.length ? args[i + 1] : d;
};

const dryRun = flag("--dry-run");
const repo = opt("--repo", "agnijs-rs/agnijs");
const dir = path.resolve(opt("--dir", "C:/Users/Jason/.t3/projects/agnijs-miri-run"));
const out = path.resolve(opt("--out", dir + "-out"));
const timeoutMs = Number(opt("--timeout-min", "180")) * 60_000;
const logFile = path.join(out, "miri-gc.log");
const stateFile = path.join(out, "state.json");
const FAILURE_TITLE = "Miri failure on main";
const STATUS_TITLE = "Miri status";
const WEEK_MS = 7 * 24 * 3600 * 1000;

fs.mkdirSync(out, { recursive: true });

function run(cmd, cmdArgs, o = {}) {
  const r = spawnSync(cmd, cmdArgs, { encoding: "utf8", shell: false, ...o });
  if (r.error) throw r.error;
  return r;
}

function gh(ghArgs, o = {}) {
  return run("gh", ghArgs, o);
}

function say(msg) {
  console.log(`[miri-local] ${msg}`);
}

// Steps 1 and 2: a clean checkout of origin/main.
function checkout() {
  if (!fs.existsSync(path.join(dir, ".git"))) {
    fs.mkdirSync(path.dirname(dir), { recursive: true });
    const r = run("git", ["clone", "--quiet", `https://github.com/${repo}.git`, dir]);
    if (r.status !== 0) throw new Error(`git clone failed: ${r.stderr}`);
  }
  for (const a of [
    ["fetch", "--quiet", "origin", "main"],
    ["checkout", "--quiet", "--detach", "origin/main"],
    ["reset", "--quiet", "--hard", "origin/main"],
    // Keep target/ (ignored) so incremental builds stay warm; drop anything else.
    ["clean", "-fdq"],
  ]) {
    const r = run("git", a, { cwd: dir });
    if (r.status !== 0) throw new Error(`git ${a.join(" ")} failed: ${r.stderr}`);
  }
  return run("git", ["rev-parse", "HEAD"], { cwd: dir }).stdout.trim();
}

// Step 3: the Miri run, one process, hard timeout, log to a file.
function miri() {
  return new Promise((resolve) => {
    const log = fs.openSync(logFile, "w");
    const cmdArgs = [
      "+nightly", "miri", "test", "--locked", "-p", "agnijs-js-vm", "--test", "gc",
    ];
    if (flag("--include-ignored")) cmdArgs.push("--", "--include-ignored");
    const env = {
      ...process.env,
      AGNIJS_MIRI_FULL: "1",
      MIRIFLAGS: "-Zmiri-disable-isolation",
      CARGO_TARGET_DIR: path.join(out, "target"),
      CARGO_TERM_COLOR: "never",
    };
    fs.writeSync(log, `$ cargo ${cmdArgs.join(" ")}\n`);
    const child = spawn("cargo", cmdArgs, { cwd: dir, env, stdio: ["ignore", log, log] });
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      fs.writeSync(log, `\n[miri-local] overall timeout of ${timeoutMs / 60000} minutes reached; killing\n`);
      if (process.platform === "win32") {
        spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"]);
      } else {
        child.kill("SIGKILL");
      }
    }, timeoutMs);
    child.on("error", (e) => {
      clearTimeout(timer);
      fs.writeSync(log, `\n[miri-local] could not start cargo: ${e.message}\n`);
      resolve({ code: 127, timedOut });
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      fs.closeSync(log);
      resolve({ code: code ?? 1, timedOut });
    });
  });
}

function readLog() {
  return fs.existsSync(logFile) ? fs.readFileSync(logFile, "utf8") : "";
}

function failingTests(text) {
  const names = new Set();
  for (const m of text.matchAll(/^test (\S+) \.\.\. FAILED/gm)) names.add(m[1]);
  return [...names];
}

function tail(text, n = 80) {
  return text.split(/\r?\n/).slice(-n).join("\n");
}

function ghJson(ghArgs) {
  const r = gh(ghArgs);
  if (r.status !== 0) throw new Error(`gh ${ghArgs.join(" ")}: ${r.stderr}`);
  return JSON.parse(r.stdout || "[]");
}

function findIssue(title, label) {
  const a = ["issue", "list", "-R", repo, "--state", "open", "--search", `"${title}" in:title`,
    "--json", "number,title", "--limit", "30"];
  if (label) a.push("--label", label);
  return ghJson(a).find((i) => i.title === title);
}

function ensureLabel(name) {
  const labels = ghJson(["label", "list", "-R", repo, "--json", "name", "--limit", "200"]);
  if (labels.some((l) => l.name === name)) return;
  gh(["label", "create", name, "-R", repo, "--color", "d93f0b", "--description", "Continuous integration"]);
}

function post(title, label, body, createOnly = false) {
  const bodyFile = path.join(out, "post-body.md");
  fs.writeFileSync(bodyFile, body);
  if (dryRun) {
    say(`DRY RUN: would post to "${title}"${label ? ` (label ${label})` : ""} in ${repo}:`);
    console.log("-----\n" + body + "\n-----");
    return;
  }
  if (label) ensureLabel(label);
  const existing = findIssue(title, label);
  if (existing) {
    const r = gh(["issue", "comment", String(existing.number), "-R", repo, "--body-file", bodyFile]);
    if (r.status !== 0) throw new Error(`gh issue comment failed: ${r.stderr}`);
    say(`commented on #${existing.number}`);
    return;
  }
  const a = ["issue", "create", "-R", repo, "--title", title, "--body-file", bodyFile];
  if (label) a.push("--label", label);
  const r = gh(a);
  if (r.status !== 0) throw new Error(`gh issue create failed: ${r.stderr}`);
  say(`opened ${r.stdout.trim()}`);
  if (title === STATUS_TITLE) {
    const num = r.stdout.trim().split("/").pop();
    gh(["issue", "pin", num, "-R", repo]); // best effort; older gh lacks it
  }
  void createOnly;
}

function readState() {
  try {
    return JSON.parse(fs.readFileSync(stateFile, "utf8"));
  } catch {
    return {};
  }
}

async function main() {
  const started = Date.now();
  let sha = "(unknown)";
  let result;
  if (flag("--skip-run")) {
    const text = readLog();
    const ok = /test result: ok\./.test(text) && failingTests(text).length === 0;
    result = { code: ok ? 0 : 1, timedOut: false };
    say(`--skip-run: judging existing log ${logFile}`);
    if (fs.existsSync(path.join(dir, ".git"))) {
      sha = run("git", ["rev-parse", "HEAD"], { cwd: dir }).stdout.trim() || sha;
    }
  } else {
    try {
      sha = checkout();
    } catch (e) {
      fs.writeFileSync(logFile, `[miri-local] checkout failed: ${e.message}\n`);
      result = { code: 2, timedOut: false };
    }
    if (!result) {
      say(`running Miri on ${sha} (log: ${logFile})`);
      result = await miri();
    }
  }

  const text = readLog();
  const minutes = ((Date.now() - started) / 60000).toFixed(1);
  const failed = failingTests(text);
  const ok = result.code === 0 && failed.length === 0;

  if (!ok) {
    const why = result.timedOut
      ? `timed out after ${timeoutMs / 60000} minutes`
      : `exit code ${result.code}`;
    const names = failed.length ? failed.map((n) => `- \`${n}\``).join("\n") : "- (no individual test failure parsed; see the log tail)";
    const body = [
      `Miri failed on \`main\` at ${sha} (${why}, ${minutes} min, ${new Date().toISOString()}).`,
      "",
      "Failing tests:",
      names,
      "",
      "Log tail (last 80 lines):",
      "```",
      tail(text),
      "```",
      "",
      `Full log on the maintainer's machine: \`${logFile}\``,
    ].join("\n");
    try {
      post(FAILURE_TITLE, "ci", body);
    } catch (e) {
      console.error(`[miri-local] could not report to GitHub: ${e.message}`);
    }
    say(`FAILED (${why})`);
    process.exit(1);
  }

  say(`green on ${sha} in ${minutes} min`);
  const state = readState();
  const last = state.lastGreenComment ? Date.parse(state.lastGreenComment) : 0;
  if (Date.now() - last >= WEEK_MS) {
    const body = `Miri (full size) is green on \`main\` at ${sha}. Run took ${minutes} min, ${new Date().toISOString()}.`;
    try {
      post(STATUS_TITLE, null, body);
      if (!dryRun) {
        fs.writeFileSync(stateFile, JSON.stringify({ lastGreenComment: new Date().toISOString(), sha }, null, 2));
      }
    } catch (e) {
      console.error(`[miri-local] could not post the weekly summary: ${e.message}`);
    }
  } else {
    say("last green comment is under 7 days old; not posting");
  }
  process.exit(0);
}

main().catch((e) => {
  console.error(e);
  process.exit(2);
});
