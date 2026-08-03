import { readFile } from 'node:fs/promises';

const defaultManifestUrl = new URL('../../../.cc-marketspec/dist/manifest.json', import.meta.url);

export async function loadSiteData(manifestUrl = defaultManifestUrl) {
  const manifest = JSON.parse(await readFile(manifestUrl, 'utf8'));
  const plugin = manifest.plugins?.find(({ id }) => id === 'cc-loadout');

  if (!plugin) {
    throw new Error('manifest has no cc-loadout plugin');
  }
  if (!plugin.tagline || !plugin.intro) {
    throw new Error('cc-loadout manifest entry requires tagline and intro');
  }

  return { marketplace: manifest.marketplace, plugin };
}
