// === WS-K WS client === //
// WebSocket client + live build animation for post-agents-retro web surface.
// This file is loaded AFTER assets/client.js (WS-J) so it can override
// the stub window.__wireCanvasUpdate with a real rAF-coalesced pipeline.
//
// Wire protocol: TaggedBuildEvent JSON over text frames from GET /p/{slug}/_ws
//   { "slug": "...", "kind": { "type": "progress", "done": N, "total": M } }
//   { "slug": "...", "kind": { "type": "v2_snapshot", "done":..., "total":..., "layers":..., "current_step":..., "log":... } }
//   { "slug": "...", "kind": { "type": "resync" } }
//
// Server already applies 60ms coalesce + slug filter + lagged->Resync.
// We add a second layer of rAF coalesce on the client for burst safety.

(function () {
  'use strict';

  if (typeof window === 'undefined' || typeof WebSocket === 'undefined') return;

  var WS_STATE = {
    socket: null,
    reconnectDelay: 1000,
    reconnectMax: 30000,
    pendingEvents: [],
    rafScheduled: false,
    slug: null,
    closed: false,
  };

  function getCurrentSlug() {
    var m = window.location.pathname.match(/^\/p\/([^/]+)/);
    return m ? m[1] : null;
  }

  function connectWS(slug) {
    if (!slug) return;
    var proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    var url = proto + '//' + window.location.host + '/p/' + slug + '/_ws';
    var sock;
    try {
      sock = new WebSocket(url);
    } catch (e) {
      console.warn('[wire] WS construction failed:', e);
      return;
    }
    WS_STATE.socket = sock;

    sock.onopen = function () {
      console.log('[wire] WS connected:', slug);
      WS_STATE.reconnectDelay = 1000;
    };
    sock.onmessage = function (ev) {
      var event;
      try {
        event = JSON.parse(ev.data);
      } catch (e) {
        return;
      }
      enqueueEvent(event);
    };
    sock.onclose = function () {
      console.log('[wire] WS closed; reconnecting in', WS_STATE.reconnectDelay, 'ms');
      var delay = WS_STATE.reconnectDelay;
      WS_STATE.reconnectDelay = Math.min(WS_STATE.reconnectDelay * 2, WS_STATE.reconnectMax);
      setTimeout(function () { connectWS(slug); }, delay);
    };
    sock.onerror = function (e) {
      console.warn('[wire] WS error:', e);
    };
  }

  function enqueueEvent(event) {
    if (!event || !event.kind) return;
    if (event.kind.type === 'resync') {
      WS_STATE.pendingEvents = [];
      refreshFromDom();
      return;
    }
    WS_STATE.pendingEvents.push(event);
    if (!WS_STATE.rafScheduled) {
      WS_STATE.rafScheduled = true;
      requestAnimationFrame(drainEvents);
    }
  }

  function drainEvents() {
    WS_STATE.rafScheduled = false;
    var events = WS_STATE.pendingEvents.splice(0, WS_STATE.pendingEvents.length);
    if (events.length === 0) return;

    // Coalesce: keep only the latest Progress and latest V2Snapshot.
    var latestProgress = null;
    var latestSnapshot = null;
    for (var i = 0; i < events.length; i++) {
      var k = events[i].kind;
      if (!k) continue;
      if (k.type === 'progress') latestProgress = k;
      else if (k.type === 'v2_snapshot') latestSnapshot = k;
    }

    if (latestProgress) renderProgress(latestProgress);
    if (latestSnapshot) renderSnapshot(latestSnapshot);
  }

  function renderProgress(p) {
    var done = p.done || 0;
    var total = p.total || 0;
    var pct = total > 0 ? Math.round((done / total) * 100) : 0;
    var barWidth = 20;
    var filled = Math.max(0, Math.min(barWidth, Math.round((pct / 100) * barWidth)));
    var bar = repeat('\u2588', filled) + repeat('\u2591', barWidth - filled);
    var text = 'BUILDING [' + bar + '] ' + done + ' / ' + total;

    var el = document.getElementById('wire-build-progress');
    if (!el) {
      el = document.createElement('div');
      el.id = 'wire-build-progress';
      el.style.cssText =
        'position:fixed;bottom:8px;left:8px;right:8px;' +
        'background:var(--bg,#000);color:var(--hot,#ff6a00);' +
        'font-family:monospace;padding:8px;' +
        'border:1px solid var(--rule,#333);text-align:center;z-index:9999;';
      document.body.appendChild(el);
    }
    el.textContent = text;

    if (done >= total && total > 0) {
      setTimeout(function () {
        var e1 = document.getElementById('wire-build-progress');
        if (e1) e1.remove();
        var e2 = document.getElementById('wire-build-step');
        if (e2) e2.remove();
      }, 2000);
    }
  }

  function renderSnapshot(s) {
    if (s && s.current_step) {
      var el = document.getElementById('wire-build-step');
      if (!el) {
        el = document.createElement('div');
        el.id = 'wire-build-step';
        el.style.cssText =
          'position:fixed;bottom:48px;left:8px;right:8px;' +
          'background:var(--bg,#000);color:var(--dim,#888);' +
          'font-family:monospace;padding:4px 8px;font-size:12px;' +
          'text-align:center;z-index:9999;';
        document.body.appendChild(el);
      }
      el.textContent = '\u2192 ' + s.current_step;
    }
    if (typeof s.done === 'number' && typeof s.total === 'number') {
      renderProgress({ done: s.done, total: s.total });
    }
  }

  function refreshFromDom() {
    if (typeof window.__wireRefresh === 'function') {
      try { window.__wireRefresh(); } catch (e) { /* swallow */ }
    }
  }

  function repeat(ch, n) {
    var s = '';
    for (var i = 0; i < n; i++) s += ch;
    return s;
  }

  // Override any WS-J stub (may be set before or after this file loads).
  window.__wireCanvasUpdate = enqueueEvent;

  // Initialize on page load. Skip non-pyramid pages (_login, _logout, empty, etc.)
  var slug = getCurrentSlug();
  WS_STATE.slug = slug;
  if (slug && slug.length > 0 && slug.charAt(0) !== '_') {
    connectWS(slug);
  }
})();
// === end WS-K === //
