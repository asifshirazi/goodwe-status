// Renders the state Rust sends (shape at the top of panel.js), reports the panel's height,
// and turns clicks and keys into `panel_action` calls. Every action closes the panel in Rust.
(function () {
  'use strict';
  const { listen } = window.__TAURI__.event, { invoke } = window.__TAURI__.core;
  const root = document.getElementById('root');
  let state = null;

  listen('state', e => {
    state = e.payload;
    const focused = document.activeElement && document.activeElement.dataset.action;
    root.innerHTML = GW.renderPanel(state);
    if (focused) { const b = root.querySelector(`[data-action="${focused}"]`); if (b) b.focus(); }
    invoke('panel_height', { h: root.firstElementChild.getBoundingClientRect().height });
  });

  // The panel opens with no row highlighted (hand-off): drop any focus left from the last time.
  // The panel only gains focus when it opens (it hides on blur), so this never fights the
  // focus restore above, which serves re-renders while it is open.
  window.addEventListener('focus', () => { if (document.activeElement) document.activeElement.blur(); });

  const enabled = () => [...root.querySelectorAll('.gw-act:not([aria-disabled="true"])')];
  const run = id => {
    if (id === 'refresh' && !(state && state.signedIn)) return; // disabled when signed out
    invoke('panel_action', { id });
  };

  // Return on a focused row is a native button click.
  root.addEventListener('click', e => {
    const b = e.target.closest('.gw-act');
    if (b && b.getAttribute('aria-disabled') !== 'true') run(b.dataset.action);
  });

  document.addEventListener('keydown', e => {
    const k = e.key.toLowerCase();
    if (e.metaKey && (k === 'r' || k === 'q')) { e.preventDefault(); run(k === 'r' ? 'refresh' : 'quit'); return; }
    if (e.key === 'Escape') { e.preventDefault(); invoke('panel_action', { id: 'close' }); return; }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const rows = enabled(), i = rows.indexOf(document.activeElement), n = rows.length;
    if (!n) return;
    const down = e.key === 'ArrowDown';
    rows[i < 0 ? (down ? 0 : n - 1) : (i + (down ? 1 : n - 1)) % n].focus();
  });
})();
