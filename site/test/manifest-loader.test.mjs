import assert from 'node:assert/strict';
import test from 'node:test';
import { loadSiteData } from '../src/lib/manifest.mjs';

test('loadSiteData returns the single cc-loadout product view', async () => {
  const { marketplace, plugin } = await loadSiteData();
  assert.equal(marketplace.name, 'cc-loadout');
  assert.equal(plugin.id, 'cc-loadout');
  assert.equal(plugin.group, 'workflow');
  assert.deepEqual(plugin.skills.map(({ name }) => name).sort(), ['acquire', 'init', 'release', 'schedule']);
});

test('loadSiteData names a missing product instead of returning partial data', async () => {
  const fixture = new URL('./fixtures/no-plugin.json', import.meta.url);
  await assert.rejects(loadSiteData(fixture), /manifest has no cc-loadout plugin/);
});
