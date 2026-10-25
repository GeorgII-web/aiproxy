// DeepSeek web chat driver. Started by AIProxy, talks JSON lines over stdio.
//
//   in : {"op":"chat","id":"…","prompt":"…","timeout_ms":300000}
//        {"op":"login","id":"…"}          force a visible browser for login
//        {"op":"cancel","id":"…"}         stop the running chat
//   out: {"type":"ready"}
//        {"id","type":"delta","text"}      streamed answer text
//        {"id","type":"done"}
//        {"id","type":"error","message"}
//        {"type":"log","level","message"}
//
// The browser profile (cookies) lives in AIPROXY_PROFILE_DIR, so a login done
// in the visible browser is reused by the headless one.

import { chromium } from 'playwright';
import fs from 'node:fs';
import { createInterface } from 'node:readline';
import path from 'node:path';

const PROFILE = process.env.AIPROXY_PROFILE_DIR || path.join(process.cwd(), 'profile');
const CHAT_URL = process.env.AIPROXY_URL || 'https://chat.deepseek.com';
// 'chromium' = Playwright's full Chromium in new headless mode (a normal user agent; the
// default headless shell announces itself as HeadlessChrome). 'chrome' / 'msedge' use the
// installed browser.
const CHANNEL = process.env.AIPROXY_CHANNEL || 'chromium';
const LOGIN_TIMEOUT_MS = 15 * 60 * 1000;
// Optional JSON with an existing session: {"token": "<userToken>", "cookie": "<Cookie header>"}.
const AUTH_FILE = process.env.AIPROXY_AUTH_FILE || '';

// DeepSeek's DOM is not stable; override any of these with AIPROXY_SELECTORS='{"input":"…"}'.
const SEL = Object.assign(
  {
    input: 'textarea#chat-input, textarea',
    // Rendered assistant answers (the last one is the current answer).
    message: '.ds-markdown',
    // Present while an answer is being generated.
    generating: '[class*="stop" i], button[aria-label*="stop" i]',
  },
  JSON.parse(process.env.AIPROXY_SELECTORS || '{}'),
);

const out = (obj) => process.stdout.write(JSON.stringify(obj) + '\n');
const log = (level, message) => out({ type: 'log', level, message });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let ctx = null;
let page = null;
let headless = true;
let cancelled = false;
let authTried = false;

/** Injects the session from AUTH_FILE into the current context; true when something was imported. */
async function importAuth() {
  if (!AUTH_FILE || !fs.existsSync(AUTH_FILE)) return false;
  let auth;
  try {
    auth = JSON.parse(fs.readFileSync(AUTH_FILE, 'utf8'));
  } catch (e) {
    log('warn', `auth file ${AUTH_FILE}: ${e.message}`);
    return false;
  }
  const host = new URL(CHAT_URL).hostname;
  const domain = '.' + host.split('.').slice(-2).join('.');
  let imported = 0;
  if (typeof auth.cookie === 'string' && auth.cookie.trim()) {
    const cookies = auth.cookie
      .split(';')
      .map((s) => s.trim())
      .filter((s) => s.includes('='))
      .map((kv) => {
        const i = kv.indexOf('=');
        return { name: kv.slice(0, i), value: kv.slice(i + 1), domain, path: '/' };
      });
    await ctx.addCookies(cookies);
    imported += cookies.length;
  }
  if (typeof auth.token === 'string' && auth.token) {
    await page.goto(CHAT_URL, { waitUntil: 'domcontentloaded' });
    await page.evaluate((t) => {
      localStorage.setItem('userToken', JSON.stringify({ value: t, __version: '0' }));
    }, auth.token);
    imported += 1;
  }
  if (imported === 0) return false;
  log('info', `session imported from ${AUTH_FILE}`);
  await goHome();
  return true;
}

async function close() {
  if (ctx) await ctx.close().catch(() => {});
  ctx = null;
  page = null;
}

// DeepSeek's CDN answers 403 to a "HeadlessChrome" user agent, so headless runs announce
// themselves as the plain Chrome of the same version.
let userAgent;
async function normalUserAgent() {
  if (userAgent) return userAgent;
  const probe = await chromium.launch({ headless: true, channel: CHANNEL });
  const major = probe.version().split('.')[0];
  await probe.close();
  const os = process.platform === 'win32' ? 'Windows NT 10.0; Win64; x64'
    : process.platform === 'darwin' ? 'Macintosh; Intel Mac OS X 10_15_7' : 'X11; Linux x86_64';
  userAgent = `Mozilla/5.0 (${os}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${major}.0.0.0 Safari/537.36`;
  return userAgent;
}

async function launch(hl) {
  await close();
  headless = hl;
  ctx = await chromium.launchPersistentContext(PROFILE, {
    headless: hl,
    channel: CHANNEL,
    viewport: { width: 1280, height: 900 },
    userAgent: hl ? await normalUserAgent() : undefined,
    args: ['--disable-blink-features=AutomationControlled'],
  });
  page = ctx.pages()[0] ?? (await ctx.newPage());
  log('info', `browser started (${hl ? 'headless' : 'visible'})`);
}

async function goHome() {
  await page.goto(CHAT_URL, { waitUntil: 'domcontentloaded' });
  await sleep(1500);
}

async function loggedIn() {
  try {
    const url = page.url();
    if (/sign_in|login/i.test(url)) return false;
    return await page.locator(SEL.input).first().isVisible();
  } catch {
    return false;
  }
}

function browserAlive() {
  return ctx !== null && page !== null && !page.isClosed();
}

