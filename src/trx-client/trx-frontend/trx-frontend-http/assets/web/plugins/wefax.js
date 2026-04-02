// ---------------------------------------------------------------------------
// wefax.js — WEFAX decoder plugin for trx-frontend-http
// ---------------------------------------------------------------------------

// --- DOM refs ---
var wefaxStatus       = document.getElementById('wefax-status');
var wefaxLiveContainer= document.getElementById('wefax-live-container');
var wefaxLiveInfo     = document.getElementById('wefax-live-info');
var wefaxLiveCanvas   = document.getElementById('wefax-live-canvas');
var wefaxGallery      = document.getElementById('wefax-gallery');
var wefaxToggleBtn    = document.getElementById('wefax-decode-toggle-btn');
var wefaxClearBtn     = document.getElementById('wefax-clear-btn');

// --- State ---
var wefaxImageHistory  = [];
var wefaxLiveCtx       = null;
var wefaxLiveLineCount = 0;
var wefaxLivePixelsPerLine = 1809;

// --- Helpers ---
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

// --- Live canvas rendering ---

function resetLiveCanvas(pixelsPerLine) {
  wefaxLivePixelsPerLine = pixelsPerLine;
  wefaxLiveLineCount = 0;
  wefaxLiveCanvas.width = pixelsPerLine;
  wefaxLiveCanvas.height = 800;
  wefaxLiveCtx = wefaxLiveCanvas.getContext('2d');
  wefaxLiveCtx.fillStyle = '#000';
  wefaxLiveCtx.fillRect(0, 0, wefaxLiveCanvas.width, wefaxLiveCanvas.height);
  wefaxLiveContainer.style.display = '';
}

function paintLine(lineBytes) {
  if (!wefaxLiveCtx) return;
  var y = wefaxLiveLineCount;

  if (y >= wefaxLiveCanvas.height) {
    var old = wefaxLiveCtx.getImageData(0, 0, wefaxLiveCanvas.width, wefaxLiveCanvas.height);
    wefaxLiveCanvas.height *= 2;
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

// --- Gallery rendering ---

function renderGalleryThumbnail(msg) {
  var card = document.createElement('div');
  card.className = 'wefax-card';
  card.style.cssText =
    'border:1px solid var(--border-color); border-radius:4px; ' +
    'padding:0.4rem; max-width:280px; cursor:pointer;';

  var ts = msg._tsMs ? new Date(msg._tsMs).toLocaleString() : '\u2014';
  var info = msg.ioc + ' IOC \u00b7 ' + msg.lpm + ' LPM \u00b7 ' + msg.line_count + ' lines';

  if (msg.path) {
    card.innerHTML =
      '<img src="/images/' + escapeHtml(msg.path.split('/').pop()) + '"' +
      ' alt="WEFAX" loading="lazy"' +
      ' style="width:100%; image-rendering:pixelated;" />' +
      '<div style="font-size:0.8rem; margin-top:0.2rem;">' + escapeHtml(ts) + '</div>' +
      '<div style="font-size:0.75rem; color:var(--text-muted);">' + info + '</div>';
  } else {
    card.innerHTML =
      '<div style="font-size:0.8rem;">' + escapeHtml(ts) + '</div>' +
      '<div style="font-size:0.75rem; color:var(--text-muted);">' + info + '</div>';
  }
  return card;
}

function renderWefaxGallery() {
  pruneWefaxHistory();
  var frag = document.createDocumentFragment();
  for (var i = 0; i < wefaxImageHistory.length; i++) {
    frag.appendChild(renderGalleryThumbnail(wefaxImageHistory[i]));
  }
  wefaxGallery.innerHTML = '';
  wefaxGallery.appendChild(frag);
}

function scheduleWefaxGalleryRender() {
  if (window.trxScheduleUiFrameJob) {
    window.trxScheduleUiFrameJob('wefax-gallery', renderWefaxGallery);
  } else {
    requestAnimationFrame(renderWefaxGallery);
  }
}

// --- SSE event handlers (public API) ---

window.onServerWefaxProgress = function (msg) {
  if (msg.line_count <= 1 || !wefaxLiveCtx) {
    resetLiveCanvas(msg.pixels_per_line || 1809);
  }

  if (msg.line_data) {
    var binary = atob(msg.line_data);
    var bytes = new Uint8Array(binary.length);
    for (var i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    paintLine(bytes);
  }

  if (wefaxLiveInfo) {
    wefaxLiveInfo.textContent =
      'Line ' + msg.line_count + ' \u00b7 ' + msg.ioc + ' IOC \u00b7 ' + msg.lpm + ' LPM';
  }
  if (wefaxStatus) {
    wefaxStatus.textContent = 'Receiving \u2014 line ' + msg.line_count;
    wefaxStatus.style.color = 'var(--text-accent)';
  }
};

window.onServerWefax = function (msg) {
  msg._tsMs = msg.ts_ms || Date.now();
  wefaxImageHistory.unshift(msg);
  pruneWefaxHistory();
  scheduleWefaxGalleryRender();

  if (wefaxLiveCtx && wefaxLiveLineCount > 0) {
    var trimmed = wefaxLiveCtx.getImageData(0, 0, wefaxLiveCanvas.width, wefaxLiveLineCount);
    wefaxLiveCanvas.height = wefaxLiveLineCount;
    wefaxLiveCtx.putImageData(trimmed, 0, 0);
  }

  if (wefaxStatus) {
    wefaxStatus.textContent = 'Complete \u2014 ' + msg.line_count + ' lines';
    wefaxStatus.style.color = '';
  }
};

window.restoreWefaxHistory = function (messages) {
  if (!messages || !messages.length) return;
  for (var i = 0; i < messages.length; i++) {
    messages[i]._tsMs = messages[i].ts_ms || Date.now();
  }
  wefaxImageHistory = messages.concat(wefaxImageHistory);
  pruneWefaxHistory();
  scheduleWefaxGalleryRender();
};

window.pruneWefaxHistoryView = function () {
  pruneWefaxHistory();
  scheduleWefaxGalleryRender();
};

window.resetWefaxHistoryView = function () {
  wefaxImageHistory = [];
  if (wefaxGallery) wefaxGallery.innerHTML = '';
  if (wefaxLiveContainer) wefaxLiveContainer.style.display = 'none';
  wefaxLiveCtx = null;
  wefaxLiveLineCount = 0;
  if (wefaxStatus) {
    wefaxStatus.textContent = 'Idle';
    wefaxStatus.style.color = '';
  }
};

// --- Button handlers ---
if (wefaxClearBtn) {
  wefaxClearBtn.addEventListener('click', function () {
    fetch('/clear_wefax_decode', { method: 'POST' });
    window.resetWefaxHistoryView();
  });
}
