import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

for (const name of ['manifest.yml', 'site.yml']) {
  test(`${name} is read-only and targets master`, async () => {
    const yaml = await readFile(new URL(`../.github/workflows/${name}`, import.meta.url), 'utf8');
    assert.match(yaml, /branches:\s*\[master\]/);
    assert.match(yaml, /contents:\s*read/);
    assert.doesNotMatch(yaml, /contents:\s*write/);
    assert.doesNotMatch(yaml, /git push|wrangler-action|CLOUDFLARE_API_TOKEN/);
  });
}
