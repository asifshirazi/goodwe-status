/* The Settings window. Rust sends `settings-open` / `settings-state` (shape: hand-off spec › State and actions, copy already
   formatted); GWS.renderSettings(state) returns the window's HTML; every control answers with `settings_action`.
   Four panes behind a noncustomizable toolbar (General, Automation, Stats, About); the native title bar shows the pane's name. */
(function (root) {
  'use strict';
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  // T-029: numbers, times and units inside a sentence in SF Mono (.num, tokens.css); the panel's rule, except a number
  // never ends on a dot or comma ("You have 0.1.2." keeps its full stop in the sentence's face). Escapes first.
  const num = s => esc(s).replace(/~?\d(?:[\d:.,]*\d)?(?:[\u00a0 ]?(?:AM|PM|kW|°C|min|W|h|m|s|%)(?![A-Za-z]))?/g, m => `<span class="num">${m}</span>`);
  const DC = ['Central Europe', 'Western Europe', 'Western America', 'Eastern America', 'China', 'India'];
  const S = 'fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"';
  // T-029 (owner): Remove is the danger colour with a filled glyph (SF Symbols' minus.circle.fill: the bar in the well colour, cut out of a red disc).
  const MINUS = '<svg viewBox="0 0 14 14"><circle cx="7" cy="7" r="6.3" fill="currentColor"/><path class="bar" d="M4.3 7h5.4" stroke-width="1.5" stroke-linecap="round"/></svg>';
  // T-029: the panel's Refresh glyph (16 grid, 1.4 stroke) for Refresh device list.
  const REFRESH = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M13 8a5 5 0 1 1-1.5-3.6"/><path d="M12 1.8v2.9H9.1"/></svg>';
  // Toolbar glyphs, 22 pt, same stroke family as the panel's icons (gear = the panel's Settings icon).
  const GEAR = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"><path d="M14.22 7.00L14.22 9.00L12.77 9.13L12.17 10.57L13.11 11.69L11.69 13.11L10.57 12.17L9.13 12.77L9.00 14.22L7.00 14.22L6.87 12.77L5.43 12.17L4.31 13.11L2.89 11.69L3.83 10.57L3.23 9.13L1.78 9.00L1.78 7.00L3.23 6.87L3.83 5.43L2.89 4.31L4.31 2.89L5.43 3.83L6.87 3.23L7.00 1.78L9.00 1.78L9.13 3.23L10.57 3.83L11.69 2.89L13.11 4.31L12.17 5.43L12.77 6.87Z"/><circle cx="8" cy="8" r="2.1"/></svg>';
  const BOLT = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round"><path d="M9.2 1.6L3.6 9.1h4.1l-1 5.3 5.7-7.6H8.3z"/></svg>';
  // T-015: bars, the same 16 grid and 1.1 stroke (SF Symbols' chart.bar).
  const BARS = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"><rect x="2.2" y="8.6" width="2.8" height="5.2" rx=".7"/><rect x="6.6" y="2.2" width="2.8" height="11.6" rx=".7"/><rect x="11" y="5.6" width="2.8" height="8.2" rx=".7"/></svg>';
  // T-022: info.circle on the same 16 grid and 1.1 stroke.
  const INFO = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"><circle cx="8" cy="8" r="6.2"/><path d="M8 7.2v4.1"/><circle cx="8" cy="4.9" r=".55" fill="currentColor" stroke="none"/></svg>';
  const PANES = [['general', 'General', GEAR], ['automation', 'Automation', BOLT], ['stats', 'Stats', BARS], ['about', 'About', INFO]];
  // T-015 Stats glyphs (14 grid, the panel's 1.4 stroke).
  const CH = d => `<svg viewBox="0 0 14 14" ${S}><path d="${d}"/></svg>`;
  const LEFT = CH('M8.6 3L4.6 7l4 4'), RIGHT = CH('M5.4 3l4 4-4 4'), DOWN = CH('M3.5 5.5L7 9l3.5-3.5');
  const CAL = `<svg viewBox="0 0 14 14" ${S}><rect x="1.8" y="2.6" width="10.4" height="9.6" rx="1.8"/><path d="M1.8 5.6h10.4M4.6 1.4v2.2M9.4 1.4v2.2"/></svg>`;
  const WARN = `<svg viewBox="0 0 14 14" ${S}><path d="M7 1.9l5.5 9.7h-11z"/><path d="M7 5.8v2.6"/><path d="M7 10.1v.1"/></svg>`;

  // Delay pop-up (seconds → label). One value per device, used both ways; 0 = at once.
  const DELAYS = [[0, 'Instant'], [30, '30 s'], [60, '1 min'], [120, '2 min'], [300, '5 min'], [600, '10 min'], [900, '15 min'], [1800, '30 min']];
  const sw = (k, on, label, off) => `<button type="button" class="switch" role="switch" data-k="${k}" data-action="${k}" aria-checked="${on}" aria-label="${esc(label)}"${off ? ' aria-disabled="true"' : ''}><i></i></button>`;
  // T-029 (owner): the buttons that end a connection (Sign Out, Disconnect) use the danger variant.
  const DANGER = ['signout', 'disconnect'];
  const btn = (action, text, def) => `<button type="button" class="btn${def ? ' def' : ''}${DANGER.includes(action) ? ' is-danger' : ''}" data-k="${action}" data-action="${action}">${text}</button>`;

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
    // T-029 (owner): the panel's opening motion and the pane switch can be turned off here; off by default (the motion plays).
    const motion = `<div class="row"><div class="lbl"><span>Reduce motion</span><small>Open the panel and switch panes here without animation. macOS’s own Reduce motion (Accessibility › Display) does the same.</small></div>${sw('motion', !!g.reduceMotion, 'Reduce motion')}</div>`;
    return `<h2>Menu Bar</h2><section class="grp"><div class="row"><div class="lbl"><span>Show temperatures</span><small>${help}</small></div>${sw('temps', g.showTemps && g.temps !== 'none', 'Show temperatures', g.temps === 'none')}</div>${cuts}${motion}</section>`;
  };
  // T-008: one alert at the setting and one at half of it (Rust: rules::low_battery, integer half), only while the grid is off.
  const LOW = [5, 10, 15, 20, 25, 30, 35, 40, 45, 50];
  const notifications = g => {
    const p = g.lowBattery || 20, opts = LOW.includes(p) ? LOW : [...LOW, p].sort((a, b) => a - b);
    return `<h2>Notifications</h2><section class="grp"><div class="row"><div class="lbl"><label for="low">Low battery alert</label><small>While the grid is off, notify at <span class="num">${p}%</span> and again at <span class="num">${Math.floor(p / 2)}%</span>.</small></div>`
      + `<select id="low" data-k="low">${opts.map(v => `<option value="${v}"${v === p ? ' selected' : ''}>${v}%</option>`).join('')}</select></div></section>`;
  };
  const troubleshooting = () => `<h2>Troubleshooting</h2><section class="grp"><div class="row"><div class="lbl"><span>Log folder</span><small>The app’s logs, for reporting a problem.</small></div>${btn('logs', 'Open Log Folder')}</div></section>`;

  // ---- About pane (T-022) ----
  // D-O1: a newer version is waiting (found, not yet installed): the About tab and the panel gear keep their dot.
  const PENDING = ['available', 'downloading', 'installing', 'relaunching', 'failed'];
  const pending = a => !!(a && a.update && PENDING.includes(a.update.kind));
  const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
  // Rust sends Unix ms for the two clock lines; the page writes them in the Mac's own zone: "today at 9:41 AM".
  function when(ms, now = Date.now()) {
    const d = new Date(ms), day = x => { const t = new Date(x); return new Date(t.getFullYear(), t.getMonth(), t.getDate()).getTime(); };
    const ago = Math.round((day(now) - day(ms)) / 864e5), h = d.getHours();
    const t = `${h % 12 || 12}:${String(d.getMinutes()).padStart(2, '0')} ${h < 12 ? 'AM' : 'PM'}`;
    return `${ago === 0 ? 'today' : ago === 1 ? 'yesterday' : `on ${d.getDate()} ${MONTHS[d.getMonth()]}`} at ${t}`;
  }
  // update.kind: checking · current · available · downloading · installing · relaunching · failed · offline · unreachable.
  // One Updates group; its first row always says where things stand and holds the one button that moves them on.
  // T-027: GitHub's own mark, unmodified (primer/octicons mark-github-16), one colour from the text; decorative, the row keeps its label.
  const GITHUB = '<svg class="ab-gh" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true" focusable="false"><path d="M6.766 11.328c-2.063-.25-3.516-1.734-3.516-3.656 0-.781.281-1.625.75-2.188-.203-.515-.172-1.609.063-2.062.625-.078 1.468.25 1.968.703.594-.187 1.219-.281 1.985-.281.765 0 1.39.094 1.953.265.484-.437 1.344-.765 1.969-.687.218.422.25 1.515.046 2.047.5.593.766 1.39.766 2.203 0 1.922-1.453 3.375-3.547 3.64.531.344.89 1.094.89 1.954v1.625c0 .468.391.734.86.547C13.781 14.359 16 11.53 16 8.03 16 3.61 12.406 0 7.984 0 3.563 0 0 3.61 0 8.031a7.88 7.88 0 0 0 5.172 7.422c.422.156.828-.125.828-.547v-1.25c-.219.094-.5.156-.75.156-1.031 0-1.64-.562-2.078-1.609-.172-.422-.36-.672-.719-.719-.187-.015-.25-.093-.25-.187 0-.188.313-.328.625-.328.453 0 .844.281 1.25.86.313.452.64.655 1.031.655s.641-.14 1-.5c.266-.265.47-.5.657-.656"/></svg>';
  const ARROW = '<svg viewBox="0 0 14 14" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M5 3h6v6M11 3L3.5 10.5"/></svg>';
  function about(a, now) {
    const u = a.update || { kind: 'current' }, last = u.checked ? `Last checked ${when(u.checked, now)}.` : '';
    // Owner (T-022): name and version on one line, each at its own size, on one baseline; a long version wraps whole.
    const hero = `<section class="ab-hero"><img src="app-icon-128.png" alt="" width="64" height="64"><div><div class="ab-name"><h1>GoodWe Status</h1>`
      + `<span class="ab-ver">Version ${esc(a.version)}</span></div><p>Your home battery and whether the grid is on, in the menu bar.</p><p class="ab-note">Not affiliated with GoodWe.</p></div></section>`;
    const spin = '<span class="spin" aria-hidden="true"></span>', also = d => [d, last].filter(Boolean).join(' ');
    const top = {
      checking: [`${spin}Checking for updates…`, u.detail, btn('check', 'Check Now').replace('<button', '<button aria-disabled="true"')],
      current: [`GoodWe Status is up to date`, u.updated ? `Updated to ${u.updated.version} ${when(u.updated.at, now)}.` : last, btn('check', 'Check Now')],
      available: [`Version ${esc(u.version)} is available`, u.detail, btn('install', 'Update and Relaunch', true)],
      downloading: [`Downloading version ${esc(u.version)}…`, '', btn('cancel', 'Cancel')],
      installing: [`${spin}Installing version ${esc(u.version)}…`, u.detail, ''],
      relaunching: [`${spin}Relaunching…`, u.detail, ''],
      failed: [`${WARN}The update couldn’t be installed`, u.reason, btn('install', 'Retry')],
      offline: [`Can’t check for updates`, also(u.detail), btn('check', 'Check Now')],
      unreachable: [`Can’t reach GitHub`, also(u.detail), btn('check', 'Check Now')],
    }[u.kind] || ['', '', ''];
    const warn = u.kind === 'failed' || u.kind === 'offline' || u.kind === 'unreachable';
    // No total from GitHub: the title gets the spinner instead of the bar (spec › For the build).
    if (u.kind === 'downloading' && u.pct == null) top[0] = spin + top[0];
    const prog = u.kind === 'downloading' && u.pct != null
      ? `<div class="ab-prog"><span class="bar" role="progressbar" aria-label="Download" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${u.pct}"><i style="width:${u.pct}%"></i></span><small>${esc(u.progress || '')}</small></div>` : '';
    const row1 = `<div class="row" role="status"><div class="lbl"><span class="ab-state${warn ? ' is-warn' : ''}">${top[0]}</span>`
      + (top[1] ? `<small${u.kind === 'failed' ? ' class="err"' : ''}>${num(top[1])}</small>` : '') + prog + `</div>${top[2]}</div>`;
    // What's new: from the release's notes, while there is a newer version to talk about.
    const notes = u.notes && u.notes.length && PENDING.includes(u.kind)
      ? `<div class="row ab-notes"><div class="lbl"><span>What’s new in ${esc(u.version)}</span><ul>${u.notes.map(n => `<li>${esc(n)}</li>`).join('')}</ul>`
        + `<a href="#" data-action="notes" data-k="notes">Release notes on GitHub${ARROW}</a></div></div>` : '';
    const foot = PENDING.includes(u.kind)
      ? 'After the update, macOS may ask whether GoodWe Status can use its saved sign-in, and again for its Tuya keys if Tuya is connected. Click Always Allow each time.'
      : 'GoodWe Status looks for a new release on GitHub each time it opens.';
    const links = `<h2>Project</h2><section class="grp"><div class="row">${GITHUB}<div class="lbl"><span>Source code</span><small>github.com/asifshirazi/goodwe-status</small></div>${btn('github', 'Open on GitHub')}</div></section>`;
    return hero + `<h2>Updates</h2><section class="grp">${row1}${notes}</section><p class="ab-foot">${foot}</p>` + links
      + troubleshooting() + '<p class="ab-lic">MIT License · © 2026 Asif Shirazi</p>'; // T-029 (owner): Log folder lives in About now
  }

  // ---- Automation pane ----
  // Change 3: no switch at the top, one line of intro (or the helper); each device row has its own switch.
  const automation = a => `<p class="intro">${esc(a.hint || 'When the grid goes off or comes back, each device that’s on switches after its delay.')}</p>`;
  function devices(s) {
    const rows = s.devices.map((d, i) => {
      // T-029 (owner): the status is a dot before the name, its words in the tooltip (title, drawn by macOS) and for
      // VoiceOver (aria-label, plus a hidden live line). T-030 (owner), online status first: spinner = being set ·
      // yellow = waiting out its delay · red = offline, not in Tuya, or the last change failed · green = online in Tuya ·
      // empty ring only while the online state isn't known yet.
      const [kind, text] = d.setting ? ['busy', d.setting] : d.waiting ? ['wait', d.waiting] : d.offline ? ['bad', 'Offline in Tuya']
        : d.missing ? ['bad', 'Not in your Tuya account'] : d.last && d.last.failed ? ['bad', d.last.text]
        : d.online ? ['ok', d.last ? 'Online · ' + d.last.text : 'Online in Tuya'] : d.last ? ['ok', d.last.text] : ['none', ''];
      const dot = kind === 'none' ? '<i class="st-dot is-none" aria-hidden="true"></i>'
        : `<i class="st-dot is-${kind}" role="img" aria-label="${esc(text)}" title="${esc(text)}">${kind === 'busy' ? '<span class="spin" aria-hidden="true"></span>' : ''}</i>`;
      const status = text ? `<span class="vh" role="status">${esc(text)}</span>` : '';
      const sel = (which, label, pos) => `<select data-k="${which}-${i}" data-device="${i}" data-which="${which}" aria-label="${label}, ${esc(d.name)}">${d.choices.map((c, k) => `<option value="${k}"${k === pos ? ' selected' : ''}>${esc(c)}</option>`).join('')}</select>`;
      // D-Y13: a saved delay that isn't in the list (an edited file) gets its own item, in order, so the pop-up never shows a value that isn't saved.
      const ds = d.delay_s || 0, dl = DELAYS.some(([v]) => v === ds) ? DELAYS
        : [...DELAYS, [ds, ds < 60 || ds % 60 ? ds + ' s' : ds / 60 + ' min']].sort((a, b) => a[0] - b[0]);
      const delay = `<select data-k="delay-${i}" data-device="${i}" data-delay aria-label="Delay, ${esc(d.name)}">${dl.map(([v, t]) => `<option value="${v}"${v === ds ? ' selected' : ''}>${t}</option>`).join('')}</select>`;
      const ctl = d.loading ? `<span class="wait" role="status"><span class="spin" aria-hidden="true"></span>Loading what it can do…</span>` : sel('off', 'Grid off', d.off) + sel('on', 'Grid on', d.on) + delay;
      return `<div class="row lt${d.enabled || d.loading ? '' : ' off'}"><span class="nm">${d.loading ? '' : dot}<b title="${esc(d.name)}">${esc(d.name)}</b></span>${ctl}`
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
    // T-029 (owner): Refresh (icon only) and Add Device… sit on the Devices heading's line, top right; a load error keeps
    // its own line under the list.
    const busy = s.add === 'loading' || !!s.refreshing; // T-030: ↻ spins while the list loads
    const refresh = `<button type="button" class="icon-btn${busy ? ' is-busy' : ''}" data-k="refresh-devices" data-action="refresh-devices" aria-label="Refresh device list" title="Refresh device list"${busy ? ' aria-disabled="true" aria-busy="true"' : ''}>${REFRESH}</button>`;
    return `<div class="h2row"><h2 id="devices-h">Devices</h2><div class="acts" role="group" aria-labelledby="devices-h">${refresh}${add}</div></div><section class="grp devices">${head}`
      + (s.addErr ? `<div class="row"><span class="err" role="alert">${esc(s.addErr)}</span></div>` : '') + `</section>`
      // T-029 (owner): the Delay note sits under a hairline, with an info glyph on its left.
      + (s.devices.length ? `<p class="foot has-info">${INFO.replace('<svg', '<svg aria-hidden="true"')}<span>Delay: how long a device waits after the grid changes, both ways. If the grid changes back first, the waiting change is dropped.</span></p>` : '');
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
  const DAYW = 476; // T-029 strip width: window 560 − well margins 16 − well padding 36 − Stats panel padding 32
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
    const sum = `<div class="st-sum"><p>${num(st.summary)}</p>${strip(st)}</div>`;
    // Footer: the note (where the numbers come from, or why this day is special), then one status line.
    const b = st.backfill, pr = st.problem;
    const status = pr ? `<p class="st-line is-warn" role="status">${WARN}<span>${esc(pr.text)}</span>${pr.action ? btn(pr.action[0], pr.action[1]) : ''}</p>`
      : b && b.paused ? `<p class="st-line" role="status"><span>${esc(b.paused)}</span></p>`
      : b ? `<p class="st-line" role="status"><span>Loading history: ${b.done} of ${b.total} days</span><span class="bar" role="progressbar" aria-label="Loading history" aria-valuemin="0" aria-valuemax="${b.total}" aria-valuenow="${b.done}"><i style="width:${(b.done / b.total * 100).toFixed(1)}%"></i></span></p>` : '';
    return head + `<section class="grp st">${sum}${table}</section><div class="st-foot">${st.note ? `<p class="foot">${esc(st.note)}</p>` : ''}${status}</div>`;
  }

  function renderSettings(s, now) {
    const pane = PANES.some(p => p[0] === s.pane) ? s.pane : 'general', title = PANES.find(p => p[0] === pane)[1];
    // D-Y8: roving tabindex (only the selected tab is in the Tab order); ← → select and show the next pane at once.
    const tabs = PANES.map(([id, label, icon]) => `<button type="button" role="tab" class="tab" data-k="tab-${id}" data-action="pane" data-pane="${id}" aria-selected="${id === pane}" tabindex="${id === pane ? 0 : -1}" id="tab-${id}" aria-controls="pane"${id === 'about' && pending(s.about) ? ' aria-description="Update available"' : ''}>${icon}<span>${label}</span>${id === 'about' && pending(s.about) ? '<i class="dot" aria-hidden="true"></i>' : ''}</button>`).join('');
    const body = pane === 'general'
      ? startup(s.general) + menuBar(s.general) + notifications(s.general) + account(s.account) // T-029 (owner): Troubleshooting moved to About
      : pane === 'stats' ? stats(s.stats)
      : pane === 'about' ? about(s.about, now)
      : automation(s.automation) + (s.tuya.connected || s.devices.length ? devices(s) : '') + tuya(s.tuya);
    // D-O11: the title strip is empty (the native title bar draws the traffic lights and the title). D-O10: the strip, the
    // toolbar's gaps and (T-029) the row round the segmented control drag the window (data-drag, see below); the tab buttons
    // don't carry it, so they stay clickable.
    return `<div class="win" role="dialog" aria-label="${title}"><div class="chrome" data-drag><div class="titlebar" data-drag></div>`
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
  const FOCUS_NEXT = { signout: 'signin', signin: 'signout', connect: 'add', aid: 'add', sec: 'add', dc: 'add', disconnect: 'aid', install: 'cancel', cancel: 'install', notes: 'check' };
  let state = null, last = '', height = 0, hold = 0, queued = null, focusTo = null;
  const DAY0 = '\u0000day'; // T-015: focus the calendar's roving day (calendar.focus, else the selected day) after the render

  // T-029: + the well's bottom margin (the frame under it), or the window would cut it and the pane would scroll by 8 pt.
  const natural = () => { const c = el.querySelector('.chrome'), b = el.querySelector('.body'); return c && b ? c.offsetHeight + b.scrollHeight + parseFloat(getComputedStyle(b).marginBottom) : 0; };
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
    document.documentElement.classList.toggle('reduce-motion', !!(s.general && s.general.reduceMotion)); // B-Y4: tokens.css stills
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
    // T-029: a new pane fades in (160 ms) while Rust eases the window's height; not on a redraw of the same pane, and not with Reduce motion.
    if (!open && was && was.pane !== s.pane && !(s.general && s.general.reduceMotion) && matchMedia('(prefers-reduced-motion: no-preference)').matches)
      el.querySelector('.body').animate([{ opacity: 0 }, { opacity: 1 }], { duration: 160, easing: 'cubic-bezier(.23,1,.32,1)' });
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
    if (b.tagName === 'A') e.preventDefault(); // How to get keys, Release notes: Rust opens the browser
    if (id === 'pane') return pane(b.dataset.pane);
    if (id === 'connect') return connect();
    // T-015 Stats: the day and calendar actions carry their step or day; Rust answers with the new state.
    if (id === 'day' || id === 'cal-month') return act({ id: id.replace('-', '_'), step: +b.dataset.step });
    if (id === 'cal-day') { focusTo = 'calendar'; return act({ id: 'cal_day', day: +b.dataset.day }); }
    if (id === 'calendar') focusTo = state.stats.calendar ? 'calendar' : DAY0; // closing: back to the button; opening: the selected day
    if (id === 'enable' || id === 'login' || id === 'temps' || id === 'cuts' || id === 'motion') {
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
    // T-022: Return presses the window's default button (Update and Relaunch) unless a control has the key.
    const def = e.key === 'Enter' && state && state.pane === 'about' && !e.target.closest('button, a, select, input') && el.querySelector('.btn.def');
    if (def) { e.preventDefault(); return def.click(); }
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
