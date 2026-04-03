// ---------------------------------------------------------------------------
// wefax.js — WEFAX decoder plugin for trx-frontend-http
// Live view: decoder state, live canvas, latest image card
// History view: filterable table of all decoded images
// ---------------------------------------------------------------------------

// ── DOM references (cached once) ───────────────────────────────────
var wefaxDom = {
  status:         document.getElementById('wefax-status'),
  liveView:       document.getElementById('wefax-live-view'),
  historyView:    document.getElementById('wefax-history-view'),
  liveContainer:  document.getElementById('wefax-live-container'),
  liveInfo:       document.getElementById('wefax-live-info'),
  liveCanvas:     document.getElementById('wefax-live-canvas'),
  liveLatest:     document.getElementById('wefax-live-latest'),
  historyList:    document.getElementById('wefax-history-list'),
  historyCount:   document.getElementById('wefax-history-count'),
  filterInput:    document.getElementById('wefax-filter'),
  sortSelect:     document.getElementById('wefax-sort'),
  toggleBtn:      document.getElementById('wefax-decode-toggle-btn'),
  clearBtn:       document.getElementById('wefax-clear-btn'),
  viewLiveBtn:    document.getElementById('wefax-view-live'),
  viewHistoryBtn: document.getElementById('wefax-view-history'),
};

// ── State ───────────────────────────────────────────────────────────
var wefaxImageHistory = [];
var WEFAX_MAX_IMAGES  = 100;
var wefaxLiveCtx      = null;
var wefaxLiveLineCount = 0;
var wefaxLivePixelsPerLine = 1809;
var wefaxActiveView   = 'live';
var wefaxFilterText   = '';

// ── Helpers ─────────────────────────────────────────────────────────
function currentWefaxHistoryRetentionMs() {
  return window.getDecodeHistoryRetentionMs ? window.getDecodeHistoryRetentionMs() : 24 * 60 * 60 * 1000;
}

