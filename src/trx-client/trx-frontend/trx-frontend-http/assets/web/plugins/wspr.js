// --- WSPR Decoder Plugin (server-side decode) ---
const wsprStatus = document.getElementById("wspr-status");
const wsprPeriodEl = document.getElementById("wspr-period");
const wsprMessagesEl = document.getElementById("wspr-messages");
const wsprFilterInput = document.getElementById("wspr-filter");
const wsprPauseBtn = document.getElementById("wspr-pause-btn");
const WSPR_PERIOD_SECONDS = 120;
let wsprFilterText = "";
let wsprMessageHistory = [];
let wsprPaused = false;
let wsprBufferedWhilePaused = 0;

function currentWsprHistoryRetentionMs() {
  return typeof window.getDecodeHistoryRetentionMs === "function"
    ? window.getDecodeHistoryRetentionMs()
    : 24 * 60 * 60 * 1000;
}

function pruneWsprMessageHistory() {
  const cutoffMs = Date.now() - currentWsprHistoryRetentionMs();
  wsprMessageHistory = wsprMessageHistory.filter((msg) => Number(msg?._tsMs ?? msg?.ts_ms) >= cutoffMs);
}

function scheduleWsprHistoryRender() {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob("wspr-history", () => renderWsprHistory());
    return;
  }
  renderWsprHistory();
}

function fmtWsprTime(tsMs) {
  if (!tsMs) return "--:--:--";
  return new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function updateWsprPeriodTimer() {
  if (!wsprPeriodEl) return;
  const nowSec = Math.floor(Date.now() / 1000);
  const remaining = WSPR_PERIOD_SECONDS - (nowSec % WSPR_PERIOD_SECONDS);
  const mm = String(Math.floor(remaining / 60)).padStart(2, "0");
  const ss = String(remaining % 60).padStart(2, "0");
  wsprPeriodEl.textContent = `Next slot ${mm}:${ss}`;
}

updateWsprPeriodTimer();
setInterval(updateWsprPeriodTimer, 500);

function renderWsprRow(msg) {
  const row = document.createElement("div");
  row.className = "ft8-row";
  row.dataset.decoder = "wspr";
  const snr = Number.isFinite(msg.snr_db) ? msg.snr_db.toFixed(1) : "--";
  const dt = Number.isFinite(msg.dt_s) ? msg.dt_s.toFixed(2) : "--";
  const baseHz = Number.isFinite(window.ft8BaseHz) ? window.ft8BaseHz : null;
  const rfHz = Number.isFinite(msg.freq_hz) && Number.isFinite(baseHz) ? (baseHz + msg.freq_hz) : null;
  const freq = Number.isFinite(rfHz) ? rfHz.toFixed(0) : "--";
  const message = (msg.message || "").toString();
  row.dataset.message = message.toUpperCase();
  row.innerHTML = `<span class="ft8-time">${fmtWsprTime(msg.ts_ms)}</span><span class="ft8-snr">${snr}</span><span class="ft8-dt">${dt}</span><span class="ft8-freq">${freq}</span><span class="ft8-msg">${renderWsprMessage(message)}</span>`;
  applyWsprFilterToRow(row);
  return row;
}

function updateWsprPauseUi() {
  if (!wsprPauseBtn) return;
  wsprPauseBtn.textContent = wsprPaused ? "Resume" : "Pause";
  wsprPauseBtn.classList.toggle("active", wsprPaused);
}

function renderWsprHistory() {
  pruneWsprMessageHistory();
  if (!wsprMessagesEl || wsprPaused) {
    updateWsprPauseUi();
    return;
  }
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < wsprMessageHistory.length; i += 1) {
    fragment.appendChild(renderWsprRow(wsprMessageHistory[i]));
  }
  wsprMessagesEl.replaceChildren(fragment);
  updateWsprPauseUi();
}

function addWsprMessage(msg) {
  msg._tsMs = Number.isFinite(msg?.ts_ms) ? Number(msg.ts_ms) : Date.now();
  wsprMessageHistory.unshift(msg);
  pruneWsprMessageHistory();
  if (wsprPaused) {
    wsprBufferedWhilePaused += 1;
    updateWsprPauseUi();
    return;
  }
  scheduleWsprHistoryRender();
}

window.pruneWsprHistoryView = function() {
  pruneWsprMessageHistory();
  renderWsprHistory();
};

function escapeWsprHtml(input) {
  return input
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("\"", "&quot;");
}

function renderWsprMessage(message) {
  let out = "";
  let i = 0;
  while (i < message.length) {
    const ch = message[i];
    if (isAlphaNum(ch)) {
      let j = i + 1;
      while (j < message.length && isAlphaNum(message[j])) j++;
      const token = message.slice(i, j);
      const grid = token.toUpperCase();
      if (isMaidenheadGridToken(grid)) {
        out += `<span class="ft8-locator" data-locator-grid="${grid}" role="button" tabindex="0" aria-label="Show locator ${grid} on map">${grid}</span>`;
      } else {
        out += escapeWsprHtml(token);
      }
      i = j;
    } else {
      out += escapeWsprHtml(ch);
      i += 1;
    }
  }
  return out;
}

