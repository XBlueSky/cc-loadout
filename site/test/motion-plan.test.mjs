import assert from 'node:assert/strict';
import test from 'node:test';
import { getMotionMode, setMotionMode, watchMotionPreference } from '../src/scripts/motion-plan.mjs';

test('reduced motion always wins', () => {
  assert.equal(getMotionMode({ reduced: true, width: 1440 }), 'reduced');
});

test('small screens use the mobile choreography', () => {
  assert.equal(getMotionMode({ reduced: false, width: 767 }), 'mobile');
});

test('wide screens use the cinematic choreography', () => {
  assert.equal(getMotionMode({ reduced: false, width: 1280 }), 'desktop');
});

test('active media choreography keeps the document mode in sync', () => {
  const root = { dataset: { motion: 'desktop' } };

  setMotionMode(root, 'mobile');

  assert.equal(root.dataset.motion, 'mobile');
});

test('motion preference changes reinitialize until cleanup', () => {
  const preference = new EventTarget();
  let changes = 0;
  const unwatch = watchMotionPreference(preference, () => changes += 1);

  preference.dispatchEvent(new Event('change'));
  unwatch();
  preference.dispatchEvent(new Event('change'));

  assert.equal(changes, 1);
});