function pruneWefaxHistory() {
  var cutoff = Date.now() - currentWefaxHistoryRetentionMs();
  wefaxImageHistory = wefaxImageHistory.filter(function (m) { return (m._tsMs || 0) > cutoff; });
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

function scheduleWefaxUi(key, job) {
  if (typeof window.trxScheduleUiFrameJob === 'function') {
    window.trxScheduleUiFrameJob(key, job);
    return;
  }
  job();
}

// ── View switching ──────────────────────────────────────────────────
function switchWefaxView(view) {
  wefaxActiveView = view;
  if (wefaxDom.liveView)    wefaxDom.liveView.style.display = view === 'live' ? '' : 'none';
  if (wefaxDom.historyView) wefaxDom.historyView.style.display = view === 'history' ? '' : 'none';

  [wefaxDom.viewLiveBtn, wefaxDom.viewHistoryBtn].forEach(function (btn) {
    if (btn) btn.classList.remove('sat-view-active');
  });
  if (view === 'live' && wefaxDom.viewLiveBtn)       wefaxDom.viewLiveBtn.classList.add('sat-view-active');
  if (view === 'history' && wefaxDom.viewHistoryBtn)  wefaxDom.viewHistoryBtn.classList.add('sat-view-active');

  if (view === 'history') renderWefaxHistoryTable();
}

if (wefaxDom.viewLiveBtn)    wefaxDom.viewLiveBtn.addEventListener('click', function () { switchWefaxView('live'); });
if (wefaxDom.viewHistoryBtn) wefaxDom.viewHistoryBtn.addEventListener('click', function () { switchWefaxView('history'); });

// ── Live canvas rendering ───────────────────────────────────────────
function resetLiveCanvas(pixelsPerLine) {
  wefaxLivePixelsPerLine = pixelsPerLine;
  wefaxLiveLineCount = 0;
  wefaxDom.liveCanvas.width = pixelsPerLine;
  wefaxDom.liveCanvas.height = 800;
  wefaxLiveCtx = wefaxDom.liveCanvas.getContext('2d');
  wefaxLiveCtx.fillStyle = '#000';
  wefaxLiveCtx.fillRect(0, 0, wefaxDom.liveCanvas.width, wefaxDom.liveCanvas.height);
  if (wefaxDom.liveContainer) wefaxDom.liveContainer.style.display = '';
}

function paintLine(lineBytes) {
  if (!wefaxLiveCtx) return;
  var y = wefaxLiveLineCount;

  if (y >= wefaxDom.liveCanvas.height) {
    var old = wefaxLiveCtx.getImageData(0, 0, wefaxDom.liveCanvas.width, wefaxDom.liveCanvas.height);
    wefaxDom.liveCanvas.height *= 2;
    wefaxLiveCtx.putImageData(old, 0, 0);
  }

  var w = wefaxLivePixelsPerLine;
  var imgData = wefaxLiveCtx.createImageData(w, 1);
  var d = imgData.data;
  for (var x = 0; x < w; x++) {
    var v = x < lineBytes.length ? lineBytes[x] : 0;
    var i = x * 4;
    d[i] = v; d[i + 1] = v; d[i + 2] = v; d[i + 3] = 255;
  }
  wefaxLiveCtx.putImageData(imgData, 0, y);
  wefaxLiveLineCount++;
}

// ── Live view: latest image card ────────────────────────────────────
function renderWefaxLatestCard() {
  if (!wefaxDom.liveLatest) return;
  if (wefaxImageHistory.length === 0) {
    wefaxDom.liveLatest.innerHTML =
      '<div style="color:var(--text-muted);font-size:0.82rem;">No images decoded yet. Enable the decoder and tune to a WEFAX station.</div>';
    return;
  }

  var img = wefaxImageHistory[0];
  var ts = img._ts || '--';
  var date = img._tsMs ? new Date(img._tsMs).toLocaleDateString() : '';
  var meta = [
    img.ioc + ' IOC',
    img.lpm + ' LPM',
    img.line_count + ' lines',
    date + ' ' + ts,
  ].join(' \u00b7 ');

  var imgSrc = img._dataUrl
    ? img._dataUrl
    : img.path
      ? '/images/' + escapeHtml(img.path.split('/').pop())
      : null;

  var html = '<div class="sat-latest-card">';
  html += '<div class="sat-latest-title">Latest decoded image</div>';
  html += '<div class="sat-latest-meta">' + escapeHtml(meta) + '</div>';
  if (imgSrc) {
    html += '<a href="' + imgSrc + '" target="_blank" style="font-size:0.8rem;color:var(--accent);display:inline-block;margin-top:0.25rem;">View full image</a>';
  }
  html += '</div>';
  wefaxDom.liveLatest.innerHTML = html;
}

// ── History view: table ─────────────────────────────────────────────
function getWefaxFilteredHistory() {
  var items = wefaxImageHistory;

  if (wefaxFilterText) {
    items = items.filter(function (i) {
      var haystack = [
        String(i.ioc || ''),
        String(i.lpm || ''),
        String(i.line_count || ''),
      ].join(' ').toUpperCase();
      return haystack.indexOf(wefaxFilterText) >= 0;
    });
  }

  var sortVal = wefaxDom.sortSelect ? wefaxDom.sortSelect.value : 'newest';
  if (sortVal === 'oldest') items = items.slice().reverse();

  return items;
}

function renderWefaxHistoryRow(img) {
  var row = document.createElement('div');
  row.className = 'sat-history-row';

  var ts = img._ts || '--';
  var date = img._tsMs ? new Date(img._tsMs).toLocaleDateString([], { month: 'short', day: 'numeric' }) : '';
  var ioc = img.ioc || '--';
  var lpm = img.lpm || '--';
  var lines = img.line_count || 0;

  var imgSrc = img._dataUrl
    ? img._dataUrl
    : img.path
      ? '/images/' + escapeHtml(img.path.split('/').pop())
      : null;
  var link = imgSrc
    ? '<a href="' + imgSrc + '" target="_blank" style="color:var(--accent);">View</a>'
    : '--';

  row.innerHTML = [
    '<span>' + escapeHtml(date + ' ' + ts) + '</span>',
    '<span>' + escapeHtml(String(ioc)) + '</span>',
    '<span>' + escapeHtml(String(lpm)) + '</span>',
    '<span>' + lines + '</span>',
    '<span>' + link + '</span>',
  ].join('');

  return row;
}

function renderWefaxHistoryTable() {
  if (!wefaxDom.historyList) return;
  pruneWefaxHistory();
  var items = getWefaxFilteredHistory();
  var fragment = document.createDocumentFragment();
  for (var i = 0; i < items.length; i++) {
    fragment.appendChild(renderWefaxHistoryRow(items[i]));
  }
  wefaxDom.historyList.replaceChildren(fragment);

  if (wefaxDom.historyCount) {
    var total = wefaxImageHistory.length;
    var shown = items.length;
    wefaxDom.historyCount.textContent =
      total === 0
        ? 'No images yet'
        : shown === total
          ? total + ' image' + (total === 1 ? '' : 's')
          : shown + ' of ' + total + ' images';
  }
}

// ── Add image to history ────────────────────────────────────────────
function addWefaxImage(msg) {
  var tsMs = Number.isFinite(msg.ts_ms) ? Number(msg.ts_ms) : Date.now();
  msg._tsMs = tsMs;
  msg._ts = new Date(tsMs).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });

  // Capture the live canvas as a data URI for thumbnails.
  if (wefaxLiveCtx && wefaxLiveLineCount > 0) {
    var trimmed = wefaxLiveCtx.getImageData(0, 0, wefaxDom.liveCanvas.width, wefaxLiveLineCount);
    wefaxDom.liveCanvas.height = wefaxLiveLineCount;
    wefaxLiveCtx.putImageData(trimmed, 0, 0);
    try { msg._dataUrl = wefaxDom.liveCanvas.toDataURL('image/png'); } catch (e) {}
  }

  wefaxImageHistory.unshift(msg);
  if (wefaxImageHistory.length > WEFAX_MAX_IMAGES) {
    wefaxImageHistory = wefaxImageHistory.slice(0, WEFAX_MAX_IMAGES);
  }

  scheduleWefaxUi('wefax-latest', renderWefaxLatestCard);
  if (wefaxActiveView === 'history') {
    scheduleWefaxUi('wefax-history', renderWefaxHistoryTable);
  }
}

