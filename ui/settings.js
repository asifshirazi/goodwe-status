/* The Settings window. Rust sends `settings-open` / `settings-state` (shape: hand-off spec › State and actions, copy already
   formatted); GWS.renderSettings(state) returns the window's HTML; every control answers with `settings_action`.
   Two panes behind a noncustomizable toolbar (General, Automation); the native title bar shows the pane's name. */
(function (root) {
  'use strict';
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const DC = ['Central Europe', 'Western Europe', 'Western America', 'Eastern America', 'China', 'India'];
  const S = 'fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"';
  const MINUS = `<svg viewBox="0 0 14 14" ${S}><circle cx="7" cy="7" r="5.8"/><path d="M4.4 7h5.2"/></svg>`;
  // Toolbar glyphs, 22 pt, same stroke family as the panel's icons (gear = the panel's Settings… icon).
  const GEAR = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"><path d="M14.22 7.00L14.22 9.00L12.77 9.13L12.17 10.57L13.11 11.69L11.69 13.11L10.57 12.17L9.13 12.77L9.00 14.22L7.00 14.22L6.87 12.77L5.43 12.17L4.31 13.11L2.89 11.69L3.83 10.57L3.23 9.13L1.78 9.00L1.78 7.00L3.23 6.87L3.83 5.43L2.89 4.31L4.31 2.89L5.43 3.83L6.87 3.23L7.00 1.78L9.00 1.78L9.13 3.23L10.57 3.83L11.69 2.89L13.11 4.31L12.17 5.43L12.77 6.87Z"/><circle cx="8" cy="8" r="2.1"/></svg>';
  const BOLT = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round"><path d="M9.2 1.6L3.6 9.1h4.1l-1 5.3 5.7-7.6H8.3z"/></svg>';
  const PANES = [['general', 'General', GEAR], ['automation', 'Automation', BOLT]];
  // Delay pop-up (seconds → label). One value per device, used both ways; 0 = at once.
  const DELAYS = [[0, 'Instant'], [30, '30 s'], [60, '1 min'], [120, '2 min'], [300, '5 min'], [600, '10 min'], [900, '15 min'], [1800, '30 min']];
  const sw = (k, on, label, off) => `<button type="button" class="switch" role="switch" data-k="${k}" data-action="${k}" aria-checked="${on}" aria-label="${esc(label)}"${off ? ' aria-disabled="true"' : ''}><i></i></button>`;
  const btn = (action, text, def) => `<button type="button" class="btn${def ? ' def' : ''}" data-k="${action}" data-action="${action}">${text}</button>`;

  // ---- General pane ----
  // Signed out or login failed: Sign In… is a normal button here (one default button per window at most, D-Y4).
  function account(a) {
    const body = a.signedIn
      ? `<div class="lbl"><span>SEMS Portal</span><small class="one" title="${esc(a.email || '')}">${a.email ? 'Signed in as ' + esc(a.email) : 'Signed in'}</small></div>${btn('signout', 'Sign Out')}`
      : `<div class="lbl"><span>SEMS Portal</span><small>Not signed in. Sign in to see your battery and grid.</small></div>${btn('signin', 'Sign In…')}`;
    return `<h2 class="first">Account</h2><section class="grp"><div class="row">${body}</div></section>`;
  }
  const startup = g => `<h2>Startup</h2><section class="grp"><div class="row"><div class="lbl"><span>Open at login</span><small>Start GoodWe Status in the menu bar when you log in.</small></div>${sw('login', g.openAtLogin, 'Open at login')}</div></section>`;
  // T-008: one alert at the setting and one at half of it (Rust: rules::low_battery, integer half), only while the grid is off.
  const LOW = [5, 10, 15, 20, 25, 30, 35, 40, 45, 50];
  const notifications = g => {
    const p = g.lowBattery || 20, opts = LOW.includes(p) ? LOW : [...LOW, p].sort((a, b) => a - b);
    return `<h2>Notifications</h2><section class="grp"><div class="row"><div class="lbl"><label for="low">Low battery alert</label><small>While the grid is off, notify at ${p}% and again at ${Math.floor(p / 2)}%.</small></div>`
      + `<select id="low" data-k="low">${opts.map(v => `<option value="${v}"${v === p ? ' selected' : ''}>${v}%</option>`).join('')}</select></div></section>`;
  };
  const troubleshooting = () => `<h2>Troubleshooting</h2><section class="grp"><div class="row"><div class="lbl"><span>Log folder</span><small>The app’s logs, for reporting a problem.</small></div>${btn('logs', 'Open Log Folder')}</div></section>`;

  // ---- Automation pane ----
  // Change 3: no switch at the top, one line of intro (or the helper); each device row has its own switch.
  const automation = a => `<p class="intro">${esc(a.hint || 'Devices that are on change when the grid goes off or comes back, each after its own delay.')}</p>`;
  function devices(s) {
    const rows = s.devices.map((d, i) => {
      // Status line, first match: in flight (spinner) · waiting out its delay · offline · last result.
      const status = d.setting ? `<small role="status"><span class="spin" aria-hidden="true"></span>${esc(d.setting)}</small>`
        : d.waiting ? `<small role="status">${esc(d.waiting)}</small>`
        : d.offline ? `<small class="err">Offline in Tuya</small>` : d.last ? `<small${d.last.failed ? ' class="err"' : ''}>${esc(d.last.text)}</small>` : '';
      const sel = (which, label, pos) => `<select data-k="${which}-${i}" data-device="${i}" data-which="${which}" aria-label="${label}, ${esc(d.name)}">${d.choices.map((c, k) => `<option value="${k}"${k === pos ? ' selected' : ''}>${esc(c)}</option>`).join('')}</select>`;
      // D-Y13: a saved delay that isn't in the list (an edited file) gets its own item, in order, so the pop-up never shows a value that isn't saved.
      const ds = d.delay_s || 0, dl = DELAYS.some(([v]) => v === ds) ? DELAYS
        : [...DELAYS, [ds, ds < 60 || ds % 60 ? ds + ' s' : ds / 60 + ' min']].sort((a, b) => a[0] - b[0]);
      const delay = `<select data-k="delay-${i}" data-device="${i}" data-delay aria-label="Delay, ${esc(d.name)}">${dl.map(([v, t]) => `<option value="${v}"${v === ds ? ' selected' : ''}>${t}</option>`).join('')}</select>`;
      const ctl = d.loading ? `<span class="wait" role="status"><span class="spin" aria-hidden="true"></span>Loading what it can do…</span>` : sel('off', 'Grid off', d.off) + sel('on', 'Grid on', d.on) + delay;
      return `<div class="row lt${d.enabled || d.loading ? '' : ' off'}"><span class="nm"><b title="${esc(d.name)}">${esc(d.name)}</b></span>${ctl}`
        + `<button type="button" class="rm" data-k="rm-${i}" data-action="remove" data-device="${i}" aria-label="Remove ${esc(d.name)}" title="Remove"${d.loading ? ' hidden' : ''}>${MINUS}</button>`
        + (d.loading ? '' : `<button type="button" class="switch" role="switch" data-k="en-${i}" data-action="enable" data-device="${i}" aria-checked="${!!d.enabled}" aria-label="Change ${esc(d.name)} with the grid"${s.automation.disabled ? ' aria-disabled="true"' : ''}><i></i></button>`)
        + `${status}</div>`;
    }).join('');
    // D-Y6: the device list can't load (keys rejected, plan expired, list call failed) → disabled with the reason.
    const add = s.add === 'loading' ? `<select data-k="add" disabled aria-label="Add a device"><option>Loading your devices…</option></select>`
      : s.add === 'failed' ? `<select data-k="add" disabled aria-label="Add a device"><option>Couldn’t load your devices</option></select>`
      : `<select data-k="add" id="add" class="is-placeholder" aria-label="Add a device"><option selected disabled>Add Device…</option>${(s.add || []).map(d => `<option>${esc(d.name)}${d.offline ? ' (offline)' : ''}</option>`).join('')}</select>`;
    const head = s.devices.length ? `<div class="row hd"><span>Device</span><span>Grid off</span><span>Grid on</span><span>Delay</span><span></span><span></span></div>` + rows
      : `<div class="row"><p>No devices yet. Add the Tuya devices you want to change.</p></div>`;
    return `<h2>Devices</h2><section class="grp devices">${head}<div class="row">${add}${s.addErr ? `<span class="err" role="alert">${esc(s.addErr)}</span>` : ''}</div></section>`
      + (s.devices.length ? `<p class="foot">Delay: how long a device waits after the grid changes, both ways. If the grid changes back first, the waiting change is dropped.</p>` : '');
  }
  function tuya(t) {
    if (t.connected) return `<h2>Tuya</h2><section class="grp"><div class="row"><div class="lbl"><span>Connected to your Tuya cloud project</span>`
      + `<small${t.problem ? ' class="err"' : ''}>${esc(t.problem || t.region + ' data center. The Access ID and Secret are in your Mac’s Keychain.')}</small></div>`
      + `${btn('disconnect', 'Disconnect')}</div></section>`;
    const v = t.values || {}, ro = t.busy ? ' readonly' : '';
    return `<h2>Tuya</h2><section class="grp">`
      + `<div class="row"><p>Use the Access ID and Secret of your own Tuya cloud project, with your Smart Life app linked to it. They’re saved in your Mac’s Keychain.</p></div>`
      + `<div class="row"><label class="lbl" for="aid">Access ID</label><input id="aid" data-k="aid" value="${esc(v.id || '')}" placeholder="Required" spellcheck="false" autocorrect="off" autocapitalize="off"${ro}></div>`
      + `<div class="row"><label class="lbl" for="sec">Access Secret</label><input id="sec" data-k="sec" type="password" placeholder="Required"${ro}></div>`
      + `<div class="row"><label class="lbl" for="dc">Data center</label><select id="dc" data-k="dc" class="wide${v.dc ? '' : ' is-placeholder'}"${t.busy ? ' disabled' : ''}><option value="" disabled${v.dc ? '' : ' selected'}>Choose…</option>${DC.map(d => `<option${d === v.dc ? ' selected' : ''}>${d}</option>`).join('')}</select></div>`
      // D-Y7: busy keeps the default button at full strength (aria-disabled); the spinner and 'Connecting…' sit in the link's place.
      + `<div class="row"><div class="lbl">${t.busy ? `<span class="connecting" role="status"><span class="spin" aria-hidden="true"></span>Connecting…</span>` : t.err ? `<span class="err" role="alert">${esc(t.err)}</span>` : `<a href="#" data-k="howto" data-action="howto">How to set up a Tuya cloud project</a>`}</div>`
      + `<button type="button" class="btn def" data-k="connect" data-action="connect"${t.busy ? ' aria-disabled="true"' : ''}>Connect</button></div></section>`;
  }

  function renderSettings(s) {
    const pane = s.pane === 'automation' ? 'automation' : 'general', title = PANES.find(p => p[0] === pane)[1];
    // D-Y8: roving tabindex (only the selected tab is in the Tab order); ← → select and show the next pane at once.
    const tabs = PANES.map(([id, label, icon]) => `<button type="button" role="tab" class="tab" data-k="tab-${id}" data-action="pane" data-pane="${id}" aria-selected="${id === pane}" tabindex="${id === pane ? 0 : -1}" id="tab-${id}" aria-controls="pane">${icon}<span>${label}</span></button>`).join('');
    const body = pane === 'general'
      ? account(s.account) + startup(s.general) + notifications(s.general) + troubleshooting()
      : automation(s.automation) + (s.tuya.connected || s.devices.length ? devices(s) : '') + tuya(s.tuya);
    // D-O11: the title strip is empty (the native title bar draws the traffic lights and the title). D-O10: the strip and
    // the toolbar's gaps drag the window (data-drag, see below); the tab buttons don't carry it, so they stay clickable.
    return `<div class="win" role="dialog" aria-label="${title}"><div class="chrome"><div class="titlebar" data-drag></div>`
      + `<div class="toolbar" role="tablist" aria-label="Settings" data-drag>${tabs}</div></div><div class="body" id="pane" role="tabpanel" aria-labelledby="tab-${pane}">${body}</div></div>`;
  }
  root.GWS = { renderSettings };
  if (!root.__TAURI__) return; // loaded for a render check, not in the app

  // ---- The live window ----
  const { listen } = root.__TAURI__.event, { invoke } = root.__TAURI__.core;
  const el = document.getElementById('root');
  const act = a => invoke('settings_action', { a }).catch(() => {});
  // A control that is gone after the action: where focus goes next (Sign Out → Sign In…, Connect → Add Device…).
  const FOCUS_NEXT = { signout: 'signin', signin: 'signout', connect: 'add', aid: 'add', sec: 'add', dc: 'add', disconnect: 'aid' };
  let state = null, last = '', height = 0, hold = 0, queued = null, focusTo = null;

  const natural = () => { const c = el.querySelector('.chrome'), b = el.querySelector('.body'); return c && b ? c.offsetHeight + b.scrollHeight : 0; };
  function render(s, open) {
    const json = JSON.stringify(s);
    if (!open && json === last) return;
    last = json;
    const was = state;
    state = s;
    const a = document.activeElement, key = focusTo || (a && a.dataset && a.dataset.k);
    const caret = a && a.tagName === 'INPUT' ? [a.selectionStart, a.selectionEnd] : null;
    focusTo = null;
    // Typed values survive a re-render (a grid change while the user types the Secret); a fresh open starts from the state.
    const typed = !open && el.querySelector('#aid') ? ['aid', 'sec', 'dc'].map(id => [id, el.querySelector('#' + id).value]) : null;
    const body = el.querySelector('.body'), scroll = body && was && was.pane === s.pane ? body.scrollTop : 0;
    el.innerHTML = renderSettings(s);
    if (typed && el.querySelector('#aid')) {
      typed.forEach(([id, v]) => { if (v) el.querySelector('#' + id).value = v; });
      const dc = el.querySelector('#dc'); dc.classList.toggle('is-placeholder', !dc.value);
    }
    el.querySelector('.body').scrollTop = scroll;
    if (open) { if (document.activeElement) document.activeElement.blur(); }
    else if (key) {
      const t = el.querySelector(`[data-k="${key}"]`) || el.querySelector(`[data-k="${FOCUS_NEXT[key]}"]`);
      if (t && !t.disabled) t.focus();
      if (caret && t && t.dataset.k === key && t.tagName === 'INPUT') t.setSelectionRange(caret[0], caret[1]);
    }
    const h = natural();
    if (open || h !== height) { height = h; invoke('settings_show', { h, show: !!open }); }
  }
  // While a switch's knob slides (150 ms), a new state waits, or the re-render would cut the slide.
  const take = (s, open) => {
    if (open) { clearTimeout(hold); hold = 0; queued = null; return render(s, true); }
    if (hold) queued = s; else render(s, false);
  };

  listen('settings-open', e => take(e.payload, true));
  listen('settings-state', e => take(e.payload, false));

  function pane(id) { if (state && id !== state.pane) { focusTo = 'tab-' + id; act({ id: 'pane', pane: id }); } }
  async function connect() {
    if (!state || state.tuya.busy) return;
    const aid = el.querySelector('#aid'), sec = el.querySelector('#sec'), dc = el.querySelector('#dc');
    if (!aid.value.trim()) return aid.focus();
    if (!sec.value) return sec.focus();
    if (!dc.value) return dc.focus();
    // The error line comes back in the state (tuya.err); nothing else to do here.
    await act({ id: 'connect', access_id: aid.value, secret: sec.value, dc: dc.value });
  }

  el.addEventListener('click', e => {
    const b = e.target.closest('[data-action]');
    if (!b || b.getAttribute('aria-disabled') === 'true') return;
    const id = b.dataset.action;
    if (id === 'howto') e.preventDefault();
    if (id === 'pane') return pane(b.dataset.pane);
    if (id === 'connect') return connect();
    if (id === 'enable' || id === 'login') {
      const on = b.getAttribute('aria-checked') !== 'true';
      b.setAttribute('aria-checked', String(on)); // the knob slides now; the state confirms it
      clearTimeout(hold);
      hold = setTimeout(() => { hold = 0; if (queued) { const q = queued; queued = null; render(q, false); } }, 160);
      return act(id === 'enable' ? { id, device: +b.dataset.device, on } : { id, on });
    }
    if (id === 'remove') return act({ id, device: +b.dataset.device });
    act({ id });
  });

  el.addEventListener('change', e => {
    const t = e.target;
    if (t.id === 'dc') return t.classList.toggle('is-placeholder', !t.value);
    if (t.id === 'add') return act({ id: 'add', item: t.selectedIndex - 1 });
    if (t.id === 'low') return act({ id: 'low_battery', pct: +t.value });
    if (t.dataset.which) return act({ id: 'choice', device: +t.dataset.device, which: t.dataset.which, choice: +t.value });
    if (t.dataset.delay != null) return act({ id: 'delay', device: +t.dataset.device, delay_s: +t.value });
  });

  document.addEventListener('keydown', e => {
    const k = e.key.toLowerCase();
    if (e.key === 'Escape' || (e.metaKey && k === 'w')) { e.preventDefault(); return act({ id: 'close' }); }
    if (e.key === 'Enter' && (e.target.id === 'aid' || e.target.id === 'sec')) { e.preventDefault(); return connect(); }
    if (e.target.getAttribute && e.target.getAttribute('role') === 'tab' && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      e.preventDefault();
      const i = PANES.findIndex(p => p[0] === state.pane), n = PANES.length;
      pane(PANES[(i + (e.key === 'ArrowRight' ? 1 : n - 1)) % n][0]);
    }
  });

  // B-R2: the drag starts once the pointer moves 3 pt with the button held, not on mousedown (Tauri's bare drag region).
  // On the first click into a window that was just shown, the mousedown call landed after the button was up, and
  // AppKit then moved the window's top-left corner to the pointer. Only a press right on the strip or the toolbar counts.
  let press = null;
  document.addEventListener('mousedown', e => {
    press = e.button === 0 && e.target.hasAttribute('data-drag') ? [e.screenX, e.screenY] : null;
    if (press) e.preventDefault(); // no text cursor, no focus change
  });
  document.addEventListener('mouseup', () => { press = null; });
  document.addEventListener('mousemove', e => {
    if (!press) return;
    if (!(e.buttons & 1)) { press = null; return; }
    if (Math.hypot(e.screenX - press[0], e.screenY - press[1]) < 3) return;
    press = null;
    invoke('plugin:window|start_dragging').catch(() => {});
  });

  invoke('settings_ready');
})(typeof window !== 'undefined' ? window : globalThis);
