import { copyFeedbackLabel } from './copy-label.mjs';

document.querySelectorAll<HTMLButtonElement>('[data-copy-command]').forEach((button) => {
  button.addEventListener('click', async () => {
    const command = button.dataset.copyCommand;
    if (!command) return;
    const actionLabel = button.getAttribute('aria-label') ?? 'Copy command';

    try {
      await navigator.clipboard.writeText(command);
      button.textContent = 'Copied';
      button.setAttribute('aria-label', copyFeedbackLabel(actionLabel, 'success'));
      window.setTimeout(() => {
        button.textContent = 'Copy';
        button.setAttribute('aria-label', copyFeedbackLabel(actionLabel, 'idle'));
      }, 1500);
    } catch {
      const code = button.previousElementSibling;
      if (!(code instanceof HTMLElement)) return;

      const range = document.createRange();
      range.selectNodeContents(code);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
      button.textContent = 'Copy failed — selected';
      button.setAttribute('aria-label', copyFeedbackLabel(actionLabel, 'failure'));
    }
  });
});
