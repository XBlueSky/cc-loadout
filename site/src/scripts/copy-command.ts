import { bindCopyButton } from './copy-control.mjs';

document.querySelectorAll<HTMLButtonElement>('[data-copy-command]').forEach((button) => {
  bindCopyButton(button, {
    writeText: (command) => navigator.clipboard.writeText(command),
    scheduleRestore: (restore) => window.setTimeout(restore, 1500),
    selectAdjacentCode: (currentButton) => {
      const code = currentButton.previousElementSibling;
      if (!(code instanceof HTMLElement)) return;

      const range = document.createRange();
      range.selectNodeContents(code);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
    },
  });
});
