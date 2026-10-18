// The sign-in box. Rust opens it with `signin-open` {email, hint}; Return = Sign In, Esc = Cancel.
// Cancel works while signing in too: Rust drops the request and saves nothing.
(function () {
  'use strict';
  const { listen } = window.__TAURI__.event, { invoke } = window.__TAURI__.core;
  const $ = id => document.getElementById(id);
  const box = $('box'), email = $('email'), pw = $('pw'), err = $('err');
  let busy = false;

  function setBusy(b) {
    busy = b;
    box.classList.toggle('busy', b);
    $('go').setAttribute('aria-disabled', String(b));
    $('golbl').textContent = b ? 'Signing In…' : 'Sign In';
    email.readOnly = pw.readOnly = b;
  }

  // The window is exactly the box; a two-line error makes it taller.
  const fit = show => invoke('signin_show', { h: box.getBoundingClientRect().height, show });
  const showError = msg => { err.textContent = msg; fit(false); };

  listen('signin-open', async e => {
    setBusy(false);
    email.value = e.payload.email || '';
    pw.value = '';
    showError('');
    $('hint').hidden = !e.payload.hint;
    await fit(true);
    (email.value ? pw : email).focus();
  });

  box.addEventListener('submit', async e => {
    e.preventDefault();
    if (busy) return;
    if (!email.value.trim()) return email.focus();
    if (!pw.value) return pw.focus();
    showError('');
    setBusy(true);
    try {
      await invoke('signin_submit', { email: email.value, password: pw.value });
    } catch (msg) {
      if (msg === 'cancelled') return; // the box is already closed
      setBusy(false);
      showError(msg);
      if (msg.startsWith('Wrong')) { pw.focus(); pw.select(); }
      return;
    }
    setBusy(false);
  });

  const cancel = () => { setBusy(false); invoke('signin_cancel'); };
  $('cancel').addEventListener('click', cancel);
  document.addEventListener('keydown', e => { if (e.key === 'Escape') { e.preventDefault(); cancel(); } });

  invoke('signin_ready');
})();
