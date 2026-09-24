'use strict';
// Portable forensic recording harness for ANY Chromium started with
// --remote-debugging-port (dev Chrome/Edge/Canary, headless CI browser…).
// Injects tools/recorder.js before page scripts and writes the same
// session/run JSONL layout Mini's built-in debug mode produces, so
// `mini.exe --debug-explain <run_dir>` / `--debug-diff <a> <b>` work
// unchanged on the output.
//
// This is the FALLBACK for external Chromiums (headless CI, Chrome-specific
// bugs). The default debugging browser is Mini itself: mini.exe --debug has
// this recorder built in — no attach step at all.
//
// Usage (run from any repo):
//   node <this file> --launch [--headless] --url <dev-url> [--cdp http://127.0.0.1:9223] [--target <url-substring>] [--out <dir>]
// Default --out is %LOCALAPPDATA%\MiniRecord (never the repo tree: runs and
// the spawned browser profile would pollute git status).
// Rotate runs from the page or any attached driver:
//   window.__miniDebug.run('faulty', '0012')
// Stop with SIGINT.

const fs = require('fs');
const path = require('path');
// Vendored beside this script (tools/node_modules); %TEMP%\mini-cdp kept as
// a fallback for machines where the repo install is missing.
let puppeteer;
try {
  puppeteer = require('puppeteer-core');
} catch (_) {
  puppeteer = require(path.join(process.env.TEMP, 'mini-cdp', 'node_modules', 'puppeteer-core'));
}

const arg = (name, dflt) => {
  const i = process.argv.indexOf(name);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : dflt;
};

const CDP = arg('--cdp', 'http://127.0.0.1:9222');
const OUT = path.resolve(arg('--out',
  path.join(process.env.LOCALAPPDATA || '.', 'MiniRecord')));
const LABEL = arg('--label', 'default');
const TARGET = arg('--target', '');
// --launch [exe]: spawn a dedicated Chromium (fresh profile, extensions off)
// instead of attaching to an already-running one. Eliminates the Windows
// single-instance trap: launching the user's Edge/Chrome while it is already
// running exits ~0 in <1s (handoff) and never opens the CDP port.
const LAUNCH = process.argv.includes('--launch');
const LAUNCH_EXE = LAUNCH ? arg('--launch', '') : '';
const START_URL = arg('--url', ''); // NEVER name this URL: shadows Node's global URL class
const HEADLESS = process.argv.includes('--headless');

function findChromium() {
  if (LAUNCH_EXE && !LAUNCH_EXE.startsWith('--')) return LAUNCH_EXE;
  const candidates = [];
  try {
    const pw = path.join(process.env.LOCALAPPDATA, 'ms-playwright');
    for (const d of fs.readdirSync(pw)) {
      if (d.startsWith('chromium-')) {
        candidates.push(path.join(pw, d, 'chrome-win64', 'chrome.exe'));
        candidates.push(path.join(pw, d, 'chrome-win', 'chrome.exe'));
      }
    }
  } catch (_) {}
  candidates.push(
    'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
    'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
    'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
    'C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe',
  );
  for (const c of candidates) {
    try { fs.accessSync(c); return c; } catch (_) {}
  }
  return null;
}

const sanitize = (s) => {
  const t = String(s).replace(/[^A-Za-z0-9_-]/g, '_');
  return t || 'run';
};

const STREAMS = ['frontend', 'dom', 'backend', 'net', 'react', 'frames', 'marks'];

function openRun(sessionDir, index, label, incident) {
  const dir = path.join(sessionDir, `run-${String(index).padStart(3, '0')}-${sanitize(label)}`);
  fs.mkdirSync(dir, { recursive: true });
  const fds = {};
  for (const s of STREAMS) fds[s] = fs.openSync(path.join(dir, `${s}.jsonl`), 'a');
  fs.writeFileSync(path.join(dir, 'run.json'),
    JSON.stringify({ index, label, incident, started_ms: Date.now() }));
  return { dir, index, label, fds };
}

