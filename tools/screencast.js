'use strict';

const fs = require('fs');
const fsp = require('fs').promises;
const path = require('path');

// Vendored beside this script (tools/node_modules); %TEMP%\mini-cdp fallback.
let puppeteer;
try {
  puppeteer = require('puppeteer-core');
} catch (_) {
  puppeteer = require(path.join(process.env.TEMP, 'mini-cdp', 'node_modules', 'puppeteer-core'));
}

async function main() {
  const currentPath = path.join(
    process.env.LOCALAPPDATA,
    'Mini',
    'Debug',
    'current.json',
  );
  const current = JSON.parse(fs.readFileSync(currentPath, 'utf8'));
  const runDir = current.active_run_dir;
  if (!runDir) {
    console.error('current.json missing active_run_dir');
    process.exit(1);
  }

  const framesDir = path.join(runDir, 'frames');
  const jsonlPath = path.join(runDir, 'screencast.jsonl');
  await fsp.mkdir(framesDir, { recursive: true });

  const browser = await puppeteer.connect({
    browserURL: current.cdp,
    defaultViewport: null,
  });
  const page = (await browser.pages())[0];
  if (!page) {
    console.error('no open pages');
    process.exit(1);
  }

  const client = await page.createCDPSession();
  let seq = 0;
  let stopping = false;

  client.on('Page.screencastFrame', async (frame) => {
    if (stopping) {
      return;
    }
    const n = seq++;
    await fsp.writeFile(
      path.join(framesDir, n + '.jpg'),
      Buffer.from(frame.data, 'base64'),
    );
    const metadataTimestamp =
      frame.metadata && frame.metadata.timestamp != null
        ? frame.metadata.timestamp
        : null;
    await fsp.appendFile(
      jsonlPath,
      JSON.stringify({ seq: n, ts: Date.now(), metadataTimestamp }) + '\n',
    );
    await client.send('Page.screencastFrameAck', { sessionId: frame.sessionId });
  });

  const stop = async () => {
    if (stopping) {
      return;
    }
    stopping = true;
    try {
      await client.send('Page.stopScreencast');
    } catch (_) {
      /* already gone */
    }
    process.exit(0);
  };

  process.on('SIGINT', stop);

  await client.send('Page.startScreencast', {
    format: 'jpeg',
    everyNthFrame: 1,
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
