/* One day's power-cut strip, shared by Settings › Stats (T-015, 494 pt) and the panel's Grid section (T-020, 280 pt).
   GWStrip.svg(strip, W) draws it; GWStrip.bind(plot, rows, W, o) wires its read-out. Rust sends strip = { from?, to?,
   cuts: [[a, b], ...], gaps?: [[a, b], ...] } in minutes of the day, and rows[i] (the formatted cut) for cuts[i]. */
(function (root) {
  'use strict';
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

  function svg(z, W) {
    z = z || {};
    const x = m => (m / 1440 * W).toFixed(1), from = z.from || 0, to = z.to == null ? 1440 : z.to;
    // Hours with no history (before the station's first minute, after now) are a dashed outline; known hours are the track.
    let g = from > 0 ? `<rect class="track-rest" x=".5" y=".5" width="${Math.max(0, from / 1440 * W - 1)}" height="7" rx="3.5"/>` : '';
    if (to < 1440) g += `<rect class="track-rest" x="${+x(to) + .5}" y=".5" width="${(1440 - to) / 1440 * W - 1}" height="7" rx="3.5"/>`;
    // D-O1: a gap (more than 5 min with no readings) splits the track and is dashed like the other unknown hours.
    let at = from;
    [...(z.gaps || []), [to, to - 1]].forEach(([a, e]) => {
      if (a > at) g += `<rect class="track" x="${x(at)}" y="0" width="${((a - at) / 1440 * W).toFixed(1)}" height="8" rx="4"/>`;
      if (e >= a) { const w = (e - a + 1) / 1440 * W; g += `<rect class="track-rest" x="${+x(a) + .5}" y=".5" width="${(w - 1).toFixed(1)}" height="7" rx="3.5"/>`; }
      at = e + 1;
    });
    // Each cut has an invisible hit area 24 pt tall and at least 12 pt wide, so a 5-minute cut is easy to point at.
    let hits = '';
    (z.cuts || []).forEach(([a, b], i) => {
      const w = Math.max(3, (b - a + 1) / 1440 * W), cx = Math.min(+x(a), W - w), hw = Math.max(12, w);
      g += `<rect class="cut" data-i="${i}" x="${cx}" y="0" width="${w.toFixed(1)}" height="8" rx="${Math.min(4, w / 2)}"/>`;
      hits += `<rect class="hit" data-i="${i}" x="${(cx + w / 2 - hw / 2).toFixed(1)}" y="-8" width="${hw.toFixed(1)}" height="24"/>`;
    });
    g += hits;
    if (z.to != null) g += `<line class="now" x1="${x(to)}" x2="${x(to)}" y1="-3" y2="11"/>`;
    const ax = [[0, 'start', '12 AM'], [360, 'middle', '6 AM'], [720, 'middle', '12 PM'], [1080, 'middle', '6 PM'], [1440, 'end', '12 AM']];
    g += ax.map(([m, a, t]) => `<text x="${x(m)}" y="24" text-anchor="${a}">${t.replace(' ', ' ')}</text>`).join('');
    return `<svg class="strip" viewBox="0 -4 ${W} 32" width="${W}" height="32" aria-hidden="true">${g}</svg>`;
  }

  // Read-out (T-015, owner): point at a cut, or Tab to the strip and use ← → Home End. The card is the panel chart's: the
  // time range, then a red swatch, Grid off and the length; the other cuts fade back. o.tip: the card's class ('st-tip' |
  // 'gw-tip'); o.left: how far left of the strip the card may reach; o.onShow(i) / o.onHide(): the caller's extras.
  // The cut read last is kept in plot.dataset.kb, so a re-render can put keyboard focus back on it.
  function bind(plot, rows, W, o) {
    if (!plot || !rows) return null;
    const tip = plot.querySelector('.' + o.tip), svg = plot.querySelector('svg'), p = o.tip;
    let kb = -1;
    function show(i) {
      const r = rows[i], cut = svg.querySelector(`.cut[data-i="${i}"]`); if (!r || !cut) return;
      const from = r.offDay ? `${r.offDay} ${r.off}` : r.off, to = r.running ? 'now' : r.onDay ? `${r.onDay} ${r.on}` : r.on;
      tip.innerHTML = `<p class="${p}-time">${esc(from)} – ${esc(to)}</p><p class="${p}-row"><i class="off"></i><span>${r.running ? 'Still off' : 'Grid off'}</span><b>${esc(r.dur)}</b></p>`;
      tip.hidden = false;
      // Centred on the cut, kept inside its box (it may reach 12 pt past the right end, o.left past the left).
      const k = plot.clientWidth / W, c = (+cut.getAttribute('x') + +cut.getAttribute('width') / 2) * k, w = tip.offsetWidth;
      tip.style.left = Math.max(o.left, Math.min(plot.clientWidth + 12 - w, c - w / 2)) + 'px';
      plot.classList.add('is-reading');
      svg.querySelectorAll('.cut').forEach(e => e.classList.toggle('is-on', e.dataset.i === String(i)));
      if (o.onShow) o.onShow(i);
    }
    function hide() {
      if (plot.matches(':focus-visible') && kb >= 0) return show(kb);
      tip.hidden = true; plot.classList.remove('is-reading');
      svg.querySelectorAll('.cut.is-on').forEach(e => e.classList.remove('is-on'));
      if (o.onHide) o.onHide();
    }
    plot.addEventListener('pointerover', e => { const h = e.target.closest('.hit'); if (h) show(+h.dataset.i); });
    plot.addEventListener('pointerleave', hide);
    plot.addEventListener('pointerdown', e => e.preventDefault()); // a click never focuses the strip; only Tab does
    plot.addEventListener('focus', () => { kb = Math.min(rows.length - 1, +plot.dataset.kb || 0); plot.dataset.kb = kb; show(kb); });
    plot.addEventListener('blur', () => { kb = -1; delete plot.dataset.kb; hide(); });
    plot.addEventListener('keydown', e => {
      const n = rows.length, to = { ArrowLeft: kb - 1, ArrowRight: kb + 1, Home: 0, End: n - 1 }[e.key];
      if (to == null) return;
      e.preventDefault(); e.stopPropagation(); // ← → read cuts here (in Stats: instead of changing the day)
      kb = Math.max(0, Math.min(n - 1, to)); plot.dataset.kb = kb; show(kb);
    });
    return { show, hide };
  }

  root.GWStrip = { svg, bind };
})(typeof window !== 'undefined' ? window : globalThis);
