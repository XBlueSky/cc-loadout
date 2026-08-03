import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const manifestUrl = new URL('../.cc-marketspec/dist/manifest.json', import.meta.url);

test('generated marketplace data exposes the cc-loadout website contract', async () => {
  const manifest = JSON.parse(await readFile(manifestUrl, 'utf8'));
  assert.equal(manifest.schemaVersion, '1.1');
  assert.equal(manifest.marketplace.name, 'cc-loadout');

  const plugin = manifest.plugins.find(({ id }) => id === 'cc-loadout');
  assert.ok(plugin, 'cc-loadout plugin must exist');
  assert.match(plugin.tagline, /Claude Code/i);
  assert.ok(plugin.intro.length > plugin.tagline.length);
  assert.deepEqual(
    plugin.skills.map(({ name }) => name).sort(),
    ['acquire', 'init', 'release', 'schedule'],
  );
  assert.ok(plugin.skills.every(({ trigger }) => typeof trigger === 'string' && trigger.length > 20));
});
