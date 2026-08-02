import assert from 'node:assert/strict';
import test from 'node:test';
import { copyFeedbackLabel } from '../src/scripts/copy-label.mjs';

test('copy feedback keeps the command identity in every live state', () => {
  const action = 'Copy Claude Code marketplace command';

  assert.equal(copyFeedbackLabel(action, 'idle'), action);
  assert.equal(copyFeedbackLabel(action, 'success'), 'Copied Claude Code marketplace command');
  assert.equal(copyFeedbackLabel(action, 'failure'), 'Copy failed; Claude Code marketplace command selected');
});
