// Small progressive-enhancement helpers: copy buttons, dropdowns, like toggles.

// The "Use this <type>" menu is position:fixed (see style4.css) so the .tabs
// scroll container can neither clip it nor scroll under it. Anchor it to its
// button, keep it inside the viewport, flip it above when short on room below.
function placeUseMenu(dd) {
  const menu = dd.querySelector('.use-menu');
  if (!menu || getComputedStyle(menu).display === 'none') return;
  const pad = 8, gap = 6;
  menu.style.visibility = 'hidden';
  const r = dd.getBoundingClientRect();
  const w = menu.offsetWidth, h = menu.offsetHeight;
  const left =  Math.max(pad, Math.min(r.right - w, window.innerWidth - w - pad));
  let top = r.bottom + gap;
  if (top + h > window.innerHeight - pad) {
    const above = r.top - gap - h;
    top = above >= pad ? above : Math.max(pad, window.innerHeight - h - pad);
  }
  menu.style.left = left + 'px';
  menu.style.top = top + 'px';
  menu.style.visibility = '';
}

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
  if (dd && e.target.closest('[tabindex="0"]')) {
    if (dd.classList.toggle('open')) placeUseMenu(dd);
    e.preventDefault();
  }
  // :focus-within also shows a menu, so a blurred dropdown would stay open after
  // "click outside" or a second click on the trigger. Drop the focus as well.
  const ae = document.activeElement;
  const focused = ae && ae.closest ? ae.closest('.dropdown') : null;
  if (focused && !focused.classList.contains('open')) focused.blur();
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

// Keep the "Use this <type>" popover anchored while the page scrolls or resizes,
// and let Escape dismiss any dropdown (:focus-within re-opens it, so blur too).
document.addEventListener('focusin', e => {
  const dd = e.target.closest('.dropdown');
  if (dd) requestAnimationFrame(() => placeUseMenu(dd));
});
document.addEventListener('scroll', () => document.querySelectorAll('.dropdown.open').forEach(placeUseMenu), true);
window.addEventListener('resize', () => document.querySelectorAll('.dropdown.open').forEach(placeUseMenu));
document.addEventListener('keydown', e => {
  if (e.key !== 'Escape') return;
  document.querySelectorAll('.dropdown.open').forEach(d => d.classList.remove('open'));
  const el = document.activeElement;
  if (el && el.closest && el.closest('.dropdown')) el.blur();
});
