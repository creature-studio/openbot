#!/usr/bin/env node
// browser-worker - Node + Playwright
// Provides simple browser API for host-agent
// API: HTTP server with JSON
// Endpoints:
//   POST /open {url}
//   GET /snapshot
//   POST /click {ref}
//   POST /fill {ref, value}
//   POST /press {key}
//   POST /scroll {x, y}
//   GET /tabs
//   POST /screenshot
//   POST /close

import http from 'node:http';
import { chromium } from 'playwright';

const PORT = parseInt(process.env.BROWSER_WORKER_PORT || '0', 10) || 0; // 0 = random
let browser = null;
let page = null;
let refMap = new Map(); // ref -> locator info
let nextRef = 1;

async function ensureBrowser() {
  if (!browser) {
    browser = await chromium.launch({
      headless: true,
      args: ['--no-sandbox', '--disable-setuid-sandbox', '--disable-dev-shm-usage']
    });
    const context = await browser.newContext();
    page = await context.newPage();
    console.log('[browser-worker] browser launched');
  }
  return { browser, page };
}

async function snapshot() {
  const { page } = await ensureBrowser();
  // Use accessibility snapshot + custom JS to build refs
  // Playwright's accessibility.snapshot gives tree, but we want simple list with refs
  const snapshot = await page.accessibility.snapshot();
  refMap.clear();
  nextRef = 1;
  const lines = [];
  
  function walk(node, depth = 0) {
    if (!node) return;
    const role = node.role || 'unknown';
    const name = node.name || '';
    // Skip generic containers with no name and no children meaningful? Keep some
    if (role === 'generic' && !name && (!node.children || node.children.length === 0)) return;
    
    // Only include interactive or meaningful elements
    const interactiveRoles = ['button', 'link', 'textbox', 'checkbox', 'radio', 'combobox', 'menuitem', 'tab', 'searchbox'];
    const hasName = name && name.trim().length > 0;
    const isInteractive = interactiveRoles.includes(role) || node.children?.some(c => interactiveRoles.includes(c.role));
    
    // For MVP, include all with role and name, plus some without name but interactive
    if (hasName || isInteractive || depth < 2) {
      const ref = nextRef++;
      refMap.set(ref.toString(), { role, name, node });
      const indent = '  '.repeat(Math.min(depth, 5));
      const displayName = name ? `"${name}"` : '';
      lines.push(`[${ref}] ${indent}${role} ${displayName}`.trim());
    }
    
    if (node.children) {
      for (const child of node.children) {
        walk(child, depth + 1);
      }
    }
  }
  
  walk(snapshot);
  
  // Also try to get more via page evaluation for better refs
  try {
    const extra = await page.evaluate(() => {
      const elements = [];
      const selector = 'button, a, input, textarea, select, [role="button"], [role="link"]';
      document.querySelectorAll(selector).forEach((el, idx) => {
        const rect = el.getBoundingClientRect();
        if (rect.width === 0 && rect.height === 0) return;
        const role = el.getAttribute('role') || el.tagName.toLowerCase();
        const name = el.innerText?.slice(0, 50) || el.getAttribute('aria-label') || el.getAttribute('placeholder') || el.value || '';
        elements.push({ role, name: name.slice(0, 50), tag: el.tagName });
      });
      return elements.slice(0, 100);
    });
    
    // Merge extra if accessibility snapshot was sparse
    if (lines.length < 10 && extra.length > 0) {
      for (const el of extra) {
        if (lines.length >= 100) break;
        const ref = nextRef++;
        refMap.set(ref.toString(), { role: el.role, name: el.name, tag: el.tag });
        lines.push(`[${ref}] ${el.role} "${el.name}"`);
      }
    }
  } catch (e) {
    console.error('evaluate failed', e);
  }
  
  return lines.join('\n');
}

async function click(ref) {
  const info = refMap.get(ref.toString());
  if (!info) throw new Error(`ref ${ref} not found, call snapshot first`);
  
  const { page } = await ensureBrowser();
  
  // Try to click by role and name
  try {
    // Try accessibility first
    if (info.name) {
      const locator = page.getByRole(info.role, { name: info.name, exact: false }).first();
      if (await locator.count() > 0) {
        await locator.click();
        return `clicked [${ref}] ${info.role} "${info.name}" via getByRole`;
      }
    }
    // Fallback: click via text
    if (info.name) {
      const locator = page.getByText(info.name, { exact: false }).first();
      if (await locator.count() > 0) {
        await locator.click();
        return `clicked [${ref}] via getByText`;
      }
    }
    // Fallback: evaluate click at position or via JS
    await page.evaluate((ref) => {
      // This is placeholder, real implementation would need more precise mapping
      document.querySelectorAll('button, a, input').forEach((el, idx) => {
        if (idx === parseInt(ref)-1) el.click();
      });
    }, ref);
    return `clicked [${ref}] via fallback`;
  } catch (e) {
    throw new Error(`click failed for ref ${ref}: ${e.message}`);
  }
}

async function fill(ref, value) {
  const info = refMap.get(ref.toString());
  if (!info) throw new Error(`ref ${ref} not found`);
  
  const { page } = await ensureBrowser();
  try {
    if (info.name) {
      const locator = page.getByRole(info.role, { name: info.name, exact: false }).first();
      if (await locator.count() > 0) {
        await locator.fill(value);
        return `filled [${ref}] with "${value}"`;
      }
      const locator2 = page.getByPlaceholder(info.name).first();
      if (await locator2.count() > 0) {
        await locator2.fill(value);
        return `filled [${ref}] via placeholder`;
      }
    }
    // Fallback: find input
    const locator = page.locator('input, textarea').nth(parseInt(ref)-1);
    await locator.fill(value);
    return `filled [${ref}] via nth`;
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
        const contexts = browser.contexts();
        const tabs = [];
        for (const ctx of contexts) {
          const pages = ctx.pages();
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
        }
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true }));
      } else if (path === '/health' && req.method === 'GET') {
        res.writeHead(200);
        res.end(JSON.stringify({ ok: true, hasBrowser: !!browser }));
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
  console.log(`[browser-worker] listening on ${addr.address}:${addr.port}`);
  // Write port to file for discovery
  const fs = awaitImport('fs');
  // Use dynamic import for fs
  import('fs').then(fs => {
    const portFile = process.env.BROWSER_WORKER_PORT_FILE || '/tmp/browser-worker.port';
    fs.writeFileSync(portFile, addr.port.toString());
  });
});

function awaitImport(name) { return import(name); }

// Graceful shutdown
process.on('SIGTERM', async () => {
  if (browser) await browser.close();
  process.exit(0);
});
process.on('SIGINT', async () => {
  if (browser) await browser.close();
  process.exit(0);
});
