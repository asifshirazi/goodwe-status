/* GoodWe Status panel renderer. No framework: GW.renderPanel(state) returns the panel's HTML,
   GW.bindChart(rootEl) wires the chart's hover / keyboard read-out after each render (T-002).
   The Rust side sends `state` (already formatted strings, see the spec) on every poll and when the panel opens.

   state = {
     status:   { kind: 'on'|'off'|'unknown'|'first'|'signedout', title, detail?, back?, next? },
               // T-002 detail (grid off, from SEMS+ history): 'Since 9:07 AM (2h 15m)' · 'Since yesterday 11:33 PM (1h 7m)'
               //   · 'Since 7:42 AM (38 min)' (history can't tell, but the app saw it go off)
               //   · 'Since at least 11:58 AM' (history can't tell and the app didn't see it go off)
               //   · 'Since at least 12:00 AM' · 'Since at least yesterday 12:00 AM' · absent while the first history call runs.
               // T-002 back: 'Usually back around 12:05 PM (last 14 days)' only while off and the days agree; else absent.
               //   T-023: once that time passes with the grid still off, the same line stays until the grid is back.
               // T-002 next (grid on): two lines, e.g. ['Next cut usually around 1:35 PM', 'back around 4:30 PM (last 14 days)']; else absent.
               // T-029 detail (grid on, no next): 'Since 12:04 PM', the Grid on time of today's last finished cut; no cut today: absent.
     refreshing: bool,   // T-002: a poll the user started (click, ⌘R, sign-in) is running; the icon spins. Timer polls: false.
     battery:  null | { soc: '86%', power: 'Discharging 998 W', left?: { time: '~4h 20m', rest: 'left at 998 W' } },  // T-029: left.rest in the power line's unit
     temps:    null | { inverter?: Temp, battery?: Temp },  // T-015: absent = that pair hidden; null = section hidden
               //   (the Settings switch is off, or neither reading is real).
               // Temp = { text: '36°C' (whole degrees, U+2212 minus), level: 0..1 (place on its scale),
               //          state: 'ok' | 'warm' | 'hot' | 'cold', trend?: 'up' | 'down' (±2 °C over 30 min of own readings) }
     updateAvailable: bool, // T-022: a newer release waits (available → failed): the footer gear gets a dot and opens Settings › About
     reduceMotion: bool,    // T-029: Settings › Reduce motion; the panel opens without its motion (macOS Reduce motion also stops it)
     updated:  null | { stale: bool, text: 'Updated 8:20 AM' | 'No new data since 8:20 AM' },  // T-024: shown in the footer; while refreshing it reads Refreshing…
     notificationsOff: bool,
     chart:    null | { status: 'ok'|'loading'|'empty'|'failed'|'fallback', since?: '7:58 AM' (fallback, only when the app started today),
                        day: '18 Oct' (T-002, station-local date for the hover read-out),
                        gap?: minutes (5 for SEMS+, 12 for own readings; default by status),
                        now: minuteOfDay, points: [[minuteOfDay, soc, kW], ...] },   // kW > 0 = discharging; x runs 12 AM to now
     cuts:     null | { status: 'ok'|'loading'|'unsaved', summary: 'So far today: 2 power cuts, off for 1h 26m.', msg?: 'Loading 19 October from SEMS…',
                        strip: { from?, to?, cuts: [[a, b], ...], gaps?: [[a, b], ...] }, rows: [Row, ...] },
               // T-020: today's Stats view (stats::view for today, whatever day Settings shows), drawn as the Stats strip at the
               // bottom of the Grid section. null = the Settings switch is off, signed out, or no battery (no cut history).
     signedIn: bool
   }
   Times are station-local minutes of today. Keep showing the last series while a refresh runs; 'loading' only before the first. */
