import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import test from 'node:test';

const run = promisify(execFile);
const logoUrl = new URL('../public/logo.png', import.meta.url);
const markUrl = new URL('../public/brand-mark.png', import.meta.url);
const deriveScriptUrl = new URL('../scripts/derive-brand-mark.mjs', import.meta.url);

function dimensions(png) {
  assert.equal(png.subarray(1, 4).toString(), 'PNG');
  return { width: png.readUInt32BE(16), height: png.readUInt32BE(20) };
}

test('supplied logo sheet remains byte-identical', async () => {
  const logo = await readFile(logoUrl);
  assert.equal(createHash('sha256').update(logo).digest('hex'), 'f688108e2f2a00d8d9c880fde4519e09c83648a5b7d155f53724f8d0ffc06109');
  assert.deepEqual(dimensions(logo), { width: 1254, height: 1254 });
});

test('compact app mark is a deterministic 256 pixel crop', async () => {
  const committedMark = await readFile(markUrl);
  assert.deepEqual(dimensions(committedMark), { width: 256, height: 256 });

  const temporaryDirectory = await mkdtemp(join(tmpdir(), 'cc-loadout-brand-mark-'));
  const generatedPath = join(temporaryDirectory, 'brand-mark.png');

  try {
    await run(process.execPath, [deriveScriptUrl.pathname, generatedPath]);
    assert.deepEqual(await readFile(generatedPath), committedMark);
  } finally {
    await rm(temporaryDirectory, { recursive: true });
  }
});
