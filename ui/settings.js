/* The Settings window. Rust sends `settings-open` / `settings-state` (shape: hand-off spec › State and actions, copy already
   formatted); GWS.renderSettings(state) returns the window's HTML; every control answers with `settings_action`.
   Three panes behind a noncustomizable toolbar (General, Automation, Stats); the native title bar shows the pane's name. */
(function (root) {
  'use strict';
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const DC = ['Central Europe', 'Western Europe', 'Western America', 'Eastern America', 'China', 'India'];
  const S = 'fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"';
  const MINUS = `<svg viewBox="0 0 14 14" ${S}><circle cx="7" cy="7" r="5.8"/><path d="M4.4 7h5.2"/></svg>`;
  // Toolbar glyphs, 22 pt, same stroke family as the panel's icons (gear = the panel's Settings icon).
  const GEAR = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"><path d="M14.22 7.00L14.22 9.00L12.77 9.13L12.17 10.57L13.11 11.69L11.69 13.11L10.57 12.17L9.13 12.77L9.00 14.22L7.00 14.22L6.87 12.77L5.43 12.17L4.31 13.11L2.89 11.69L3.83 10.57L3.23 9.13L1.78 9.00L1.78 7.00L3.23 6.87L3.83 5.43L2.89 4.31L4.31 2.89L5.43 3.83L6.87 3.23L7.00 1.78L9.00 1.78L9.13 3.23L10.57 3.83L11.69 2.89L13.11 4.31L12.17 5.43L12.77 6.87Z"/><circle cx="8" cy="8" r="2.1"/></svg>';
  const BOLT = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round"><path d="M9.2 1.6L3.6 9.1h4.1l-1 5.3 5.7-7.6H8.3z"/></svg>';
  // T-015: bars, the same 16 grid and 1.1 stroke (SF Symbols' chart.bar).
  const BARS = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"><rect x="2.2" y="8.6" width="2.8" height="5.2" rx=".7"/><rect x="6.6" y="2.2" width="2.8" height="11.6" rx=".7"/><rect x="11" y="5.6" width="2.8" height="8.2" rx=".7"/></svg>';
  const PANES = [['general', 'General', GEAR], ['automation', 'Automation', BOLT], ['stats', 'Stats', BARS]];
  // T-015 Stats glyphs (14 grid, the panel's 1.4 stroke).
  const CH = d => `<svg viewBox="0 0 14 14" ${S}><path d="${d}"/></svg>`;
  const LEFT = CH('M8.6 3L4.6 7l4 4'), RIGHT = CH('M5.4 3l4 4-4 4'), DOWN = CH('M3.5 5.5L7 9l3.5-3.5');
  const CAL = `<svg viewBox="0 0 14 14" ${S}><rect x="1.8" y="2.6" width="10.4" height="9.6" rx="1.8"/><path d="M1.8 5.6h10.4M4.6 1.4v2.2M9.4 1.4v2.2"/></svg>`;
  const WARN = `<svg viewBox="0 0 14 14" ${S}><path d="M7 1.9l5.5 9.7h-11z"/><path d="M7 5.8v2.6"/><path d="M7 10.1v.1"/></svg>`;

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
    // T-015: Account is the last group of General (owner), as Tuya is the last of Automation.
    return `<h2>Account</h2><section class="grp"><div class="row">${body}</div></section>`;
  }
  const startup = g => `<h2 class="first">Startup</h2><section class="grp"><div class="row"><div class="lbl"><span>Open at login</span><small>Start GoodWe Status in the menu bar when you log in.</small></div>${sw('login', g.openAtLogin, 'Open at login')}</div></section>`;
  // T-015: the panel's temperature row. The helper says what the panel will really show (temps: 'both' | 'inverter' | 'none').
  const menuBar = g => {
    const help = { inverter: 'This inverter doesn’t send a battery temperature, so the panel shows the inverter’s only.',
      none: 'This inverter doesn’t send its temperatures.' }[g.temps] || 'Show the inverter and battery temperature in the panel.';
    // T-020: today's cut strip in the panel (on by default). No battery = no cut history, so it is dimmed and says why.
    const noBat = g.hasBattery === false;
    const cuts = `<div class="row"><div class="lbl"><span>Show today’s power cuts</span><small>${noBat ? 'Power cuts are worked out from the battery’s readings, and this station has no battery.' : 'Show a 24-hour strip of today’s cuts under Grid in the panel.'}</small></div>${sw('cuts', !!g.showCuts && !noBat, 'Show today’s power cuts', noBat)}</div>`;
    return `<h2>Menu Bar</h2><section class="grp"><div class="row"><div class="lbl"><span>Show temperatures</span><small>${help}</small></div>${sw('temps', g.showTemps && g.temps !== 'none', 'Show temperatures', g.temps === 'none')}</div>${cuts}</section>`;
  };
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

  // ---- Stats pane (T-015) ----
  // Rust sends every string formatted (spec › Stats state). The pane keeps one height on every day: the table box is
  // 5½ rows tall and scrolls inside, the footer keeps two lines, so ← / → never resize the window.
  const DAYW = 494; // strip width: window 560 − body 40 − group border 2 − row padding 24
  function strip(st) {
    // With a part to read, the strip is one focusable read-out (as the panel chart): ← → move from part to part (T-025).
    const svg = GWStrip.svg(st.strip, DAYW), n = svg.includes('class="hit"');
    return `<div class="st-plot"${n ? ` tabindex="0" role="group" aria-label="Power cuts on the day strip. Use left and right arrows to read each part of the day."` : ''}>`
      + `${svg}${n ? '<div class="st-tip" role="status" hidden></div>' : ''}</div>`;
  }
  // A cell that isn't a clock time (Before midnight, After midnight, No readings) is muted: unknown, not measured.
  const cell = (day, t, unknown) => unknown ? `<span><em>${esc(t)}</em></span>` : day ? `<span><em>${esc(day)}</em> ${esc(t)}</span>` : `<span>${esc(t)}</span>`;
  function calendar(c) {
    const head = `<div class="cal-head"><b id="cal-month">${esc(c.month)}</b>`
      + `<button type="button" class="rm" data-k="cal-prev" data-action="cal-month" data-step="-1" aria-label="Previous month"${c.prev ? '' : ' aria-disabled="true"'}>${LEFT}</button>`
      + `<button type="button" class="rm" data-k="cal-next" data-action="cal-month" data-step="1" aria-label="Next month"${c.next ? '' : ' aria-disabled="true"'}>${RIGHT}</button></div>`;
    // D-O7: the week starts on the Mac's first weekday; Rust sends the letters in order and the lead for that start.
    const wk = (c.letters || ['S', 'M', 'T', 'W', 'T', 'F', 'S']).map(d => `<span class="wd" aria-hidden="true">${d}</span>`).join('');
    const blank = Array.from({ length: c.lead }, () => '<span></span>').join('');
    const days = c.days.map((k, i) => {
      const d = i + 1, cls = ['day', k === 'cut' ? 'has-cut' : '', d === c.today ? 'is-today' : '', d === c.selected ? 'is-sel' : ''].join(' ').trim();
      const lab = `${d} ${c.month}${k === 'cut' ? ', power cut' : k === 'clear' ? ', no cuts' : k === 'none' ? ', no data' : ''}`;
      return `<button type="button" class="${cls}" data-k="cal-${d}" data-action="cal-day" data-day="${d}" aria-label="${esc(lab)}"${d === c.selected ? ' aria-current="date"' : ''}${k === 'none' ? ' aria-disabled="true"' : ''} tabindex="${d === (c.focus || c.selected) ? 0 : -1}">${d}</button>`;
    }).join('');
    return `<div class="cal" role="dialog" aria-labelledby="cal-month">${head}<div class="cal-grid" role="group" aria-label="${esc(c.month)}">${wk}${blank}${days}</div>`
      + `<p class="cal-key"><i aria-hidden="true"></i>Days with a power cut</p></div>`;
  }
  function stats(st) {
    if (st.status === 'nobattery') st = { ...st, prev: false, next: false, isToday: true, calendar: null, rows: [], summary: '\u00a0', strip: { from: 1440 } };
    const off = st.status === 'nobattery' ? ' aria-disabled="true"' : '';
    const head = `<div class="st-head">`
      + `<button type="button" class="btn st-date" data-k="calendar" data-action="calendar" aria-haspopup="dialog" aria-expanded="${!!st.calendar}"${off}>${CAL}<span>${esc(st.date)}</span>${DOWN}</button>`
      + `<div class="seg" role="group" aria-label="Day">`
      + `<button type="button" data-k="day-prev" data-action="day" data-step="-1" aria-label="Previous day" title="${esc(st.prevTip || 'Previous day (←)')}"${st.prev ? '' : ' aria-disabled="true"'}>${LEFT}</button>`
      + `<button type="button" data-k="day-today" data-action="today" title="Today (⌘T)"${st.isToday ? ' aria-disabled="true"' : ''}>Today</button>`
      + `<button type="button" data-k="day-next" data-action="day" data-step="1" aria-label="Next day" title="Next day (→)"${st.next ? '' : ' aria-disabled="true"'}>${RIGHT}</button></div>`
      + (st.calendar ? calendar(st.calendar) : '') + `</div>`;
    const rows = st.rows || [];
    const box = st.status === 'nobattery' ? `<div class="st-msg"><p>${esc(st.msg)}</p></div>`
      : st.status === 'loading' ? `<div class="st-msg" role="status"><span class="spin" aria-hidden="true"></span>${esc(st.msg)}</div>`
      : st.status === 'unsaved' ? `<div class="st-msg" role="status"><p>${esc(st.msg)}</p>${btn(...(st.retry || ['retry', 'Try Again']))}</div>`
      : !rows.length ? `<div class="st-msg"><p>${esc(st.msg || 'The grid was on all day.')}</p></div>`
      : `<div class="st-rows">` + rows.map((r, i) => `<div class="st-tr${r.running ? ' is-running' : ''}" role="row" data-i="${i}">`
          + `<span role="cell">${cell(r.offDay, r.off, r.offUnknown)}</span><span role="cell">${r.running ? '<b>Still off</b>' : cell(r.onDay, r.on, r.onUnknown)}</span><span role="cell">${esc(r.dur)}</span></div>`).join('') + `</div>`;
    const table = `<div class="st-table" role="table" aria-label="Power cuts, ${esc(st.date)}">`
      + (rows.length ? `<div class="st-tr st-th" role="row"><span role="columnheader">Grid off</span><span role="columnheader">Grid on</span><span role="columnheader">Duration</span></div>` : '')
      + box + `</div>`;
    const sum = `<div class="st-sum"><p>${esc(st.summary)}</p>${strip(st)}</div>`;
    // Footer: the note (where the numbers come from, or why this day is special), then one status line.
    const b = st.backfill, pr = st.problem;
    const status = pr ? `<p class="st-line is-warn" role="status">${WARN}<span>${esc(pr.text)}</span>${pr.action ? btn(pr.action[0], pr.action[1]) : ''}</p>`
      : b && b.paused ? `<p class="st-line" role="status"><span>${esc(b.paused)}</span></p>`
      : b ? `<p class="st-line" role="status"><span>Loading history: ${b.done} of ${b.total} days</span><span class="bar" role="progressbar" aria-label="Loading history" aria-valuemin="0" aria-valuemax="${b.total}" aria-valuenow="${b.done}"><i style="width:${(b.done / b.total * 100).toFixed(1)}%"></i></span></p>` : '';
    return head + `<section class="grp st">${sum}${table}</section><div class="st-foot">${st.note ? `<p class="foot">${esc(st.note)}</p>` : ''}${status}</div>`;
  }

  function renderSettings(s) {
    const pane = PANES.some(p => p[0] === s.pane) ? s.pane : 'general', title = PANES.find(p => p[0] === pane)[1];
    // D-Y8: roving tabindex (only the selected tab is in the Tab order); ← → select and show the next pane at once.
    const tabs = PANES.map(([id, label, icon]) => `<button type="button" role="tab" class="tab" data-k="tab-${id}" data-action="pane" data-pane="${id}" aria-selected="${id === pane}" tabindex="${id === pane ? 0 : -1}" id="tab-${id}" aria-controls="pane">${icon}<span>${label}</span></button>`).join('');
    const body = pane === 'general'
      ? startup(s.general) + menuBar(s.general) + notifications(s.general) + troubleshooting() + account(s.account)
      : pane === 'stats' ? stats(s.stats)
      : automation(s.automation) + (s.tuya.connected || s.devices.length ? devices(s) : '') + tuya(s.tuya);
    // D-O11: the title strip is empty (the native title bar draws the traffic lights and the title). D-O10: the strip and
    // the toolbar's gaps drag the window (data-drag, see below); the tab buttons don't carry it, so they stay clickable.
    return `<div class="win" role="dialog" aria-label="${title}"><div class="chrome"><div class="titlebar" data-drag></div>`
      + `<div class="toolbar" role="tablist" aria-label="Settings" data-drag>${tabs}</div></div><div class="body" id="pane" role="tabpanel" aria-labelledby="tab-${pane}">${body}</div></div>`;
  }
  // ---- Strip read-out (T-015, owner): point at a cut (or its table row), or Tab to the strip and use ← →. The tooltip is the
  // panel chart's card: the time range on top, then a red swatch, Grid off and the length. The cut's row lights up and the
  // other cuts fade back. strip.cuts[i] is the day part of rows[i] (Rust sends them in the same order).
  function bindStrip(rootEl, st) {
    if (!st) return;
    const rows = () => rootEl.querySelectorAll('.st-rows .st-tr');
    const r = GWStrip.bind(rootEl.querySelector('.st-plot[tabindex]'), st.rows, DAYW, { tip: 'st-tip', left: -12,
      onShow: i => {
        rows().forEach(e => e.classList.toggle('is-on', e.dataset.i === String(i)));
        const row = rootEl.querySelector(`.st-rows .st-tr[data-i="${i}"]`);
        if (row) { const box = row.parentElement; if (row.offsetTop < box.scrollTop || row.offsetTop + row.offsetHeight > box.scrollTop + box.clientHeight) box.scrollTop = row.offsetTop - 8; }
      },
      onHide: () => rows().forEach(e => e.classList.remove('is-on')) });
    if (r) rows().forEach(row => { row.addEventListener('pointerenter', () => r.show(+row.dataset.i)); row.addEventListener('pointerleave', r.hide); });
  }
  root.GWS = { renderSettings, bindStrip };

  if (!root.__TAURI__) return; // loaded for a render check, not in the app

  // ---- The live window ----
  const { listen } = root.__TAURI__.event, { invoke } = root.__TAURI__.core;
  const el = document.getElementById('root');
  const act = a => invoke('settings_action', { a }).catch(() => {});
  // A control that is gone after the action: where focus goes next (Sign Out → Sign In…, Connect → Add Device…).
  const FOCUS_NEXT = { signout: 'signin', signin: 'signout', connect: 'add', aid: 'add', sec: 'add', dc: 'add', disconnect: 'aid' };
  let state = null, last = '', height = 0, hold = 0, queued = null, focusTo = null;
  const DAY0 = '\u0000day'; // T-015: focus the calendar's roving day (calendar.focus, else the selected day) after the render

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
      const t = key === DAY0 ? el.querySelector('.cal .day[tabindex="0"]') : el.querySelector(`[data-k="${key}"]`) || el.querySelector(`[data-k="${FOCUS_NEXT[key]}"]`);
      if (t && !t.disabled) t.focus();
      if (caret && t && t.dataset.k === key && t.tagName === 'INPUT') t.setSelectionRange(caret[0], caret[1]);
    }
    if (s.pane === 'stats') bindStrip(el, s.stats);
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
    // T-015 Stats: the day and calendar actions carry their step or day; Rust answers with the new state.
    if (id === 'day' || id === 'cal-month') return act({ id: id.replace('-', '_'), step: +b.dataset.step });
    if (id === 'cal-day') { focusTo = 'calendar'; return act({ id: 'cal_day', day: +b.dataset.day }); }
    if (id === 'calendar') focusTo = state.stats.calendar ? 'calendar' : DAY0; // closing: back to the button; opening: the selected day
    if (id === 'enable' || id === 'login' || id === 'temps' || id === 'cuts') {
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

  // T-015: a click outside the open calendar closes it (the date button toggles it itself).
  document.addEventListener('mousedown', e => {
    if (state && state.pane === 'stats' && state.stats.calendar && !e.target.closest('.cal, [data-k="calendar"]')) { focusTo = 'calendar'; act({ id: 'calendar' }); }
  });
  // T-015 Stats keys. Calendar open: ← → ↑ ↓ move focus a day / a week (into the next month through Rust), Return / Space
  // pick (native button click), Esc closes it. Calendar closed: ← → step a day unless a toolbar item has focus, ⌘T = today.
  function statsKey(e) {
    const st = state.stats, t = e.target, k = e.key;
    if (st.calendar) {
      if (k === 'Escape') { focusTo = 'calendar'; act({ id: 'calendar' }); return true; }
      const mv = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7 }[k];
      if (!mv || !t.classList.contains('day')) return false;
      const d = +t.dataset.day + mv, n = st.calendar.days.length;
      if (d < 1 || d > n) { focusTo = DAY0; act({ id: 'cal_month', step: Math.sign(mv), focus: mv < 0 ? d : d - n }); return true; } // focus: day in that month, ≤ 0 = from its end
      const b = el.querySelector(`[data-k="cal-${d}"]`); if (b) b.focus();
      return true;
    }
    if (st.status === 'nobattery') return false;
    if ((k === 'ArrowLeft' || k === 'ArrowRight') && t.getAttribute('role') !== 'tab' && t.tagName !== 'SELECT' && !t.classList.contains('st-plot')) { act({ id: 'day', step: k === 'ArrowRight' ? 1 : -1 }); return true; }
    if (e.metaKey && k.toLowerCase() === 't') { act({ id: 'today' }); return true; }
    return false;
  }

  document.addEventListener('keydown', e => {
    const k = e.key.toLowerCase();
    if (state && state.pane === 'stats' && !(e.metaKey && k === 'w') && statsKey(e)) return e.preventDefault();
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