/** Makes sure a headless, logged-in page is ready; opens a visible browser for login when needed. */
async function ensureSession(forceLogin = false) {
  if (!browserAlive()) await launch(!forceLogin);
  await goHome();
  if (!forceLogin && (await loggedIn())) return;

  if (!forceLogin && !authTried) {
    authTried = true;
    if ((await importAuth()) && (await loggedIn())) return;
    if (AUTH_FILE) log('warn', 'imported session not accepted (expired?), falling back to the login window');
  }

  if (headless) {
    const title = await page.title().catch(() => '');
    log('info', `login needed (at ${page.url()} "${title}"): opening a visible browser window`);
    await launch(false);
    await goHome();
  }
  const deadline = Date.now() + LOGIN_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (!browserAlive()) throw new Error('browser was closed before login finished');
    if (await loggedIn()) {
      log('info', 'login detected, switching to headless');
      await launch(true);
      await goHome();
      if (await loggedIn()) return;
      throw new Error('login did not persist in the browser profile');
    }
    await sleep(2000);
  }
  throw new Error('login timed out');
}

// Text of a rendered answer with code blocks back in markdown fences (the page renders a code block
// as a banner "<language> Copy Download" plus <pre>; innerText alone would leak the banner).
const ANSWER_TEXT = (el) => {
  const clone = el.cloneNode(true);
  for (const pre of Array.from(clone.querySelectorAll('pre'))) {
    const box = pre.closest('[class*="code-block"]') || pre;
    const code = (pre.querySelector('code') || pre).textContent.replace(/\n$/, '');
    let lang = '';
    for (const n of box.querySelectorAll('span, div')) {
      if (n.children.length || n === pre || pre.contains(n) || n.contains(pre)) continue;
      const t = n.textContent.trim();
      if (/^[A-Za-z][\w+#.-]{0,19}$/.test(t) && !/^(copy|copied|download|edit|run)$/i.test(t)) { lang = t; break; }
    }
    const node = document.createElement('div');
    node.style.whiteSpace = 'pre-wrap';
    node.textContent = '```' + lang + '\n' + code + '\n```';
    box.replaceWith(node);
  }
  clone.style.cssText = 'position:absolute;left:-99999px;top:0;opacity:0;pointer-events:none';
  document.body.appendChild(clone);
  try { return clone.innerText; } finally { clone.remove(); }
};

async function chat(id, prompt, timeoutMs) {
  await ensureSession();
  await goHome(); // a fresh conversation per request
  const input = page.locator(SEL.input).first();
  await input.waitFor({ state: 'visible', timeout: 30000 });
  const messages = page.locator(SEL.message);
  const before = await messages.count();
  await input.fill(prompt);
  await input.press('Enter');

  cancelled = false;
  let extractError = false;
  const deadline = Date.now() + timeoutMs;
  let sent = '';
  let started = false;
  let generatingSeen = false;
  let lastChange = Date.now();
  while (Date.now() < deadline) {
    if (cancelled) {
      await page.locator(SEL.generating).first().click({ timeout: 1000 }).catch(() => {});
      log('info', 'chat cancelled');
      return;
    }
    const count = await messages.count();
    const text = count > before
      ? (await messages.nth(count - 1).evaluate(ANSWER_TEXT).catch((e) => {
          if (!extractError) log('warn', `answer text: ${String(e?.message ?? e).split('\n')[0]}`);
          extractError = true;
          return '';
        })).trimEnd()
      : '';
    const generating = (await page.locator(SEL.generating).count()) > 0;
    generatingSeen ||= generating;
    if (text && text !== sent) {
      started = true;
      lastChange = Date.now();
      if (text.startsWith(sent)) {
        out({ id, type: 'delta', text: text.slice(sent.length) });
        sent = text;
      }
      // Otherwise earlier text was re-rendered; wait for the final version.
    }
    const stableMs = generatingSeen ? 1500 : 4000;
    if (started && !generating && Date.now() - lastChange > stableMs) {
      if (!text.startsWith(sent)) {
        let i = 0;
        while (i < sent.length && i < text.length && sent[i] === text[i]) i++;
        log('warn', 'answer was re-rendered mid-stream; sending the remainder');
        out({ id, type: 'delta', text: text.slice(i) });
      }
      return;
    }
    await sleep(250);
  }
  throw new Error('answer timed out');
}

async function handle(req) {
  try {
    if (req.op === 'chat') {
      await chat(req.id, String(req.prompt ?? ''), Number(req.timeout_ms) || 300000);
      out({ id: req.id, type: 'done' });
    } else if (req.op === 'login') {
      await ensureSession(true);
      out({ id: req.id, type: 'done' });
    } else {
      out({ id: req.id, type: 'error', message: `unknown op ${req.op}` });
    }
  } catch (e) {
    const message = String(e?.message ?? e).split('\n')[0];
    log('error', message);
    out({ id: req.id, type: 'error', message });
    await close(); // start clean next time
  }
}

let queue = Promise.resolve();
const rl = createInterface({ input: process.stdin });
rl.on('line', (line) => {
  let req;
  try {
    req = JSON.parse(line);
  } catch {
    return;
  }
  if (req.op === 'cancel') {
    cancelled = true;
    return;
  }
  queue = queue.then(() => handle(req));
});
rl.on('close', async () => {
  await close();
  process.exit(0);
});
process.on('uncaughtException', (e) => log('error', `uncaught: ${e?.message ?? e}`));
out({ type: 'ready' });
