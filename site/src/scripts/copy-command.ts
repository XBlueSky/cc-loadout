document.querySelectorAll<HTMLButtonElement>('[data-copy-command]').forEach((button) => {
  button.addEventListener('click', async () => {
    const command = button.dataset.copyCommand;
    if (!command) return;

    try {
      await navigator.clipboard.writeText(command);
      button.textContent = 'Copied';
      window.setTimeout(() => {
        button.textContent = 'Copy';
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
    }
  });
});
