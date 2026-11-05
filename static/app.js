// Small progressive-enhancement helpers: copy buttons, dropdowns, like toggles.
document.addEventListener('click', function (e) {
  const copyEl = e.target.closest('[data-copy]');
  if (copyEl) {
    const text = copyEl.getAttribute('data-copy');
    if (navigator.clipboard) navigator.clipboard.writeText(text);
    const old = copyEl.innerHTML;
    copyEl.innerHTML = '✓ copied';
    setTimeout(() => { copyEl.innerHTML = old; }, 900);
    if (copyEl.tagName === 'A') e.preventDefault();
  }
  const dd = e.target.closest('.dropdown');
  document.querySelectorAll('.dropdown.open').forEach(d => { if (d !== dd) d.classList.remove('open'); });
  if (dd && e.target.closest('[tabindex="0"]')) { dd.classList.toggle('open'); e.preventDefault(); }
});

// Branch selector: filter the branch list.
document.querySelectorAll('.dropdown .menu').forEach(menu => {
  const n = menu.querySelectorAll('a').length;
  if (n > 12) {
    const input = document.createElement('input');
    input.className = 'input';
    input.placeholder = 'Filter...';
    input.style.margin = '0.25rem';
    input.style.width = 'calc(100% - 0.5rem)';
    menu.prepend(input);
    input.addEventListener('input', () => {
      const q = input.value.toLowerCase();
      menu.querySelectorAll('a').forEach(a => { a.style.display = a.textContent.toLowerCase().includes(q) ? '' : 'none'; });
    });
  }
});
