#!/usr/bin/env node
// browse: a headless Chromium for office workers, to see what they build.
// Installed in every worker's VM at /usr/local/bin/browse by the daemon;
// Playwright and its Chromium come with the office image.
//
//   browse shot URL FILE.png [--width 1280] [--height 800] [--full]
//   browse text URL                visible text, after the page's scripts ran
//   browse html URL                the rendered DOM
//   browse console URL             console messages and page errors
//
// Network goes through the firm's policy like everything else: the firm's
// product (http://apps.<domain>) works; other sites need an officer's
// approval. For anything more (clicks, forms, several steps), write a
// Playwright script: `require('playwright')` works in node.

'use strict';
process.env.PLAYWRIGHT_BROWSERS_PATH = process.env.PLAYWRIGHT_BROWSERS_PATH || '/opt/ms-playwright';

const USAGE = `usage: browse shot URL FILE.png [--width N] [--height N] [--full]
       browse text URL | browse html URL | browse console URL`;

function args(argv) {
  const pos = [], opt = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--full') opt.full = true;
    else if (a === '--width' || a === '--height') opt[a.slice(2)] = Number(argv[++i]);
    else pos.push(a);
  }
  return { pos, opt };
}

async function main() {
  const { pos, opt } = args(process.argv.slice(2));
  const [cmd, url, file] = pos;
  if (!cmd || !url || (cmd === 'shot' && !file) || !['shot', 'text', 'html', 'console'].includes(cmd)) {
    console.error(USAGE);
    process.exit(2);
  }
  let chromium;
  try {
    ({ chromium } = require('/usr/local/lib/node_modules/playwright'));
  } catch (e) {
    console.error('browse: Playwright is not installed in this image (make image IMAGE=office)');
    process.exit(1);
  }
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: opt.width || 1280, height: opt.height || 800 } });
    const log = [];
    page.on('console', (m) => log.push(`console.${m.type()}: ${m.text()}`));
    page.on('pageerror', (e) => log.push(`pageerror: ${e.message}`));
    page.on('requestfailed', (r) => log.push(`requestfailed: ${r.method()} ${r.url()} ${r.failure() ? r.failure().errorText : ''}`));
    const resp = await page.goto(url, { waitUntil: 'networkidle', timeout: 30000 });
    const status = resp ? resp.status() : 0;
    if (cmd === 'shot') {
      await page.screenshot({ path: file, fullPage: !!opt.full });
      console.log(`${file}: ${url} (HTTP ${status}, ${opt.width || 1280}x${opt.height || 800}${opt.full ? ', full page' : ''})`);
    } else if (cmd === 'text') {
      console.log(await page.innerText('body'));
    } else if (cmd === 'html') {
      console.log(await page.content());
    } else {
      console.log(`HTTP ${status}`);
      for (const l of log) console.log(l);
      if (!log.length) console.log('no console messages or errors');
    }
  } finally {
    await browser.close();
  }
}

main().catch((e) => {
  console.error('browse: ' + (e && e.message ? e.message.split('\n')[0] : e));
  process.exit(1);
});
