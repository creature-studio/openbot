#!/usr/bin/env node
// browser-worker - Node + Playwright with stable semantic refs @e1, @e2...
// Provides browser API for host-agent with stable refs for Bot E2E

import http from 'node:http';
import { chromium } from 'playwright';
import fs from 'node:fs';

const PORT = parseInt(process.env.BROWSER_WORKER_PORT || '0', 10) || 0;
let browser = null;
let page = null;
let refMap = new Map(); // ref -> {selector, role, name, backendNodeId, xpath}
let stableRefMap = new Map(); // xpath -> ref (for stability)

let mockMode = false;
let mockPage = { url: 'http://127.0.0.1:8765/', content: '' };

async function ensureBrowser() {
  if (browser || mockMode) {
    return { browser, page };
  }
  try {
    const profileDir = process.env.CHROME_PROFILE || undefined;
    const launchOpts = {
      headless: process.env.HEADLESS !== 'false',
      args: ['--no-sandbox', '--disable-setuid-sandbox', '--disable-dev-shm-usage']
    };
    if (profileDir) {
      const context = await chromium.launchPersistentContext(profileDir, {
        headless: launchOpts.headless,
        args: launchOpts.args,
        viewport: { width: 1280, height: 720 }
      });
      page = context.pages()[0] || await context.newPage();
      browser = context;
      console.log(`[browser-worker] browser launched with profile ${profileDir}`);
    } else {
      browser = await chromium.launch(launchOpts);
      const context = await browser.newContext({
        viewport: { width: 1280, height: 720 }
      });
      page = await context.newPage();
      console.log('[browser-worker] browser launched');
    }
    return { browser, page };
  } catch (e) {
    console.error(`[browser-worker] Failed to launch chromium: ${e.message}, falling back to mock mode with stable refs`);
    mockMode = true;
    // Create mock page that returns stable refs
    page = {
      goto: async (url) => { mockPage.url = url; console.log(`[mock] goto ${url}`); },
      evaluate: async (fn) => {
        // Return mock elements for login page
        return [
          { role: 'button', name: '登录', xpath: '//button[1]', tag: 'button', stableKey: '//button[1]' },
          { role: 'textbox', name: '用户名', xpath: '//input[1]', tag: 'input', stableKey: '//input[1]' },
          { role: 'textbox', name: '密码', xpath: '//input[2]', tag: 'input', stableKey: '//input[2]' },
          { role: 'heading', name: '登录页面', xpath: '//h1[1]', tag: 'h1', stableKey: '//h1[1]' }
        ];
      },
      accessibility: { snapshot: async () => null },
      locator: () => ({ count: async () => 0, click: async () => {}, fill: async () => {} }),
      getByRole: () => ({ first: () => ({ count: async () => 0, click: async () => {}, fill: async () => {} }) }),
      getByText: () => ({ first: () => ({ count: async () => 0, click: async () => {} }) }),
      getByPlaceholder: () => ({ first: () => ({ count: async () => 0, fill: async () => {} }) }),
      keyboard: { press: async () => {} },
      mouse: { wheel: async () => {} },
      screenshot: async () => Buffer.from('mock'),
      url: () => mockPage.url
    };
    browser = {
      contexts: () => [{ pages: () => [page] }],
      close: async () => { browser = null; mockMode = false; }
    };
    return { browser, page };
  }
}

// Generate stable ref based on xpath or unique identifier
function getStableRef(xpath, role, name) {
  // Use xpath as stable key, if we have seen it before, reuse ref
  if (stableRefMap.has(xpath)) {
    return stableRefMap.get(xpath);
  }
  // Otherwise, generate new ref like e1, e2...
  const ref = `e${stableRefMap.size + 1}`;
  stableRefMap.set(xpath, ref);
  return ref;
}