(function (root) {
  'use strict';
  let uid = 0;
  const CHARTS = {}; // T-002: geometry + points of the live plot, keyed by its id, for bindChart; cleared on every render
  const IDLE_KW = 0.05; // same as rules.rs IDLE_WATTS (50 W, director 2026-10-18)
  const AX_CLEAR = 68; // T-003: widest end label '12:00 AM' 47.4 px + half of '12 AM' 16.3 + 4 gap (was 48 for 24 h)
  const TIP_GAP = 8, TIP_PAD = 12; // card 8 pt from the line, may reach 12 pt (--gw-s3) into the section padding
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  // T-029: numbers, times and units inside a sentence are set in SF Mono (.num); the words stay SF Pro. Rust's strings are unchanged.
  const NUM = /~?\d(?:[\d:.,]*\d)?(?:[\u00a0 ]?(?:AM|PM|kW|°C|min|W|h|m|s|%)(?![A-Za-z]))?/g;
  const num = s => esc(s).replace(NUM, m => `<span class="num">${m}</span>`);

  const PLUG = '<rect x="5.25" y="1" width="1.75" height="4.75" rx=".875"/><rect x="11" y="1" width="1.75" height="4.75" rx=".875"/>'
    + '<path d="M4.5 5H13.5A1.25 1.25 0 0 1 14.75 6.25V8.75C14.75 11.1 12.9 13 10.6 13H9.75V16.25A.75 .75 0 0 1 8.25 16.25V13H7.4C5.1 13 3.25 11.1 3.25 8.75V6.25A1.25 1.25 0 0 1 4.5 5Z"/>';
  const S = 'fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"';
  const ICONS = {
    plug: () => `<svg viewBox="0 0 18 18" fill="currentColor">${PLUG}</svg>`,
    plugoff: () => { const id = 'gwk' + (++uid); return `<svg viewBox="0 0 18 18" fill="currentColor"><defs><mask id="${id}" maskUnits="userSpaceOnUse" x="0" y="0" width="18" height="18"><rect width="18" height="18" fill="#fff"/><path d="M2.25 2.25L15.75 15.75" stroke="#000" stroke-width="4.1" stroke-linecap="round"/></mask></defs><g mask="url(#${id})">${PLUG}</g><path d="M2.25 2.25L15.75 15.75" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>`; },
    battery: () => `<svg viewBox="0 0 16 16" ${S}><rect x="1.5" y="4.5" width="11" height="7" rx="1.8"/><path d="M14.5 7v2"/></svg>`,
    clock: () => `<svg viewBox="0 0 16 16" ${S}><circle cx="8" cy="8" r="6"/><path d="M8 4.8V8l2.2 1.4"/></svg>`,
    warn: () => `<svg viewBox="0 0 16 16" ${S}><path d="M8 2.2l6.2 11H1.8z"/><path d="M8 6.6v3"/><path d="M8 11.5v.1"/></svg>`,
    bellslash: () => `<svg viewBox="0 0 16 16" ${S}><path d="M4 11.2V7.6a4 4 0 0 1 6.9-2.8M12 7.6v3.6l1 1.3H5"/><path d="M6.6 14h2.8"/><path d="M2.5 2.5l11 11"/></svg>`,
    bell: () => `<svg viewBox="0 0 16 16" ${S}><path d="M4 11.2V7.6a4 4 0 0 1 8 0v3.6l1 1.3H3z"/><path d="M6.6 14h2.8"/></svg>`,
    refresh: () => `<svg viewBox="0 0 16 16" ${S}><path d="M13 8a5 5 0 1 1-1.5-3.6"/><path d="M12 1.8v2.9H9.1"/></svg>`,
    person: () => `<svg viewBox="0 0 16 16" ${S}><circle cx="8" cy="5.5" r="2.6"/><path d="M3 13.6c.8-2.4 2.7-3.6 5-3.6s4.2 1.2 5 3.6"/></svg>`,
    power: () => `<svg viewBox="0 0 16 16" ${S}><path d="M8 2v5.4"/><path d="M4.7 4.3a5 5 0 1 0 6.6 0"/></svg>`,
    // T-015: thermometer, same stroke family (SF Symbols' thermometer.medium, drawn on the 16 grid).
    thermo: () => `<svg viewBox="0 0 16 16" ${S}><path d="M6.4 9.3V3.6a1.6 1.6 0 0 1 3.2 0v5.7a2.9 2.9 0 1 1-3.2 0z"/><path d="M8 6.6v4.6"/></svg>`,
    gear: () => `<svg viewBox="0 0 16 16" ${S}><path d="M14.22 7.00L14.22 9.00L12.77 9.13L12.17 10.57L13.11 11.69L11.69 13.11L10.57 12.17L9.13 12.77L9.00 14.22L7.00 14.22L6.87 12.77L5.43 12.17L4.31 13.11L2.89 11.69L3.83 10.57L3.23 9.13L1.78 9.00L1.78 7.00L3.23 6.87L3.83 5.43L2.89 4.31L4.31 2.89L5.43 3.83L6.87 3.23L7.00 1.78L9.00 1.78L9.13 3.23L10.57 3.83L11.69 2.89L13.11 4.31L12.17 5.43L12.77 6.87Z"/><circle cx="8" cy="8" r="2.1"/></svg>`,
  };
  const ARROW = { up: '<svg viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M5 8.5V1.8M2.2 4.4L5 1.6l2.8 2.8"/></svg>',
    down: '<svg viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M5 1.5v6.7M2.2 5.6L5 8.4l2.8-2.8"/></svg>' };
  const ic = (name, cls = '') => `<span class="gw-ic ${cls}" aria-hidden="true">${ICONS[name]()}</span>`;

  // Chart: x 00:00 to now (chart.now, minute of day), left axis kW (symmetric, so 0 kW sits on the 50 % line), right axis SOC %.
  function chartSVG(chart) {
    let plotId = '';
    const W = 304, Hh = 140, L = 26, R = 30, T = 16, B = 26, w = W - L - R, h = Hh - T - B; // T-029: B holds the time band
    const now = chart.now != null ? chart.now : 0;
    // Fallback since launch only when the app started today (chart.since set); otherwise the day from 00:00.
    const all = (chart.points || []).filter(p => p && p[1] != null && p[2] != null && p[0] >= 0 && p[0] <= now);
    const start = chart.status === 'fallback' && chart.since && all.length ? all[0][0] : 0;
    const pts = all.filter(p => p[0] >= start);
    const maxKw = Math.max(1, Math.ceil(Math.max(0, ...pts.map(p => Math.abs(p[2])))));
    const msg = { loading: 'Loading today’s chart…', empty: 'No readings yet today', failed: 'Today’s history isn’t available right now' }[chart.status]
      || (pts.length < 2 ? 'No readings yet today' : '');
    const end = Math.max(now, start + 2);
    const x = m => L + ((m - start) / (end - start)) * w, ys = s => T + (1 - s / 100) * h, yk = k => T + (1 - (k + maxKw) / (2 * maxKw)) * h;
    const fmt = k => (k < 0 ? '−' : '') + Math.abs(k);
    // 12 h: '1:36 PM' (no leading zero; 0 → 12 AM; a no-break space keeps AM/PM with its time, as in Rust's hhmm12).
    // Round first so 59.5 can't print ':60'. hr: whole-hour axis label, '6 AM'.
    const ap = h => (h % 12 || 12) + '', ampm = h => (h % 24 < 12 ? '\u00a0AM' : '\u00a0PM');
    const hm = m => { m = Math.round(m); const h = Math.floor(m / 60); return ap(h) + ':' + String(m % 60).padStart(2, '0') + ampm(h); };
    const hr = m => ap(m / 60) + ampm(m / 60);
    let g = '';
    if (msg) {
      // No data to plot: a quiet frame and the message; no value or time axes (they would mean nothing).
      g += `<line class="grid" x1="${L}" x2="${L + w}" y1="${T}" y2="${T}"/><line class="grid" x1="${L}" x2="${L + w}" y1="${T + h}" y2="${T + h}"/>`;
      g += `<text class="msg" x="${L + w / 2}" y="${T + h / 2 + 4}">${esc(msg)}</text>`;
    } else {
      // T-029: graph paper under the lines (8 pt squares), and the time labels in an inset band under the plot.
      const gp = 'gwp' + (++uid), by = T + h + 6, ty = by + 12;
      g += `<defs><pattern id="${gp}" x="${L}" y="${T}" width="${w / 32}" height="${h / 12}" patternUnits="userSpaceOnUse"><path class="paper" d="M${w / 32} 0H0V${h / 12}"/></pattern></defs>`
        + `<rect x="${L}" y="${T}" width="${w}" height="${h}" fill="url(#${gp})"/><rect class="band" x="${L - 6}" y="${by}" width="${w + 12}" height="18" rx="5"/>`;
      [0, .5, 1].forEach(f => { g += `<line class="${f === .5 ? 'zero' : 'grid'}" x1="${L}" x2="${L + w}" y1="${T + f * h}" y2="${T + f * h}"/>`; });
      g += `<text class="ax" x="${L - 4}" y="${T - 6}" text-anchor="end">kW</text><text class="ax" x="${L + w + 4}" y="${T - 6}">%</text>`;
      [[maxKw, 0], [0, .5], [-maxKw, 1]].forEach(([k, f]) => { g += `<text class="ax" x="${L - 4}" y="${T + f * h + 3.5}" text-anchor="end">${fmt(k)}</text>`; });
      [[100, 0], [50, .5], [0, 1]].forEach(([s, f]) => { g += `<text class="ax" x="${L + w + 4}" y="${T + f * h + 3.5}">${s}</text>`; });
      // Time labels: start, whole hours that fit, and now at the right end.
      // An hour label needs AX_CLEAR px from the end labels (half its width + a full label + a gap), or they touch.
      const step = (end - start) <= 240 ? 60 : (end - start) <= 720 ? 180 : 360;
      g += `<text class="ax" x="${x(start)}" y="${ty}">${hm(start)}</text>`;
      for (let t = Math.ceil((start + 1) / step) * step; t < end; t += step) if (x(t) - x(start) >= AX_CLEAR && x(end) - x(t) >= AX_CLEAR) g += `<text class="ax" x="${x(t)}" y="${ty}" text-anchor="middle">${hr(t)}</text>`;
      g += `<text class="ax is-now" x="${x(end)}" y="${ty}" text-anchor="end">${hm(end)}</text>`;

      // Break the lines where no reading came for more than GAP minutes: never draw values nobody measured.
      // Threshold follows the source: SEMS+ points every 1–2 min → 5; the app's own readings (one per new info.time, ~5 min) → 12.
      const GAP = chart.gap || (chart.status === 'fallback' ? 12 : 5), segs = [];
      pts.forEach((p, i) => { if (!i || p[0] - pts[i - 1][0] > GAP) segs.push([p]); else segs[segs.length - 1].push(p); });
      const path = (seg, f) => seg.map((p, i) => `${i ? 'L' : 'M'}${x(p[0]).toFixed(1)} ${f(p).toFixed(1)}`).join('');
      const z = yk(0).toFixed(1), clip = 'gwc' + (++uid);
      let d = `<clipPath id="${clip}"><rect x="${L - 3}" y="${T - 3}" width="${w + 6}" height="${h + 6}"/></clipPath><g clip-path="url(#${clip})"><g class="lines">`; // T-029: .lines is what the opening motion wipes in
      segs.forEach(seg => {
        const kw = path(seg, p => yk(p[2]));
        d += `<path class="kwfill" d="${kw}L${x(seg[seg.length - 1][0]).toFixed(1)} ${z}L${x(seg[0][0]).toFixed(1)} ${z}Z"/><path class="kw" d="${kw}"/>`;
        d += seg.length > 1 ? `<path class="soc" d="${path(seg, p => ys(p[1]))}"/>` : `<circle cx="${x(seg[0][0])}" cy="${ys(seg[0][1])}" r="1.5" fill="var(--gw-soc)"/>`;
      });
      const last = pts[pts.length - 1];
      d += `<g class="now"><circle cx="${x(last[0])}" cy="${yk(last[2])}" r="2.5" fill="var(--gw-kw)"/><circle cx="${x(last[0])}" cy="${ys(last[1])}" r="3" fill="var(--gw-soc)"/></g></g></g>`;
      // T-002 read-out cursor: hidden until hover or keyboard focus; bindChart moves it.
      d += `<g class="cur" visibility="hidden"><line class="cur-line" x1="0" x2="0" y1="${T}" y2="${T + h}"/>`
        + `<circle class="cur-kw" r="3.5" cx="0" cy="0"/><circle class="cur-soc" r="4" cx="0" cy="0"/></g>`;
      g += d;
      CHARTS[clip] = { L, w, T, h, start, end, GAP, pts, x, ys, yk, hm, day: chart.day || '' };
      plotId = clip;
    }
    const label = chart.status === 'ok' || chart.status === 'fallback'
      ? `Battery today: ${pts.length ? pts[pts.length - 1][1] + '% now' : ''}` : (msg || '');
    const svg = `<svg viewBox="0 0 ${W} ${Hh}" aria-hidden="true">${g}</svg>`;
    // T-002: a plot with data is one focusable slider over time (HIG Charts: whole plot is the target; keyboard can move it).
    if (!plotId) return `<div class="gw-plot" role="img" aria-label="${esc(label)}">${svg}</div>`;
    return `<div class="gw-plot" data-plot="${plotId}" tabindex="0" role="slider" aria-label="${esc(label)}. Use left and right arrows to read the values at a time."`
      + ` aria-valuemin="${start}" aria-valuemax="${end}" aria-valuenow="${pts[pts.length - 1][0]}">${svg}<div class="gw-tip" hidden></div></div>`;
  }

  // ---- T-002 chart read-out (hover + keyboard) ----
  // Same wording and units as the Battery row (rules.rs fmt_power): whole watts below 1000 W, else kW with one decimal.
  const kwRow = k => {
    if (Math.abs(k) < IDLE_KW) return ['Idle', ''];
    const w = Math.round(Math.abs(k) * 1000);
    return [k > 0 ? 'Discharging' : 'Charging', w < 1000 ? w + ' W' : (Math.abs(k)).toFixed(1) + ' kW'];
  };
  function nearest(pts, m) { // pts are sorted by minute
    let lo = 0, hi = pts.length - 1;
    while (hi - lo > 1) { const mid = (lo + hi) >> 1; if (pts[mid][0] < m) lo = mid; else hi = mid; }
    return Math.abs(pts[lo][0] - m) <= Math.abs(pts[hi][0] - m) ? pts[lo] : pts[hi];
  }
  // Show the cursor at minute m (snapped to the nearest reading). No reading within GAP → line + "No reading", no dots.
  function showAt(el, m) {
    const c = CHARTS[el.dataset.plot]; if (!c) return;
    m = Math.max(c.start, Math.min(c.end, m));
    const p = nearest(c.pts, m), hit = Math.abs(p[0] - m) <= c.GAP / 2 + 1;
    const at = hit ? p[0] : Math.round(m), cx = c.x(at);
    const cur = el.querySelector('.cur'), tip = el.querySelector('.gw-tip');
    cur.setAttribute('visibility', 'visible');
    cur.querySelector('.cur-line').setAttribute('x1', cx); cur.querySelector('.cur-line').setAttribute('x2', cx);
    [['.cur-soc', hit && c.ys(p[1])], ['.cur-kw', hit && c.yk(p[2])]].forEach(([s, cy]) => {
      const d = cur.querySelector(s); d.setAttribute('visibility', hit ? 'visible' : 'hidden'); if (hit) { d.setAttribute('cx', cx); d.setAttribute('cy', cy); }
    });
    el.querySelector('.now').setAttribute('visibility', 'hidden');
    const when = (c.day ? c.day + ' ' : '') + c.hm(at), [kl, kv] = hit ? kwRow(p[2]) : [];
    tip.innerHTML = `<p class="gw-tip-time">${esc(when)}</p>` + (hit
      ? `<p class="gw-tip-row"><i class="soc"></i><span>Battery</span><b>${p[1]}%</b></p><p class="gw-tip-row"><i class="kw"></i><span>${kl}</span><b>${kv}</b></p>`
      : `<p class="gw-tip-row gw-tip-none">No reading</p>`);
    tip.hidden = false;
    // Beside the line, right if it fits, else left; top-aligned with the plot. It may reach TIP_PAD into the section's
    // padding on either side. The card is at most 156 pt wide (= (304 + 2·12 − 2·8) / 2), so one side always fits. The svg is drawn 1:1.
    const k = el.clientWidth / 304, tw = tip.offsetWidth, px = cx * k;
    tip.style.left = (px + TIP_GAP + tw <= el.clientWidth + TIP_PAD ? px + TIP_GAP : Math.max(-TIP_PAD, px - TIP_GAP - tw)) + 'px';
    tip.style.top = (c.T * k) + 'px';
    el.setAttribute('aria-valuenow', at);
    el.setAttribute('aria-valuetext', hit ? `${when}, battery ${p[1]}%, ${kl.toLowerCase()}${kv ? ' ' + kv : ''}` : `${when}, no reading`);
    el.dataset.at = at;
  }
  function hide(el) {
    el.querySelector('.cur').setAttribute('visibility', 'hidden');
    el.querySelector('.now').setAttribute('visibility', 'visible');
    el.querySelector('.gw-tip').hidden = true;
  }
  function bindChart(rootEl) {
    bindStrip(rootEl, lastCuts); // T-020
    rootEl.querySelectorAll('.gw-plot[data-plot]').forEach(el => {
      const c = CHARTS[el.dataset.plot], toMin = e => {
        const r = el.getBoundingClientRect(), vx = (e.clientX - r.left) * 304 / r.width;
        return c.start + (vx - c.L) / c.w * (c.end - c.start);
      };
      el.addEventListener('pointermove', e => showAt(el, toMin(e)));
      el.addEventListener('pointerdown', e => e.preventDefault()); // a click never focuses the plot; only Tab does
      el.addEventListener('pointerleave', () => el.matches(':focus-visible') ? showAt(el, +el.dataset.kb) : hide(el));
      el.addEventListener('focus', () => { el.dataset.kb = c.pts[c.pts.length - 1][0]; showAt(el, +el.dataset.kb); });
      el.addEventListener('blur', () => hide(el));
      el.addEventListener('keydown', e => {
        const step = { ArrowLeft: -5, ArrowRight: 5 }[e.key], m = +el.dataset.kb, P = c.pts;
        let to = null;
        if (step) {
          // Snap to the reading nearest the step; if that is the reading we are on, take the next one that way,
          // so each press moves at least one reading (own readings can be 10–12 min apart).
          const dir = Math.sign(step), i = P.findIndex(p => p[0] === m);
          let q = nearest(P, m + step * (e.shiftKey ? 12 : 1));
          if (q[0] === m || Math.sign(q[0] - m) !== dir) q = P[Math.max(0, Math.min(P.length - 1, i + dir))];
          to = q[0];
        } else if (e.key === 'Home') to = P[0][0]; else if (e.key === 'End') to = P[P.length - 1][0];
        if (to == null) return;
        e.preventDefault(); e.stopPropagation();
        showAt(el, to); el.dataset.kb = el.dataset.at;
      });
    });
  }

  // ---- T-020 cut strip: the Stats day strip (settings.js › strip) at the panel's text width ----
  const SW = 300; // T-029: the well's content width (340 − 2 × 6 frame − 2 × 14 padding)
  // T-025: the strip itself is the app's ui/strip.js (index.html loads it), so the design shows every part's card.
  function stripSVG(cu) {
    const label = cu.status === 'ok' ? cu.summary : cu.msg, svg = GWStrip.svg(cu.strip, SW), n = svg.includes('class="hit"');
    // T-023: the whole strip is a button (click, Return, Space) to Settings › Stats on today; ← → still read the cuts.
    const desc = `${label || ''}${n ? ' Use left and right arrows to read each part of the day.' : ''}`;
    return `<div class="gw-strip"><div class="gw-strip-plot" tabindex="0" role="button" data-action="stats" aria-label="Show power cuts in Stats" aria-describedby="gw-strip-d">`
      + `${svg}${n ? '<div class="gw-tip" role="status" hidden></div>' : ''}<span id="gw-strip-d" hidden>${esc(desc)}</span></div></div>`;
  }
  // Same read-out as Stats: point at a cut, or Tab to the strip and use ← → / Home / End. The card is the chart's; it sits
  // under the strip (over the hour labels), never over the lines above, so it can't leave the panel at its top.
  function bindStrip(rootEl, cu) {
    const plot = rootEl.querySelector('.gw-strip-plot'); if (!plot || !cu) return;
    if (plot.querySelector('.gw-tip')) GWStrip.bind(plot, cu.rows || [], SW, { tip: 'gw-tip', left: -12 });
    // T-023: the press fill by class, since the read-out cancels pointerdown (no focus on click) and with it :active.
    plot.addEventListener('pointerdown', () => plot.classList.add('is-press'));
    ['pointerup', 'pointerleave', 'pointercancel'].forEach(n => plot.addEventListener(n, () => plot.classList.remove('is-press')));
  }
  let lastCuts = null; // the strip's rows, for bindChart after a render

  function renderPanel(st) {
    for (const id in CHARTS) delete CHARTS[id]; // one live panel: drop the previous render's plot data
    const s = st.status, out = [];
    // Stale never shows the live red alarm.
    const statusIcon = { on: ic('plug', st.updated && st.updated.stale ? 'is-strong' : 'is-green'), off: ic('plugoff', st.updated && st.updated.stale ? 'is-strong' : 'is-red'), unknown: ic('plug'), first: ic('plug'), signedout: ic('plug') }[s.kind];
    // T-002: Refresh is an icon button on the first row (top right), replacing the "Refresh Now" row.
    const busy = !!st.refreshing, off = !st.signedIn;
    const refresh = `<button class="gw-icon-btn${busy ? ' is-busy' : ''}" data-action="refresh" aria-label="${busy ? 'Refreshing' : 'Refresh'}" title="${busy ? 'Refreshing…' : 'Refresh (⌘R)'}"`
      + `${off ? ' aria-disabled="true" tabindex="-1"' : busy ? ' aria-disabled="true" aria-busy="true"' : ''}>${ic('refresh')}</button>`;
    // T-023: no info button on the back-on line any more; the cut strip itself opens Settings › Stats on today.
    lastCuts = st.signedIn && st.cuts ? st.cuts : null;
    // T-029: the card header: an icon tile (the grid's state colour), the title, and the first line under it as the subtitle;
    // Refresh on the right. The outlook lines and today's strip get their own section under a full-width hairline.
    const tile = { on: 'is-on', off: 'is-off' }[s.kind] && !(st.updated && st.updated.stale) ? ' is-' + s.kind : '';
    // Owner (T-029): Grid on gets a second line too. With no detail line, the first next-cut line is the subtitle and the
    // rest of that estimate stays in the outlook, its first letter capitalised ('Back around 4:30 PM (last 14 days)').
    const next = s.next || [], sub = s.detail || (s.kind === 'on' && next.length ? next[0] : '');
    const rest = s.detail ? next : next.slice(1).map(l => l.replace(/^./, c => c.toUpperCase()));
    out.push(`<header class="gw-head"><span class="gw-tile${tile}">${statusIcon}</span><div class="gw-head-tx"><h1 class="gw-title">${esc(s.title)}</h1>`
      + `${sub ? `<p class="gw-sub">${num(sub)}</p>` : ''}</div>${refresh}</header>`);
    const outlook = [s.back, ...rest].filter(Boolean);
    if (outlook.length || lastCuts) out.push(`<section class="gw-sec gw-outlook">${outlook.map(l => `<p class="gw-line">${num(l)}</p>`).join('')}`
      + (lastCuts ? stripSVG(lastCuts) : '') + `</section>`);

    const b = st.battery;
    // T-029 (owner, second references): battery as a hero. The % large in SF Pro, under it 'Battery · Discharging 640 W'
    // (the power string with its 'Battery ' dropped), then the charge as 20 cells of 5 % (owner: a battery meter, not a solid line), coloured by
    // level (owner): under 30 % red-pink, 30–49 % yellow, 50–79 % blue, 80 % and up green, then the time left:
    // the time in SF Mono, its words muted ('~6h 50m left at 0.6 kW').
    // T-034: while charging, a mint dot before "Charging" pulses softly (still while stale or under Reduce motion), and
    // the time-left line says the time to full ('~1h 10m to full at 3.8 kW').
    if (b) {
      const pw = b.power.replace(/^Battery /, '').replace(/^./, c => c.toUpperCase()), lv = Math.max(0, Math.min(100, parseFloat(b.soc) || 0));
      out.push(`<section class="gw-sec gw-bat" aria-label="Battery"><div class="gw-hero"><b>${esc(b.soc)}</b><p>Battery · ${b.charging ? `<i class="gw-chg${st.updated && st.updated.stale ? ' is-still' : ''}" aria-hidden="true"></i>` : ''}${num(pw)}</p></div>`
        + `<span class="gw-cells is-${lv < 30 ? 'low' : lv < 50 ? 'mid' : lv < 80 ? 'high' : 'full'}" aria-hidden="true">${Array.from({ length: 20 }, (_, i) => `<i${i < Math.round(lv / 5) ? ` class="on" style="--i:${i}"` : ''}></i>`).join('')}</span>`
        + (b.left ? `<p class="gw-left"><b>${esc(b.left.time)}</b> ${num(b.left.rest)}</p>` : '') + `</section>`);
    }

    // T-015: one quiet row under Battery, Inverter | Battery. A missing reading leaves its column empty; none drops the row.
    // Each reading answers "is it fine?": a heat bar on its scale, a word and a colour only when it isn't (Warm, Hot, Cold),
    // and an arrow when it moved 2 °C or more in 30 min. Stale data shows no arrow (an old value has no trend).
    const t = st.temps, WORD = { warm: 'Warm', hot: 'Hot', cold: 'Cold' }, stale = st.updated && st.updated.stale;
    // T-029 (owner): the two readings side by side on one line, split by a hairline (boxes took too much room): the name,
    // the value in SF Mono and the trend, then the heat bar with the word at its end when it isn't fine (Warm, Hot, Cold). A missing reading leaves its half empty, so
    // nothing moves when it comes or goes; none drops the section.
    const box = (k, v) => {
      if (!v) return '<span></span>';
      const tr = v.trend && !stale ? `<span class="gw-trend" aria-label="${v.trend === 'up' ? 'rising' : 'falling'}">${ARROW[v.trend]}</span>` : '';
      const l = Math.round(Math.max(.04, Math.min(1, v.level)) * 100), lvl = WORD[v.state] ? v.state : 'ok';
      // The word sits at the end of the bar, so the name never has to shorten (a column is ~135 pt).
      return `<div class="gw-tbox is-${lvl}"><p class="gw-tbox-top"><span>${k}</span><b>${esc(v.text)}</b>${tr}</p>`
        + `<p class="gw-tbox-bar"><span class="gw-meter is-${k.toLowerCase()}" aria-hidden="true"><i style="width:${l}%;background-size:${(10000 / l).toFixed(0)}% 100%"></i></span>${WORD[v.state] ? `<em>${WORD[v.state]}</em>` : ''}</p></div>`;
    };
    if (t && (t.inverter || t.battery)) out.push(`<section class="gw-sec gw-temps" aria-label="Temperatures">${box('Inverter', t.inverter)}${box('Battery', t.battery)}</section>`);

    if (st.notificationsOff) out.push(`<section class="gw-sec gw-notif"><span class="gw-tile">${ic('bellslash', 'is-strong')}</span><div><p class="gw-notif-t">Notifications are off</p><p class="gw-sub">You won’t hear when the grid goes off.</p></div></section>`);

    const c = st.chart;
    if (c) {
      const cap = { fallback: 'SEMS+ history isn’t available, so this shows this app’s own readings.',
        failed: 'The chart fills in from this app’s readings, every 2 minutes.' }[c.status] || '';
      out.push(`<section class="gw-sec gw-chart"><div class="gw-chart-head"><span class="gw-label">${c.status === 'fallback' && c.since ? 'Since ' + esc(c.since) : 'Today'}</span>`
        + `<span class="gw-legend"><span><i style="background:var(--gw-soc)"></i>Battery %</span><span><i style="background:var(--gw-kw)"></i>Battery kW</span></span></div>`
        + chartSVG(c) + (cap ? `<p class="gw-caption">${cap}</p>` : '') + `</section>`);
    }

    // T-024: action rows only for what the panel needs right now (Turn On Notifications…, Sign In…); Settings and Quit
    // are icon buttons on one footer row with the last update between them (the old "Updated" section).
    const acts = [];
    if (st.notificationsOff) acts.push(['notif', 'bell', 'Turn On Notifications…']);
    if (!st.signedIn) acts.push(['signin', 'person', 'Sign In…']);
    if (acts.length) out.push(`<nav class="gw-actions" aria-label="Actions">${acts.map(([id, icon, t]) => `<button class="gw-act${id === 'signin' ? ' is-primary' : ''}" data-action="${id}">${ic(icon)}<span>${t}</span></button>`).join('')}</nav>`);
    const u = st.updated;
    const foot = (id, icon, label, tip, keys, dot) => `<button class="gw-icon-btn" data-action="${id}" aria-label="${label}" aria-keyshortcuts="${keys}" title="${tip}">${ic(icon)}${dot ? '<i class="gw-dot" aria-hidden="true"></i>' : ''}</button>`;
    // Empty while signed out and before the first reading: the first row already says so.
    const when = u && st.refreshing ? '<p class="gw-when"><span>Refreshing…</span></p>'
      : u ? `<p class="gw-when${u.stale ? ' is-stale' : ''}">${u.stale ? ic('warn') : ''}<span>${num(u.text)}</span></p>` : '<p class="gw-when"></p>';
    out.push(`<footer class="gw-foot">${st.updateAvailable ? foot('settings', 'gear', 'Settings, update available', 'Update available · Settings (⌘,)', 'Meta+,', true) : foot('settings', 'gear', 'Settings', 'Settings (⌘,)', 'Meta+,')}${when}${foot('quit', 'power', 'Quit GoodWe Status', 'Quit GoodWe Status (⌘Q)', 'Meta+Q')}</footer>`);

    // T-029: the header and the footer sit on the frame; everything between them sits in the inset well.
    const head = out.shift(), footer = out.pop();
    return `<div class="gw-panel" role="dialog" aria-label="GoodWe Status">${head}<div class="gw-well">${out.join('')}</div>${footer}</div>`;
  }

  // ---- T-029 (owner): the opening motion ----
  // Only when the panel opens (never on a poll or a refresh), one after another from the top: the strip's cuts grow from
  // the left, the battery % counts up as its cells fill, the heat bars grow, then the chart's lines wipe in (CSS,
  // .is-opening; 90 ms apart, ≈ 0.75 s in all), ease-out, and skipped when Settings › Reduce motion is on or macOS asks for reduced motion.
  function animateOpen(rootEl, o = {}) {
    const p = rootEl.querySelector('.gw-panel');
    if (!p || o.reduce || (root.matchMedia && root.matchMedia('(prefers-reduced-motion: reduce)').matches)) return;
    p.classList.remove('is-opening'); void p.offsetWidth; p.classList.add('is-opening');
    // Drop the class when every opening animation has finished (not on a timer, which cuts them short if they run slow).
    const end = () => p.classList.remove('is-opening');
    Promise.all(p.getAnimations({ subtree: true }).map(x => x.finished)).then(end, end);
    const b = p.querySelector('.gw-hero b'), m = b && /^(\d+)%$/.exec(b.textContent);
    if (!m) return;
    // On the document timeline, so the count runs in step with the CSS (and slows with it in DevTools' animation panel).
    const clock = () => document.timeline.currentTime, to = +m[1], t0 = clock(), ease = t => 1 - Math.pow(1 - t, 3);
    const tick = () => { const t = Math.max(0, Math.min(1, (clock() - t0 - 90) / 420)); /* starts with the battery cells, 90 ms in */ b.textContent = Math.round(to * ease(t)) + '%'; if (t < 1 && b.isConnected) requestAnimationFrame(tick); };
    requestAnimationFrame(tick);
  }

  root.GW = { renderPanel, chartSVG, bindChart, showAt, hide, animateOpen };
})(typeof window !== 'undefined' ? window : globalThis);
