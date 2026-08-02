import assert from 'node:assert/strict';
import test from 'node:test';
import { bindCopyButton } from '../src/scripts/copy-control.mjs';

function createButton(label = 'Copy Claude Code marketplace command') {
  const attributes = new Map([['aria-label', label]]);
  return {
    dataset: { copyCommand: '/plugin marketplace add https://github.com/XBlueSky/cc-loadout' },
    textContent: 'Copy',
    addEventListener(_type, listener) { this.listener = listener; },
    getAttribute(name) { return attributes.get(name) ?? null; },
    setAttribute(name, value) { attributes.set(name, value); },
  };
}

test('rapid repeat clicks keep the original command label', async () => {
  const button = createButton();
  const restores = [];
  const handleClick = bindCopyButton(button, {
    writeText: async () => {},
    scheduleRestore: (restore) => restores.push(restore),
    selectAdjacentCode: () => assert.fail('selection fallback must not run'),
  });

  await handleClick();
  assert.equal(button.getAttribute('aria-label'), 'Copied Claude Code marketplace command');
  await handleClick();
  assert.equal(button.getAttribute('aria-label'), 'Copied Claude Code marketplace command');

  restores.forEach((restore) => restore());
  assert.equal(button.getAttribute('aria-label'), 'Copy Claude Code marketplace command');
});

test('a successful retry after failure restores the original command identity', async () => {
  const button = createButton();
  let writeAttempt = 0;
  let selectionCount = 0;
  const handleClick = bindCopyButton(button, {
    writeText: async () => {
      writeAttempt += 1;
      if (writeAttempt === 1) throw new Error('clipboard denied');
    },
    scheduleRestore: () => {},
    selectAdjacentCode: () => { selectionCount += 1; },
  });

  await handleClick();
  assert.equal(button.textContent, 'Copy failed — selected');
  assert.equal(button.getAttribute('aria-label'), 'Copy failed; Claude Code marketplace command selected');
  assert.equal(selectionCount, 1);

  await handleClick();
  assert.equal(button.textContent, 'Copied');
  assert.equal(button.getAttribute('aria-label'), 'Copied Claude Code marketplace command');
});