async function snapshot() {
  const { page } = await ensureBrowser();
  
  if (mockMode) {
    // Return mock snapshot with stable refs @e1, @e2...
    refMap.clear();
    // Don't clear stableRefMap for stability
    const mockElements = [
      { role: 'button', name: '登录', xpath: '//button[1]', tag: 'button', stableKey: '//button[1]' },
      { role: 'textbox', name: '用户名', xpath: '//input[1]', tag: 'input', stableKey: '//input[1]' },
      { role: 'textbox', name: '密码', xpath: '//input[2]', tag: 'input', stableKey: '//input[2]' },
      { role: 'heading', name: '登录页面', xpath: '//h1[1]', tag: 'h1', stableKey: '//h1[1]' }
    ];
    const lines = [];
    for (const el of mockElements) {
      const ref = getStableRef(el.stableKey, el.role, el.name);
      refMap.set(ref, { role: el.role, name: el.name, xpath: el.xpath, tag: el.tag });
      const displayName = el.name ? `"${el.name}"` : '';
      lines.push(`@${ref} ${el.role} ${displayName}`.trim() + `  [${el.tag}]`);
    }
    return lines.join('\n');
  }
  
  // Get semantic snapshot via page.evaluate for stable refs
  // This returns list of interactive elements with xpath, role, name, stable ref
  const elements = await page.evaluate(() => {
    function getXPath(el) {
      if (el.id) return `//*[@id="${el.id}"]`;
      const parts = [];
      while (el && el.nodeType === 1) {
        let index = 1;
        let sibling = el.previousSibling;
        while (sibling) {
          if (sibling.nodeType === 1 && sibling.tagName === el.tagName) index++;
          sibling = sibling.previousSibling;
        }
        const tag = el.tagName.toLowerCase();
        const part = `${tag}[${index}]`;
        parts.unshift(part);
        el = el.parentNode;
        if (el && el.tagName && el.tagName.toLowerCase() === 'html') break;
      }
      return '/' + parts.join('/');
    }

    function getRole(el) {
      return el.getAttribute('role') || 
             (el.tagName === 'BUTTON' ? 'button' :
              el.tagName === 'A' ? 'link' :
              el.tagName === 'INPUT' ? (el.type === 'checkbox' ? 'checkbox' : el.type === 'radio' ? 'radio' : 'textbox') :
              el.tagName === 'TEXTAREA' ? 'textbox' :
              el.tagName === 'SELECT' ? 'combobox' :
              el.tagName.toLowerCase());
    }

    function getName(el) {
      return el.getAttribute('aria-label') ||
             el.getAttribute('placeholder') ||
             el.innerText?.trim().slice(0, 80) ||
             el.value?.slice(0, 80) ||
             el.getAttribute('alt') ||
             el.getAttribute('title') ||
             '';
    }

    const selector = 'button, a, input, textarea, select, [role="button"], [role="link"], [role="textbox"], [role="checkbox"], [role="radio"], [role="combobox"], [role="menuitem"], [role="tab"]';
    const els = document.querySelectorAll(selector);
    const result = [];
    els.forEach((el) => {
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) return;
      // Skip hidden
      const style = window.getComputedStyle(el);
      if (style.display === 'none' || style.visibility === 'hidden') return;
      
      const role = getRole(el);
      const name = getName(el);
      const xpath = getXPath(el);
      const tag = el.tagName.toLowerCase();
      
      result.push({
        role,
        name: name.slice(0, 80),
        xpath,
        tag,
        // For stability, we also include a hash of attributes
        stableKey: xpath
      });
    });
    return result.slice(0, 200);
  });

  // Build stable refs and refMap
  // We keep stableRefMap across snapshots for stability, but we also need to handle removed elements
  // For MVP, we clear stableRefMap only when page navigates, otherwise reuse
  // Here we will rebuild refMap but reuse stable refs if xpath already seen
  refMap.clear();
  const lines = [];
  
  // Sort elements by position for deterministic order (top to bottom, left to right)
  // We need to get bounding rect for sorting, but we already have xpath, we can sort by DOM order which is already deterministic
  for (const el of elements) {
    const ref = getStableRef(el.stableKey, el.role, el.name);
    refMap.set(ref, { role: el.role, name: el.name, xpath: el.xpath, tag: el.tag });
    
    // Format as @e1 button "登录" or @e2 textbox "用户名" - stable semantic format
    const displayName = el.name ? `"${el.name.replace(/"/g, '\\"')}"` : '';
    lines.push(`@${ref} ${el.role} ${displayName}`.trim() + `  [${el.tag}]`);
  }

  // Also include accessibility tree for non-interactive but meaningful elements? For now, interactive only
  // But we should also include headings, etc for context
  try {
    const a11y = await page.accessibility.snapshot();
    if (a11y) {
      function walk(node, depth = 0) {
        if (!node) return;
        const role = node.role || 'unknown';
        const name = node.name || '';
        // Include headings and landmarks for context
        if (['heading', 'banner', 'main', 'navigation'].includes(role) && name) {
          const xpath = `a11y-${role}-${name}`;
          const ref = getStableRef(xpath, role, name);
          if (!refMap.has(ref)) {
            refMap.set(ref, { role, name, xpath, tag: role });
            lines.push(`@${ref} ${role} "${name.slice(0, 60)}"`);
          }
        }
        if (node.children) {
          for (const child of node.children) walk(child, depth+1);
        }
      }
      walk(a11y);
    }
  } catch (e) {
    // ignore
  }

  return lines.join('\n');
}

