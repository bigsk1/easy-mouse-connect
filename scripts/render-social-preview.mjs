// Render the editable social card without starting the app or using a real profile.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { chromium } from 'playwright';

const source = fileURLToPath(new URL('../docs/images/social-preview.svg', import.meta.url));
const destination = fileURLToPath(new URL('../docs/images/social-preview.png', import.meta.url));
const executablePath = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH || undefined;
const browser = await chromium.launch({ headless: true, executablePath });
try {
  const page = await browser.newPage({
    viewport: { width: 1280, height: 640 }, deviceScaleFactor: 1, reducedMotion: 'reduce',
  });
  // The source is self-contained; rendering must never fetch remote content.
  await page.route(/^https?:/, (route) => route.abort());
  await page.goto(pathToFileURL(source).href, { waitUntil: 'load' });
  await page.evaluate(() => document.fonts.ready);
  await page.locator('svg').screenshot({ path: destination, omitBackground: false });
  const png = await readFile(destination);
  assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
  assert.equal(png.readUInt32BE(16), 1280);
  assert.equal(png.readUInt32BE(20), 640);
  assert.ok(png.length < 1_000_000, 'GitHub requires a social preview smaller than 1 MB.');
  console.log(`Rendered 1280 × 640 PNG (${png.length.toLocaleString('en-US')} bytes): ${destination}`);
} finally {
  await browser.close();
}
