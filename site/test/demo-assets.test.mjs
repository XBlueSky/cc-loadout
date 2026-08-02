import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import sharp from 'sharp';
import { DEMO_FRAME_RATE, DEMO_STILL_FRAME, deriveDemoStill } from '../scripts/derive-demo-still.mjs';

const demoUrl = new URL('../public/demo.gif', import.meta.url);
const stillUrl = new URL('../public/demo-still.png', import.meta.url);

test('reduced-motion still deterministically comes from the representative Profile Rules frame', async () => {
  const demo = await readFile(demoUrl);
  assert.equal(createHash('sha256').update(demo).digest('hex'), 'ddb8a925a81de506f790edb73267742519da377fb4ff331ae7388f047eabcf61');

  const metadata = await sharp(demo, { animated: true }).metadata();
  assert.equal(metadata.pages, 336);
  assert.equal(DEMO_FRAME_RATE, 25);
  assert.equal(DEMO_STILL_FRAME, 225);
  assert.equal(DEMO_STILL_FRAME / DEMO_FRAME_RATE, 9);

  const temporaryDirectory = await mkdtemp(join(tmpdir(), 'cc-loadout-demo-still-'));
  const generatedPath = join(temporaryDirectory, 'demo-still.png');

  try {
    await deriveDemoStill(generatedPath);
    assert.deepEqual(await readFile(generatedPath), await readFile(stillUrl));
  } finally {
    await rm(temporaryDirectory, { recursive: true });
  }
});

test('reduced-motion still contains a settled high-contrast product interface', async () => {
  const still = await readFile(stillUrl);
  const { data, info } = await sharp(still).removeAlpha().raw().toBuffer({ resolveWithObject: true });
  const sampleOffset = ((8 * info.width) + (info.width - 8)) * 3;
  const background = [data[sampleOffset], data[sampleOffset + 1], data[sampleOffset + 2]];
  let foregroundPixels = 0;
  let lightTextPixels = 0;
  let orangeSignalPixels = 0;

  for (let offset = 0; offset < data.length; offset += 3) {
    const [red, green, blue] = data.subarray(offset, offset + 3);
    const backgroundDistance = Math.abs(red - background[0]) + Math.abs(green - background[1]) + Math.abs(blue - background[2]);
    if (backgroundDistance > 24) foregroundPixels += 1;
    if (red + green + blue > 330) lightTextPixels += 1;
    if (red > 80 && red > green * 1.15 && green > blue * 1.15) orangeSignalPixels += 1;
  }

  const totalPixels = info.width * info.height;
  assert.ok(foregroundPixels / totalPixels > 0.5, 'the TUI panel must occupy most of the settled frame');
  assert.ok(lightTextPixels / totalPixels > 0.003, 'the settled frame must contain readable interface text');
  assert.ok(orangeSignalPixels / totalPixels > 0.004, 'the settled frame must contain active orange interface signals');
});