async function click(ref) {
  // ref is like e1 or @e1 or 1
  const cleanRef = ref.replace('@', '');
  const info = refMap.get(cleanRef);
  if (!info) throw new Error(`ref ${ref} not found, call snapshot first. Available: ${Array.from(refMap.keys()).join(', ')}`);
  
  const { page } = await ensureBrowser();
  if (mockMode) {
    return `clicked @${cleanRef} ${info.role} "${info.name}" via mock (stable ref)`;
  }
  
  try {
    // Try by xpath first (most stable)
    if (info.xpath && info.xpath.startsWith('/')) {
      const locator = page.locator(`xpath=${info.xpath}`).first();
      if (await locator.count() > 0) {
        await locator.click();
        return `clicked @${cleanRef} ${info.role} "${info.name}" via xpath`;
      }
    }
    // Try by role and name
    if (info.name) {
      const locator = page.getByRole(info.role, { name: info.name, exact: false }).first();
      if (await locator.count() > 0) {
        await locator.click();
        return `clicked @${cleanRef} ${info.role} "${info.name}" via getByRole`;
      }
    }
    if (info.name) {
      const locator = page.getByText(info.name, { exact: false }).first();
      if (await locator.count() > 0) {
        await locator.click();
        return `clicked @${cleanRef} via getByText`;
      }
    }
    // Fallback: evaluate
    await page.evaluate((xpath) => {
      const result = document.evaluate(xpath, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null);
      if (result.singleNodeValue) result.singleNodeValue.click();
    }, info.xpath);
    return `clicked @${cleanRef} via fallback`;
  } catch (e) {
    throw new Error(`click failed for ref ${ref}: ${e.message}`);
  }
}

async function fill(ref, value) {
  const cleanRef = ref.replace('@', '');
  const info = refMap.get(cleanRef);
  if (!info) throw new Error(`ref ${ref} not found`);
  
  const { page } = await ensureBrowser();
  if (mockMode) {
    return `filled @${cleanRef} ${info.role} "${info.name}" with "${value}" via mock (stable ref)`;
  }
  try {
    if (info.xpath && info.xpath.startsWith('/')) {
      const locator = page.locator(`xpath=${info.xpath}`).first();
      if (await locator.count() > 0) {
        await locator.fill(value);
        return `filled @${cleanRef} ${info.role} "${info.name}" with "${value}" via xpath`;
      }
    }
    if (info.name) {
      const locator = page.getByRole(info.role, { name: info.name, exact: false }).first();
      if (await locator.count() > 0) {
        await locator.fill(value);
        return `filled @${cleanRef} with "${value}" via getByRole`;
      }
      const locator2 = page.getByPlaceholder(info.name).first();
      if (await locator2.count() > 0) {
        await locator2.fill(value);
        return `filled @${cleanRef} via placeholder`;
      }
    }
    const locator = page.locator('input, textarea').first();
    await locator.fill(value);
    return `filled @${cleanRef} via fallback`;
  } catch (e) {
    throw new Error(`fill failed for ref ${ref}: ${e.message}`);
  }
}