function extractAllGrids(message) {
  const out = [];
  const seen = new Set();
  const parts = message.toUpperCase().split(/[^A-Z0-9]+/);
  for (const token of parts) {
    if (!token) continue;
    if (isMaidenheadGridToken(token) && !seen.has(token)) {
      seen.add(token);
      out.push(token);
    }
  }
  return out;
}

function extractLikelyCallsign(message) {
  const parts = String(message || "").toUpperCase().split(/[^A-Z0-9/]+/);
  for (const token of parts) {
    if (!token) continue;
    if (token.length < 3 || token.length > 12) continue;
    if (token === "CQ" || token === "DE" || token === "QRZ" || token === "DX") continue;
    if (isMaidenheadGridToken(token)) continue;
    if (/^[A-Z0-9/]{1,5}\d[A-Z0-9/]{1,6}$/.test(token)) return token;
  }
  return null;
}

function isFtxFarewellToken(token) {
  const normalized = String(token || "").trim().toUpperCase();
  return normalized === "RR73" || normalized === "73" || normalized === "RR";
}

function isMaidenheadGridToken(token) {
  const normalized = String(token || "").trim().toUpperCase();
  return /^[A-R]{2}\d{2}(?:[A-X]{2})?$/.test(normalized) && !isFtxFarewellToken(normalized);
}

function isAlphaNum(ch) {
  return /[A-Za-z0-9]/.test(ch);
}

function activateWsprHistoryLocator(targetEl) {
  const locatorEl = targetEl?.closest?.(".ft8-locator[data-locator-grid]");
  if (!locatorEl) return false;
  const grid = String(locatorEl.dataset.locatorGrid || "").toUpperCase();
  if (!grid) return false;
  if (typeof window.navigateToMapLocator === "function") {
    window.navigateToMapLocator(grid, "wspr");
  }
  return true;
}

function applyWsprFilterToRow(row) {
  if (!wsprFilterText) {
    row.style.display = "";
    return;
  }
  const message = row.dataset.message || "";
  row.style.display = message.includes(wsprFilterText) ? "" : "none";
}

function applyWsprFilterToAll() {
  const rows = wsprMessagesEl.querySelectorAll(".ft8-row");
  rows.forEach((row) => applyWsprFilterToRow(row));
}

window.resetWsprHistoryView = function() {
  wsprMessagesEl.innerHTML = "";
  wsprMessageHistory = [];
  wsprBufferedWhilePaused = 0;
  renderWsprHistory();
  if (window.clearMapMarkersByType) window.clearMapMarkersByType("wspr");
};

if (wsprFilterInput) {
  wsprFilterInput.addEventListener("input", () => {
    wsprFilterText = wsprFilterInput.value.trim().toUpperCase();
    renderWsprHistory();
  });
}

if (wsprMessagesEl) {
  wsprMessagesEl.addEventListener("click", (event) => {
    if (!activateWsprHistoryLocator(event.target)) return;
    event.preventDefault();
    event.stopPropagation();
  });
  wsprMessagesEl.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    if (!activateWsprHistoryLocator(event.target)) return;
    event.preventDefault();
    event.stopPropagation();
  });
}

if (wsprPauseBtn) {
  wsprPauseBtn.addEventListener("click", () => {
    wsprPaused = !wsprPaused;
    if (!wsprPaused) {
      wsprBufferedWhilePaused = 0;
      renderWsprHistory();
    } else {
      updateWsprPauseUi();
    }
  });
}

document.getElementById("wspr-decode-toggle-btn").addEventListener("click", async () => {
  try { await postPath("/toggle_wspr_decode"); } catch (e) { console.error("WSPR toggle failed", e); }
});

document.getElementById("wspr-clear-btn").addEventListener("click", async () => {
  try {
    await postPath("/clear_wspr_decode");
    window.resetWsprHistoryView();
  } catch (e) {
    console.error("WSPR clear failed", e);
  }
});

window.onServerWspr = function(msg) {
  wsprStatus.textContent = wsprPaused ? "Paused" : "Receiving";
  const raw = (msg.message || "").toString();
  const grids = extractAllGrids(raw);
  const station = extractLikelyCallsign(raw);
  const baseHz = Number.isFinite(window.ft8BaseHz) ? Number(window.ft8BaseHz) : null;
  const rfHz = Number.isFinite(msg.freq_hz) && Number.isFinite(baseHz)
    ? (baseHz + Number(msg.freq_hz))
    : (Number.isFinite(msg.freq_hz) ? Number(msg.freq_hz) : null);
  if (grids.length > 0 && window.ft8MapAddLocator) {
    window.ft8MapAddLocator(raw, grids, "wspr", station, {
      ...msg,
      freq_hz: rfHz,
    });
  }
  addWsprMessage({
    receiver: window.getDecodeRigMeta ? window.getDecodeRigMeta() : null,
    ts_ms: msg.ts_ms,
    snr_db: msg.snr_db,
    dt_s: msg.dt_s,
    freq_hz: msg.freq_hz,
    message: raw,
  });
};

updateWsprPauseUi();
