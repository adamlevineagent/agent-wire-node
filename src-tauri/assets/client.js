/* WS-J — post-agents-retro canvas progressive-enhancement client.
 *
 * Vanilla JS, no transpilation, no bundler. Loaded by render.rs via a
 * <script src="/p/_assets/client.js" defer></script> tag. The page works
 * fine without this script (HTML-only fallback); the script only *adds*
 * a canvas overlay that can be toggled on via a [GRID] button.
 *
 * Contract:
 *   - Never modifies the semantic HTML. Screen readers always see the
 *     <article class="node"> tree unchanged.
 *   - Canvas is aria-hidden="true". HTML keeps aria-live="polite".
 *   - Default mode: 'html' (canvas hidden). User opts in via [GRID].
 *   - Preference persisted in localStorage under 'wire_canvas_mode'.
 *   - Exposes window.__wireCanvasUpdate(taggedEvent) as a stub for WS-K
 *     to replace with real build-event animation logic.
 */
(function () {
  'use strict';

  var STATE = {
    mode: 'html',
    canvas: null,
    ctx: null,
    toggleBtn: null,
    pageData: null,
    fontSize: 14,
    lineHeight: 21,
    charWidth: 0,
    banner: '',
  };

  // Retro color tokens — kept in sync with assets/app.css §Aesthetic spec.
  var COLORS = {
    bg: '#0a0a0a',
    fg: '#d0d0c8',
    dim: '#707068',
    hot: '#ff6b35',
    warn: '#f0c040',
    gap: '#a04040',
    link: '#60a0ff',
  };

  var FONT_FAMILY = 'JetBrains Mono, ui-monospace, SFMono-Regular, Menlo, monospace';

  function init() {
    try {
      STATE.pageData = readPageStructure();
      STATE.banner =
        (document.body && document.body.getAttribute('data-banner')) || '';
      createCanvasOverlay();
      measureCharWidth();
      createToggle();
      window.addEventListener('resize', handleResize);
      var stored = null;
      try {
        stored = localStorage.getItem('wire_canvas_mode');
      } catch (_) {}
      if (stored === 'canvas') {
        toggleMode();
      } else {
        sizeCanvas();
      }
    } catch (err) {
      // Never break the HTML page if anything goes wrong.
      if (window.console && console.warn) {
        console.warn('[wire] canvas client init failed:', err);
      }
    }
  }

  function readPageStructure() {
    var h1 = document.querySelector('h1');
    var title = h1 ? (h1.textContent || '').trim() : document.title;
    var articles = [];
    var nodeEls = document.querySelectorAll('article.node');
    for (var i = 0; i < nodeEls.length; i++) {
      var el = nodeEls[i];
      var headlineEl = el.querySelector('.headline');
      var distilledEl = el.querySelector('.distilled');
      // Staleness class lives on the <article> itself.
      var cls = el.className || '';
      var state = 'verified';
      if (cls.indexOf('gap') !== -1) state = 'gap';
      else if (cls.indexOf('stale') !== -1) state = 'stale';
      else if (cls.indexOf('hot') !== -1) state = 'hot';
      articles.push({
        id: el.getAttribute('id') || ('n' + i),
        headline: headlineEl ? (headlineEl.textContent || '').trim() : '',
        distilled: distilledEl ? (distilledEl.textContent || '').trim() : '',
        state: state,
      });
    }
    var tocItems = [];
    var tocLinks = document.querySelectorAll('ul.toc a, nav.toc a');
    for (var j = 0; j < tocLinks.length; j++) {
      tocItems.push((tocLinks[j].textContent || '').trim());
    }
    return { title: title, articles: articles, toc: tocItems };
  }

  function createCanvasOverlay() {
    var c = document.createElement('canvas');
    c.setAttribute('aria-hidden', 'true');
    c.style.position = 'fixed';
    c.style.top = '0';
    c.style.left = '0';
    c.style.width = '100vw';
    c.style.height = '100vh';
    c.style.pointerEvents = 'none';
    c.style.opacity = '0';
    c.style.transition = 'opacity 180ms linear';
    c.style.zIndex = '9998';
    c.style.background = COLORS.bg;
    document.body.appendChild(c);
    STATE.canvas = c;
    STATE.ctx = c.getContext('2d');
  }

  function measureCharWidth() {
    STATE.ctx.font = STATE.fontSize + 'px ' + FONT_FAMILY;
    var m = STATE.ctx.measureText('M');
    STATE.charWidth = m.width || STATE.fontSize * 0.6;
  }

  function sizeCanvas() {
    var dpr = window.devicePixelRatio || 1;
    STATE.canvas.width = Math.floor(window.innerWidth * dpr);
    STATE.canvas.height = Math.floor(window.innerHeight * dpr);
    STATE.canvas.style.width = window.innerWidth + 'px';
    STATE.canvas.style.height = window.innerHeight + 'px';
    STATE.ctx.setTransform(1, 0, 0, 1, 0, 0);
    STATE.ctx.scale(dpr, dpr);
    STATE.ctx.font = STATE.fontSize + 'px ' + FONT_FAMILY;
    STATE.ctx.textBaseline = 'top';
  }

  function createToggle() {
    var b = document.createElement('button');
    b.type = 'button';
    b.textContent = '[GRID]';
    b.setAttribute('aria-label', 'Toggle canvas grid view');
    b.style.position = 'fixed';
    b.style.top = '8px';
    b.style.right = '8px';
    b.style.zIndex = '9999';
    b.style.font = '12px ' + FONT_FAMILY;
    b.style.background = COLORS.bg;
    b.style.color = COLORS.fg;
    b.style.border = '1px solid ' + COLORS.dim;
    b.style.padding = '4px 8px';
    b.style.cursor = 'pointer';
    b.addEventListener('click', toggleMode);
    document.body.appendChild(b);
    STATE.toggleBtn = b;
  }

  function toggleMode() {
    if (STATE.mode === 'html') {
      STATE.mode = 'canvas';
      sizeCanvas();
      STATE.canvas.style.opacity = '1';
      STATE.canvas.style.pointerEvents = 'auto';
      document.documentElement.style.overflow = 'hidden';
      // Hide HTML visually but keep it in the a11y tree.
      var main = document.querySelector('main.page') || document.body;
      main.style.opacity = '0';
      drawCanvas();
    } else {
      STATE.mode = 'html';
      STATE.canvas.style.opacity = '0';
      STATE.canvas.style.pointerEvents = 'none';
      document.documentElement.style.overflow = '';
      var main2 = document.querySelector('main.page') || document.body;
      main2.style.opacity = '';
    }
    try {
      localStorage.setItem('wire_canvas_mode', STATE.mode);
    } catch (_) {}
  }

  function drawCanvas() {
    var ctx = STATE.ctx;
    var w = window.innerWidth;
    var h = window.innerHeight;
    ctx.fillStyle = COLORS.bg;
    ctx.fillRect(0, 0, w, h);
    ctx.font = STATE.fontSize + 'px ' + FONT_FAMILY;
    ctx.textBaseline = 'top';

    var padX = 16;
    var y = 16;
    var cols = Math.max(20, Math.floor((w - padX * 2) / STATE.charWidth));

    // Optional ASCII banner from <body data-banner="...">.
    if (STATE.banner) {
      ctx.fillStyle = COLORS.hot;
      var bannerLines = STATE.banner.split('\n');
      for (var bi = 0; bi < bannerLines.length; bi++) {
        ctx.fillText(bannerLines[bi], padX, y);
        y += STATE.lineHeight;
      }
      y += STATE.lineHeight / 2;
    }

    // Title.
    ctx.fillStyle = COLORS.fg;
    var title = STATE.pageData.title || '';
    ctx.fillText(title, padX, y);
    y += STATE.lineHeight;
    var rule = new Array(Math.min(cols, title.length || 1) + 1).join('=');
    ctx.fillStyle = COLORS.dim;
    ctx.fillText(rule, padX, y);
    y += STATE.lineHeight * 1.5;

    // Articles: headline + distilled, with a state-colored left bar.
    var arts = STATE.pageData.articles || [];
    for (var i = 0; i < arts.length; i++) {
      if (y > h - STATE.lineHeight * 2) break;
      var a = arts[i];
      var barColor = COLORS.fg;
      if (a.state === 'gap') barColor = COLORS.gap;
      else if (a.state === 'stale') barColor = COLORS.warn;
      else if (a.state === 'hot') barColor = COLORS.hot;
      ctx.fillStyle = barColor;
      ctx.fillText('|', padX, y);
      ctx.fillStyle = COLORS.fg;
      ctx.fillText(truncate(a.headline, cols - 4), padX + STATE.charWidth * 2, y);
      y += STATE.lineHeight;
      if (a.distilled && y < h - STATE.lineHeight) {
        ctx.fillStyle = COLORS.dim;
        ctx.fillText(
          truncate(a.distilled, cols - 4),
          padX + STATE.charWidth * 2,
          y
        );
        y += STATE.lineHeight;
      }
      y += STATE.lineHeight / 2;
    }

    // Footer hint.
    ctx.fillStyle = COLORS.dim;
    ctx.fillText(
      '[GRID] press button to return to HTML mode',
      padX,
      h - STATE.lineHeight - 8
    );
  }

  function truncate(s, n) {
    if (!s) return '';
    if (s.length <= n) return s;
    return s.slice(0, Math.max(0, n - 1)) + '\u2026';
  }

  function handleResize() {
    if (STATE.mode === 'canvas') {
      sizeCanvas();
      drawCanvas();
    }
  }

  // Stub for WS-K. WS-K will replace this to animate build events arriving
  // over the WebSocket (routes_ws.rs). The `taggedEvent` shape is defined by
  // WS-K's `BuildEvent` enum serialized as JSON. For V1 we just log.
  window.__wireCanvasUpdate = function (taggedEvent) {
    if (window.console && console.log) {
      console.log('[wire] build event:', taggedEvent);
    }
    if (STATE.mode === 'canvas') {
      drawCanvas();
    }
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
