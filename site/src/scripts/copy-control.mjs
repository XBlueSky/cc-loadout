import { copyFeedbackLabel } from './copy-label.mjs';

export function bindCopyButton(button, { writeText, scheduleRestore, selectAdjacentCode }) {
  const command = button.dataset.copyCommand;
  const actionLabel = button.getAttribute('aria-label') ?? 'Copy command';

  const handleClick = async () => {
    if (!command) return;

    try {
      await writeText(command);
      button.textContent = 'Copied';
      button.setAttribute('aria-label', copyFeedbackLabel(actionLabel, 'success'));
      scheduleRestore(() => {
        button.textContent = 'Copy';
        button.setAttribute('aria-label', copyFeedbackLabel(actionLabel, 'idle'));
      });
    } catch {
      selectAdjacentCode(button);
      button.textContent = 'Copy failed — selected';
      button.setAttribute('aria-label', copyFeedbackLabel(actionLabel, 'failure'));
    }
  };

  button.addEventListener('click', handleClick);
  return handleClick;
}