// ── SSE event handlers (public API) ─────────────────────────────────
window.onServerWefaxProgress = function (msg) {
  // State-only update (no image data): show decoder state in status.
  if (msg.state && !msg.line_data) {
    if (wefaxDom.status) {
      wefaxDom.status.textContent = msg.state;
      // Highlight active states, dim idle/scanning.
      wefaxDom.status.style.color = msg.state.indexOf('Idle') === 0 ? '' : 'var(--text-accent)';
    }
    return;
  }

  if (msg.line_count <= 1 || !wefaxLiveCtx) {
    resetLiveCanvas(msg.pixels_per_line || 1809);
  }

  if (msg.line_data) {
    var binary = atob(msg.line_data);
    var bytes = new Uint8Array(binary.length);
    for (var i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    paintLine(bytes);
  }

  if (wefaxDom.liveInfo) {
    wefaxDom.liveInfo.textContent =
      'Line ' + msg.line_count + ' \u00b7 ' + msg.ioc + ' IOC \u00b7 ' + msg.lpm + ' LPM';
  }
  if (wefaxDom.status) {
    wefaxDom.status.textContent = 'Receiving \u2014 line ' + msg.line_count;
    wefaxDom.status.style.color = 'var(--text-accent)';
  }
};

window.onServerWefax = function (msg) {
  addWefaxImage(msg);

  if (wefaxDom.liveContainer) wefaxDom.liveContainer.style.display = 'none';
  if (wefaxDom.status) {
    wefaxDom.status.textContent = 'Complete \u2014 ' + msg.line_count + ' lines';
    wefaxDom.status.style.color = '';
  }
};

window.restoreWefaxHistory = function (messages) {
  if (!messages || !messages.length) return;
  for (var i = 0; i < messages.length; i++) {
    var tsMs = Number.isFinite(messages[i].ts_ms) ? Number(messages[i].ts_ms) : Date.now();
    messages[i]._tsMs = tsMs;
    messages[i]._ts = new Date(tsMs).toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    });
  }
  wefaxImageHistory = messages.concat(wefaxImageHistory);
  pruneWefaxHistory();
  scheduleWefaxUi('wefax-latest', renderWefaxLatestCard);
  if (wefaxActiveView === 'history') {
    scheduleWefaxUi('wefax-history', renderWefaxHistoryTable);
  }
};

window.pruneWefaxHistoryView = function () {
  pruneWefaxHistory();
  renderWefaxHistoryTable();
  renderWefaxLatestCard();
};

window.resetWefaxHistoryView = function () {
  wefaxImageHistory = [];
  if (wefaxDom.historyList) wefaxDom.historyList.innerHTML = '';
  if (wefaxDom.liveContainer) wefaxDom.liveContainer.style.display = 'none';
  wefaxLiveCtx = null;
  wefaxLiveLineCount = 0;
  renderWefaxLatestCard();
  renderWefaxHistoryTable();
  if (wefaxDom.status) {
    wefaxDom.status.textContent = 'Idle';
    wefaxDom.status.style.color = '';
  }
};

// ── Filter / sort handlers ──────────────────────────────────────────
if (wefaxDom.filterInput) {
  wefaxDom.filterInput.addEventListener('input', function () {
    wefaxFilterText = wefaxDom.filterInput.value.trim().toUpperCase();
    scheduleWefaxUi('wefax-history', renderWefaxHistoryTable);
  });
}
if (wefaxDom.sortSelect) {
  wefaxDom.sortSelect.addEventListener('change', function () {
    scheduleWefaxUi('wefax-history', renderWefaxHistoryTable);
  });
}

// ── Button handlers ─────────────────────────────────────────────────
if (wefaxDom.toggleBtn) {
  wefaxDom.toggleBtn.addEventListener('click', async function () {
    try {
      if (window.takeSchedulerControlForDecoderDisable) {
        await window.takeSchedulerControlForDecoderDisable(wefaxDom.toggleBtn);
      }
      await postPath('/toggle_wefax_decode');
    } catch (e) {
      console.error('WEFAX toggle failed', e);
    }
  });
}
if (wefaxDom.clearBtn) {
  wefaxDom.clearBtn.addEventListener('click', async function () {
    try {
      await postPath('/clear_wefax_decode');
      window.resetWefaxHistoryView();
    } catch (e) {
      console.error('WEFAX clear failed', e);
    }
  });
}

// ── Initial render ──────────────────────────────────────────────────
renderWefaxLatestCard();