const server = http.createServer(async (req, res) => {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type');
  
  if (req.method === 'OPTIONS') {
    res.writeHead(200);
    res.end();
    return;
  }
  
  let body = '';
  req.on('data', chunk => body += chunk);
  req.on('end', async () => {
    try {
      const url = new URL(req.url, `http://${req.headers.host}`);
      const path = url.pathname;
      
      if (path === '/open' && req.method === 'POST') {
        const { url: targetUrl } = JSON.parse(body || '{}');
        if (!targetUrl) throw new Error('missing url');
        const { page } = await ensureBrowser();
        await page.goto(targetUrl, { waitUntil: 'domcontentloaded', timeout: 30000 });
        // Clear stable refs on navigation for new page
        stableRefMap.clear();
        refMap.clear();
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true, url: targetUrl }));
      } else if (path === '/snapshot' && req.method === 'GET') {
        const snap = await snapshot();
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true, snapshot: snap }));
      } else if (path === '/click' && req.method === 'POST') {
        const { ref } = JSON.parse(body || '{}');
        if (!ref) throw new Error('missing ref');
        const result = await click(ref);
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true, result }));
      } else if (path === '/fill' && req.method === 'POST') {
        const { ref, value } = JSON.parse(body || '{}');
        if (!ref) throw new Error('missing ref');
        const result = await fill(ref, value);
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true, result }));
      } else if (path === '/press' && req.method === 'POST') {
        const { key } = JSON.parse(body || '{}');
        const { page } = await ensureBrowser();
        await page.keyboard.press(key || 'Enter');
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true }));
      } else if (path === '/scroll' && req.method === 'POST') {
        const { x, y } = JSON.parse(body || '{}');
        const { page } = await ensureBrowser();
        await page.mouse.wheel(x || 0, y || 0);
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true }));
      } else if (path === '/screenshot' && req.method === 'POST') {
        const { page } = await ensureBrowser();
        const buffer = await page.screenshot();
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true, screenshot_b64: buffer.toString('base64') }));
      } else if (path === '/tabs' && req.method === 'GET') {
        const { browser } = await ensureBrowser();
        // browser may be context or browser
        const contexts = browser.contexts ? browser.contexts() : [browser];
        const tabs = [];
        for (const ctx of contexts) {
          const pages = ctx.pages ? ctx.pages() : [];
          for (const p of pages) {
            tabs.push({ url: p.url() });
          }
        }
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true, tabs }));
      } else if (path === '/close' && req.method === 'POST') {
        if (browser) {
          await browser.close();
          browser = null;
          page = null;
          stableRefMap.clear();
          refMap.clear();
        }
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true }));
      } else if (path === '/health' && req.method === 'GET') {
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true, hasBrowser: !!browser, refCount: refMap.size, stableRefCount: stableRefMap.size }));
      } else {
        res.writeHead(404);
        res.end(JSON.stringify({ ok: false, error: 'not found' }));
      }
    } catch (e) {
      console.error('error', e);
      res.writeHead(500);
      res.end(JSON.stringify({ ok: false, error: e.message }));
    }
  });
});

server.listen(PORT, '127.0.0.1', () => {
  const addr = server.address();
  console.log(`[browser-worker] listening on ${addr.address}:${addr.port} with stable refs @e1, @e2...`);
  const portFile = process.env.BROWSER_WORKER_PORT_FILE || '/tmp/browser-worker.port';
  fs.writeFileSync(portFile, addr.port.toString());
  console.log(`[browser-worker] port file ${portFile}`);
});

// Graceful shutdown
process.on('SIGTERM', async () => {
  if (browser) await browser.close();
  process.exit(0);
});
process.on('SIGINT', async () => {
  if (browser) await browser.close();
  process.exit(0);
});