async function main() {
  const sessionDir = path.join(OUT, `session-${Date.now()}`);
  fs.mkdirSync(sessionDir, { recursive: true });
  let run = openRun(sessionDir, 0, LABEL, '');
  let nextIndex = 1;

  const writeCurrent = () => {
    fs.writeFileSync(path.join(OUT, 'current.json'), JSON.stringify({
      cdp: CDP,
      session_dir: sessionDir,
      pid: process.pid,
      started_ms: Date.now(),
      active_run_dir: run.dir,
      run_label: run.label,
      run_index: run.index,
    }));
  };
  writeCurrent();

  const line = (stream, body) => {
    try { fs.writeSync(run.fds[stream], body + '\n'); } catch (_) {}
  };
  const backend = (event, detail) => {
    line('backend', JSON.stringify({ t: Date.now(), event, detail }));
  };

  const onPost = (body) => {
    let msg = null;
    try { msg = JSON.parse(body); } catch (_) { return; }
    // Scope filter at write time: pages are instrumented unconditionally
    // (a new tab is about:blank at targetcreated, so URL filtering at
    // attach time would skip the very tab under test).
    if (TARGET && !(typeof msg.url === 'string' && msg.url.includes(TARGET))) return;
    switch (msg.kind) {
      case 'dom': line('dom', body); break;
      case 'net': line('net', body); break;
      case 'react': line('react', body); break;
      case 'frame': line('frames', body); break;
      case 'mark': line('marks', body); break;
      case 'run': {
        try {
          const next = openRun(sessionDir, nextIndex, msg.label || 'run', msg.incident || '');
          for (const s of STREAMS) { try { fs.closeSync(run.fds[s]); } catch (_) {} }
          run = next;
          nextIndex += 1;
          writeCurrent();
        } catch (e) {
          backend('run-error', String(e));
        }
        break;
      }
      default: line('frontend', body);
    }
  };

  const recorder = fs.readFileSync(path.join(__dirname, 'recorder.js'), 'utf8');
  let browser;
  if (LAUNCH) {
    const exe = findChromium();
    if (!exe) {
      console.error('--launch: no Chromium found (ms-playwright cache, Chrome, Edge)');
      process.exit(1);
    }
    const port = Number(new (require('url').URL)(CDP).port) || 9222;
    browser = await puppeteer.launch({
      executablePath: exe,
      headless: HEADLESS ? 'new' : false,
      defaultViewport: null,
      userDataDir: path.join(OUT, 'browser-profile'),
      args: [
        `--remote-debugging-port=${port}`,
        '--disable-extensions',
        '--no-first-run',
        '--no-default-browser-check',
      ],
    });
    console.log(JSON.stringify({ launched: exe, cdp: CDP }));
  } else {
    browser = await puppeteer.connect({ browserURL: CDP, defaultViewport: null });
  }
  const instrumented = new WeakSet();
  const instrument = async (page) => {
    if (!page || instrumented.has(page)) return;
    instrumented.add(page);
    try {
      await page.exposeFunction('__miniPost', onPost);
      await page.evaluateOnNewDocument(recorder);
      await page.evaluate(recorder).catch(() => {}); // late-inject the already-loaded doc
      backend('record-attach', page.url());
      console.log(JSON.stringify({ attached: page.url() || '(blank)' }));
    } catch (e) {
      backend('record-attach-error', String(e));
    }
  };

  const pages = await browser.pages();
  if (!pages.length && !LAUNCH) {
    console.error('no open pages');
    process.exit(1);
  }
  for (const p of pages) await instrument(p);
  browser.on('targetcreated', async (t) => {
    try {
      if (t.type() === 'page') await instrument(await t.page());
    } catch (_) {}
  });
  if (START_URL && pages[0]) {
    await pages[0].goto(START_URL, { waitUntil: 'domcontentloaded' }).catch((e) => {
      backend('goto-error', String(e));
    });
  }
  console.log(JSON.stringify({ out: OUT, active_run_dir: run.dir }));

  process.on('SIGINT', async () => {
    backend('record-detach', '');
    for (const s of STREAMS) { try { fs.closeSync(run.fds[s]); } catch (_) {} }
    if (LAUNCH) { try { await browser.close(); } catch (_) {} }
    process.exit(0);
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
