/* GoodWe Status panel renderer. No framework: GW.renderPanel(state) returns the panel's HTML.
   The Rust side sends `state` (already formatted strings, see the spec) on every poll and when the panel opens.

   state = {
     status:   { kind: 'on'|'off'|'unknown'|'first'|'signedout', title, detail? },
     battery:  null | { soc: '86%', power: 'Discharging 998 W', left?: { time: '~4h 20m', rest: 'left at 1.0 kW' } },
     updated:  null | { stale: bool, text: 'Updated 08:20' | 'No new data since 08:20' },
     notificationsOff: bool,
     chart:    null | { status: 'ok'|'loading'|'empty'|'failed'|'fallback', since?: '07:58' (fallback, only when the app started today),
                        gap?: minutes (5 for SEMS+, 12 for own readings; default by status),
                        now: minuteOfDay, points: [[minuteOfDay, soc, kW], ...] },   // kW > 0 = discharging; x runs 00:00 to now
     signedIn: bool
   }
   Times are station-local minutes of today. Keep showing the last series while a refresh runs; 'loading' only before the first. */
(function (root) {
  'use strict';
  let uid = 0;
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

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
    folder: () => `<svg viewBox="0 0 16 16" ${S}><path d="M2 4.6A1.6 1.6 0 0 1 3.6 3h2.9l1.6 1.6h4.3A1.6 1.6 0 0 1 14 6.2v5.2a1.6 1.6 0 0 1-1.6 1.6H3.6A1.6 1.6 0 0 1 2 11.4z"/></svg>`,
    power: () => `<svg viewBox="0 0 16 16" ${S}><path d="M8 2v5.4"/><path d="M4.7 4.3a5 5 0 1 0 6.6 0"/></svg>`,
  };
  const ic = (name, cls = '') => `<span class="gw-ic ${cls}" aria-hidden="true">${ICONS[name]()}</span>`;

  // Chart: x 00:00 to now (chart.now, minute of day), left axis kW (symmetric, so 0 kW sits on the 50 % line), right axis SOC %.
  function chartSVG(chart) {
    const W = 304, Hh = 132, L = 26, R = 30, T = 16, B = 18, w = W - L - R, h = Hh - T - B;
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
    const hm = m => String(Math.floor(m / 60)).padStart(2, '0') + ':' + String(Math.round(m % 60)).padStart(2, '0');
    let g = '';
    if (msg) {
      // No data to plot: a quiet frame and the message; no value or time axes (they would mean nothing).
      g += `<line class="grid" x1="${L}" x2="${L + w}" y1="${T}" y2="${T}"/><line class="grid" x1="${L}" x2="${L + w}" y1="${T + h}" y2="${T + h}"/>`;
      g += `<text class="msg" x="${L + w / 2}" y="${T + h / 2 + 4}">${esc(msg)}</text>`;
    } else {
      [0, .5, 1].forEach(f => { g += `<line class="${f === .5 ? 'zero' : 'grid'}" x1="${L}" x2="${L + w}" y1="${T + f * h}" y2="${T + f * h}"/>`; });
      g += `<text class="ax" x="${L - 4}" y="${T - 6}" text-anchor="end">kW</text><text class="ax" x="${L + w + 4}" y="${T - 6}">%</text>`;
      [[maxKw, 0], [0, .5], [-maxKw, 1]].forEach(([k, f]) => { g += `<text class="ax" x="${L - 4}" y="${T + f * h + 3.5}" text-anchor="end">${fmt(k)}</text>`; });
      [[100, 0], [50, .5], [0, 1]].forEach(([s, f]) => { g += `<text class="ax" x="${L + w + 4}" y="${T + f * h + 3.5}">${s}</text>`; });
      // Time labels: start, whole hours that fit, and now at the right end.
      // An hour label needs 48 px from the end labels (half its width + a full label + a gap), or they touch.
      const step = (end - start) <= 240 ? 60 : (end - start) <= 720 ? 180 : 360;
      g += `<text class="ax" x="${x(start)}" y="${Hh - 4}">${hm(start)}</text>`;
      for (let t = Math.ceil((start + 1) / step) * step; t < end; t += step) if (x(t) - x(start) >= 48 && x(end) - x(t) >= 48) g += `<text class="ax" x="${x(t)}" y="${Hh - 4}" text-anchor="middle">${hm(t)}</text>`;
      g += `<text class="ax" x="${x(end)}" y="${Hh - 4}" text-anchor="end">${hm(end)}</text>`;

      // Break the lines where no reading came for more than GAP minutes: never draw values nobody measured.
      // Threshold follows the source: SEMS+ points every 1–2 min → 5; the app's own readings (one per new info.time, ~5 min) → 12.
      const GAP = chart.gap || (chart.status === 'fallback' ? 12 : 5), segs = [];
      pts.forEach((p, i) => { if (!i || p[0] - pts[i - 1][0] > GAP) segs.push([p]); else segs[segs.length - 1].push(p); });
      const path = (seg, f) => seg.map((p, i) => `${i ? 'L' : 'M'}${x(p[0]).toFixed(1)} ${f(p).toFixed(1)}`).join('');
      const z = yk(0).toFixed(1), clip = 'gwc' + (++uid);
      let d = `<clipPath id="${clip}"><rect x="${L - 3}" y="${T - 3}" width="${w + 6}" height="${h + 6}"/></clipPath><g clip-path="url(#${clip})">`;
      segs.forEach(seg => {
        const kw = path(seg, p => yk(p[2]));
        d += `<path class="kwfill" d="${kw}L${x(seg[seg.length - 1][0]).toFixed(1)} ${z}L${x(seg[0][0]).toFixed(1)} ${z}Z"/><path class="kw" d="${kw}"/>`;
        d += seg.length > 1 ? `<path class="soc" d="${path(seg, p => ys(p[1]))}"/>` : `<circle cx="${x(seg[0][0])}" cy="${ys(seg[0][1])}" r="1.5" fill="var(--gw-soc)"/>`;
      });
      const last = pts[pts.length - 1];
      d += `<circle cx="${x(last[0])}" cy="${yk(last[2])}" r="2.5" fill="var(--gw-kw)"/><circle cx="${x(last[0])}" cy="${ys(last[1])}" r="3" fill="var(--gw-soc)"/></g>`;
      g += d;
    }
    const label = chart.status === 'ok' || chart.status === 'fallback'
      ? `Battery today: ${pts.length ? pts[pts.length - 1][1] + '% now' : ''}` : (msg || '');
    return `<svg viewBox="0 0 ${W} ${Hh}" role="img" aria-label="${esc(label)}">${g}</svg>`;
  }

  function renderPanel(st) {
    const s = st.status, out = [];
    // Stale never shows the live red alarm.
    const statusIcon = { on: ic('plug', 'is-strong'), off: ic('plugoff', st.updated && st.updated.stale ? 'is-strong' : 'is-red'), unknown: ic('plug'), first: ic('plug'), signedout: ic('plug') }[s.kind];
    out.push(`<section class="gw-sec"><div class="gw-row">${statusIcon}<span class="gw-title">${esc(s.title)}</span></div>${s.detail ? `<p class="gw-sub">${esc(s.detail)}</p>` : ''}</section>`);

    const b = st.battery;
    if (b) out.push(`<section class="gw-sec"><div class="gw-row">${ic('battery')}<span class="gw-title">Battery</span><span class="gw-val">${esc(b.soc)}</span></div>`
      + `<p class="gw-sub">${esc(b.power)}</p>${b.left ? `<p class="gw-hero"><b>${esc(b.left.time)}</b>${esc(b.left.rest)}</p>` : ''}</section>`);

    const u = st.updated;
    if (u) out.push(`<section class="gw-sec gw-note${u.stale ? ' is-stale' : ''}"><div class="gw-row">${u.stale ? ic('warn', 'is-strong') : ic('clock')}<span class="gw-title"${u.stale ? '' : ' style="color:var(--gw-label-2)"'}>${esc(u.text)}</span></div></section>`);

    if (st.notificationsOff) out.push(`<section class="gw-sec"><div class="gw-row">${ic('bellslash', 'is-strong')}<span class="gw-title">Notifications are off</span></div><p class="gw-sub">You won’t hear when the grid goes off.</p></section>`);

    const c = st.chart;
    if (c) {
      const cap = { fallback: 'SEMS+ history isn’t available, so this shows this app’s own readings.',
        failed: 'The chart fills in from this app’s readings, every 2 minutes.',
        ok: 'Above 0 kW the battery is discharging, below 0 it is charging.' }[c.status] || '';
      out.push(`<section class="gw-sec gw-chart"><div class="gw-chart-head"><span class="gw-title">${c.status === 'fallback' && c.since ? 'Since ' + esc(c.since) : 'Today'}</span>`
        + `<span class="gw-legend"><span><i style="background:var(--gw-soc)"></i>Battery %</span><span><i style="background:var(--gw-kw)"></i>Battery kW</span></span></div>`
        + chartSVG(c) + (cap ? `<p class="gw-caption">${cap}</p>` : '') + `</section>`);
    }

    const acts = [];
    if (st.notificationsOff) acts.push(['notif', 'bell', 'Turn On Notifications…', '']);
    acts.push(['refresh', 'refresh', 'Refresh Now', '⌘R', !st.signedIn], ['signin', 'person', 'Sign In…', ''],
      ['logs', 'folder', 'Open Log Folder', ''], ['quit', 'power', 'Quit', '⌘Q']);
    out.push(`<nav class="gw-actions" aria-label="Actions">${acts.map(([id, icon, t, k, dis]) =>
      `<button class="gw-act" data-action="${id}"${dis ? ' aria-disabled="true" tabindex="-1"' : ''}>${ic(icon)}<span>${t}</span>${k ? `<span class="k">${k}</span>` : ''}</button>`).join('')}</nav>`);

    return `<div class="gw-panel" role="dialog" aria-label="GoodWe Status">${out.join('')}</div>`;
  }

  root.GW = { renderPanel, chartSVG };
})(typeof window !== 'undefined' ? window : globalThis);
