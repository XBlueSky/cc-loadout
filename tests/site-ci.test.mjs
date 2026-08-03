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

for (const lockfile of ['package-lock.json', 'site/package-lock.json']) {
  test(`${lockfile} uses no private registry URLs`, async () => {
    const contents = await readFile(new URL(`../${lockfile}`, import.meta.url), 'utf8');
    assert.doesNotMatch(contents, /npm\.synology\.inc/);
  });
}

test('README documents actionable Cloudflare Pages environment variables', async () => {
  const readme = await readFile(new URL('../README.md', import.meta.url), 'utf8');
  assert.match(
    readme,
    /- Environment variables \(Cloudflare Pages dashboard\):\n  - `NODE_VERSION=22`\n  - `SITE_URL=https:\/\/cc-loadout\.pages\.dev` \(replace with the canonical custom domain when one is connected\)/,
  );
});
