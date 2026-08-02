import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { access, mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';

const dist = new URL('../dist/', import.meta.url);
const execFileAsync = promisify(execFile);

test('static build publishes the page and the exact manifest', async () => {
  await access(new URL('index.html', dist));
  await access(new URL('manifest.json', dist));

  const source = JSON.parse(await readFile(new URL('../../.cc-marketspec/dist/manifest.json', import.meta.url), 'utf8'));
  const published = JSON.parse(await readFile(new URL('manifest.json', dist), 'utf8'));
  assert.deepEqual(published, source);
});

test('static page includes product social metadata', async () => {
  const html = await readFile(new URL('index.html', dist), 'utf8');
  assert.match(html, /name=["']twitter:card["'] content=["']summary_large_image["']/);
  assert.match(html, /href=["']\/logo\.png["']/);
  try {
    await access(new URL('og.png', dist));
    assert.match(html, /property=["']og:image["']/);
    assert.match(html, /name=["']twitter:image["']/);
  } catch {
    assert.doesNotMatch(html, /property=["']og:image["']/);
    assert.doesNotMatch(html, /name=["']twitter:image["']/);
  }
});

test('custom site URL produces absolute canonical page metadata', async (context) => {
  const output = await mkdtemp(join(tmpdir(), 'cc-loadout-site-'));
  context.after(() => rm(output, { recursive: true, force: true }));

  await execFileAsync(
    process.execPath,
    [fileURLToPath(new URL('../node_modules/astro/astro.js', import.meta.url)), 'build', '--outDir', output],
    {
      cwd: fileURLToPath(new URL('../', import.meta.url)),
      env: { ...process.env, SITE_URL: 'https://custom.example.test' },
    },
  );

  const html = await readFile(join(output, 'index.html'), 'utf8');
  assert.match(html, /<link rel=["']canonical["'] href=["']https:\/\/custom\.example\.test\/["']/);
  assert.match(html, /<meta property=["']og:url["'] content=["']https:\/\/custom\.example\.test\/["']/);
  assert.doesNotMatch(html, /localhost/);
});
