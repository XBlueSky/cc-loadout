export function copyFeedbackLabel(actionLabel, state) {
  const commandLabel = actionLabel.replace(/^Copy\s+/, '');

  if (state === 'success') return `Copied ${commandLabel}`;
  if (state === 'failure') return `Copy failed; ${commandLabel} selected`;
  return `Copy ${commandLabel}`;
}
