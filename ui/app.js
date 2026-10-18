// Renders the state Rust sends (shape at the top of panel.js), reports the panel's height,
// and turns clicks and keys into `panel_action` calls. Refresh and Sign Out keep the panel open;
// every other action closes it in Rust.
(function () {
  'use strict';
  const { listen } = window.__TAURI__.event, { invoke } = window.__TAURI__.core;
  const root = document.getElementById('root');
  const SPIN_MS = 800; // one full turn of the Refresh icon (--gw-spin)
  // After Sign Out its row is gone: focus goes to Sign In…, in the same slot.
  const FOCUS_NEXT = { signout: 'signin' };
  let state = null, busySince = 0, held = 0;

  function render(s) {
    const was = state;
    state = s;
    const a = document.activeElement, focused = a && a.dataset.action;
    // The chart read-out survives a re-render: same minute, same focus (spec › Re-render while reading).
    const plot = root.querySelector('.gw-plot[data-plot]');
    const reading = plot && (plot.matches(':hover') || a === plot) && plot.dataset.at != null
      ? { at: +plot.dataset.at, kb: plot.dataset.kb, focus: a === plot } : null;
    root.innerHTML = GW.renderPanel(s);
    GW.bindChart(root);
    if (focused) {
      const b = root.querySelector(`[data-action="${focused}"]`) || root.querySelector(`[data-action="${FOCUS_NEXT[focused]}"]`);
      if (b) b.focus();
    }
    const np = root.querySelector('.gw-plot[data-plot]');
    if (reading && np) {
      if (reading.focus) { np.focus({ preventScroll: true }); if (reading.kb != null) np.dataset.kb = reading.kb; }
      GW.showAt(np, reading.at);
    }
    if (s.refreshing && !(was && was.refreshing)) busySince = Date.now();
    invoke('panel_height', { h: root.firstElementChild.getBoundingClientRect().height });
  }

  listen('state', e => {
    clearTimeout(held);
    // A quick poll still shows one full turn: hold the "done" state until SPIN_MS after the spin began.
    const wait = !e.payload.refreshing && state && state.refreshing ? busySince + SPIN_MS - Date.now() : 0;
    if (wait > 0) held = setTimeout(() => render(e.payload), wait); else render(e.payload);
  });

  // The panel opens with no row highlighted (hand-off): drop any focus left from the last time.
  // The panel only gains focus when it opens (it hides on blur), so this never fights the
  // focus restore above, which serves re-renders while it is open.
  window.addEventListener('focus', () => { if (document.activeElement) document.activeElement.blur(); });

  const enabled = () => [...root.querySelectorAll('.gw-act:not([aria-disabled="true"])')];
  const run = id => {
    if (id === 'refresh' && !(state && state.signedIn && !state.refreshing)) return; // disabled signed out; one poll at a time
    invoke('panel_action', { id });
  };

  // Return / Space on a focused row or the Refresh button is a native button click.
  root.addEventListener('click', e => {
    const b = e.target.closest('.gw-act, .gw-icon-btn');
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
