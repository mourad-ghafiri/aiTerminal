// The install command's Copy button. One delegated listener, so it works for every
// `.install` block on the page (hero + download band) and for any added later.
document.addEventListener('click', async (e) => {
  const button = e.target.closest('.install button[data-copy]');
  if (!button) return;
  const text = button.getAttribute('data-copy');
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Clipboard access can be refused (an insecure origin, a permission prompt). Fall
    // back to selecting the command so the usual ⌘C still works.
    const code = button.parentElement.querySelector('code');
    if (code) {
      const range = document.createRange();
      range.selectNodeContents(code);
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
    }
    return;
  }
  const was = button.textContent;
  button.textContent = 'Copied';
  button.classList.add('copied');
  setTimeout(() => {
    button.textContent = was;
    button.classList.remove('copied');
  }, 1600);
});
