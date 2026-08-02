import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';
import test from 'node:test';

const dist = new URL('../dist/', import.meta.url);

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
