import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const htmlUrl = new URL('../dist/index.html', import.meta.url);

test('homepage renders the approved product story and honest install flow', async () => {
  const html = await readFile(htmlUrl, 'utf8');
  for (const id of ['hero', 'problem', 'profiles', 'visual-control', 'install']) {
    assert.match(html, new RegExp(`id=["']${id}["']`));
  }
  assert.match(html, /One plugin\. Every loadout\./);
  assert.match(html, /Your tools shouldn.t follow you everywhere\./);
  assert.match(html, /Every repo gets the right kit\./);
  assert.match(html, /Claude guides it\. The TUI makes it visible\./);
  assert.match(html, /Install the CLI engine/);
  assert.match(html, /Install the Claude Code plugin/);
  assert.match(html, /\/plugin marketplace add https:\/\/github\.com\/xbluesky\/cc-loadout/i);
  assert.match(html, /\/plugin install cc-loadout@cc-loadout/);
});

test('homepage keeps page landmarks outside main and announces copy feedback', async () => {
  const html = await readFile(htmlUrl, 'utf8');
  assert.match(html, /<header\b[\s\S]*?<\/header>\s*<main id=["']main["']>/);
  assert.match(html, /<\/main>\s*<footer\b/);

  const copyButtons = html.match(/<button\b[^>]*data-copy-command[^>]*>/g) ?? [];
  assert.equal(copyButtons.length, 3);
  assert.ok(copyButtons.every((button) => /aria-live=["']polite["']/.test(button)));
});
