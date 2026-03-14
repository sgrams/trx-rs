// --- FT2 Decoder Plugin (server-side decode) ---
// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

function ft8RenderMessageFt2(message) {
  if (typeof renderFt8Message === "function") return renderFt8Message(message);
  if (typeof ft8EscapeHtml === "function") return ft8EscapeHtml(message);
  return message;
}

const ft2Status = document.getElementById("ft2-status");
const ft2PeriodEl = document.getElementById("ft2-period");
const ft2MessagesEl = document.getElementById("ft2-messages");
const ft2FilterInput = document.getElementById("ft2-filter");
const ft2PauseBtn = document.getElementById("ft2-pause-btn");
const FT2_PERIOD_MS = 3750;
let ft2FilterText = "";
let ft2MessageHistory = [];
let ft2Paused = false;
let ft2BufferedWhilePaused = 0;

function currentFt2HistoryRetentionMs() {
  return typeof window.getDecodeHistoryRetentionMs === "function"
    ? window.getDecodeHistoryRetentionMs()
    : 24 * 60 * 60 * 1000;
}

function pruneFt2MessageHistory() {
  const cutoffMs = Date.now() - currentFt2HistoryRetentionMs();
  ft2MessageHistory = ft2MessageHistory.filter((msg) => Number(msg?._tsMs ?? msg?.ts_ms) >= cutoffMs);
}

function scheduleFt2Ui(key, job) {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob(key, job);
    return;
  }
  job();
}

function scheduleFt2HistoryRender() { scheduleFt2Ui("ft2-history", () => renderFt2History()); }

function normalizeFt2DisplayFreqHz(freqHz) {
  const rawHz = Number(freqHz);
  if (!Number.isFinite(rawHz)) return null;
  const baseHz = Number.isFinite(window.ft8BaseHz) ? Number(window.ft8BaseHz) : null;
  if (Number.isFinite(baseHz) && baseHz > 0 && rawHz >= 0 && rawHz < 100000) {
    return baseHz + rawHz;
  }
  return rawHz;
}

function updateFt2PeriodTimer() {
  if (!ft2PeriodEl) return;
  const nowMs = Date.now();
  const remaining = (FT2_PERIOD_MS - nowMs % FT2_PERIOD_MS) / 1000;
  ft2PeriodEl.textContent = `Next slot ${remaining.toFixed(1)}s`;
}

updateFt2PeriodTimer();
setInterval(updateFt2PeriodTimer, 250);

function renderFt2Row(msg) {
  const row = document.createElement("div");
  row.className = "ft8-row";
  const rawMessage = (msg.message || "").toString();
  row.dataset.message = rawMessage.toUpperCase();
  row.dataset.decoder = "ft2";
  row.dataset.storedFreqHz = Number.isFinite(msg.freq_hz) ? String(msg.freq_hz) : "";
  const snr = Number.isFinite(msg.snr_db) ? msg.snr_db.toFixed(1) : "--";
  const dt = Number.isFinite(msg.dt_s) ? msg.dt_s.toFixed(2) : "--";
  const displayFreqHz = normalizeFt2DisplayFreqHz(msg.freq_hz);
  const freq = Number.isFinite(displayFreqHz) ? displayFreqHz.toFixed(0) : "--";
  const renderedMessage = ft8RenderMessageFt2(rawMessage);
  const tsMs = msg._tsMs ?? msg.ts_ms;
  const timeStr = tsMs ? new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" }) : "--:--:--";
  row.innerHTML = `<span class="ft8-time">${timeStr}</span><span class="ft8-snr">${snr}</span><span class="ft8-dt">${dt}</span><span class="ft8-freq">${freq}</span><span class="ft8-msg">${renderedMessage}</span>`;
  return row;
}

function updateFt2PauseUi() {
  if (!ft2PauseBtn) return;
  ft2PauseBtn.textContent = ft2Paused ? "Resume" : "Pause";
  ft2PauseBtn.classList.toggle("active", ft2Paused);
}

function renderFt2History() {
  pruneFt2MessageHistory();
  if (!ft2MessagesEl || ft2Paused) { updateFt2PauseUi(); return; }
  const filter = ft2FilterText;
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < ft2MessageHistory.length; i++) {
    const msg = ft2MessageHistory[i];
    if (filter && !(msg.message || "").toString().toUpperCase().includes(filter)) continue;
    fragment.appendChild(renderFt2Row(msg));
  }
  ft2MessagesEl.replaceChildren(fragment);
  updateFt2PauseUi();
}

function addFt2Message(msg) {
  msg._tsMs = Number.isFinite(msg?.ts_ms) ? Number(msg.ts_ms) : Date.now();
  ft2MessageHistory.unshift(msg);
  pruneFt2MessageHistory();
  if (ft2Paused) { ft2BufferedWhilePaused += 1; updateFt2PauseUi(); return; }
  scheduleFt2HistoryRender();
}

function normalizeServerFt2Message(msg) {
  const raw = (msg.message || "").toString();
  const locatorDetails = typeof ft8ExtractLocatorDetails === "function" ? ft8ExtractLocatorDetails(raw) : [];
  const grids = locatorDetails.length > 0
    ? locatorDetails.map((d) => d.grid)
    : (typeof ft8ExtractAllGrids === "function" ? ft8ExtractAllGrids(raw) : []);
  const station = typeof ft8ExtractLikelyCallsign === "function" ? ft8ExtractLikelyCallsign(raw) : null;
  const rfHz = normalizeFt2DisplayFreqHz(msg.freq_hz);
  return {
    raw, grids, station, rfHz, locatorDetails,
    history: {
      receiver: window.getDecodeRigMeta ? window.getDecodeRigMeta() : null,
      ts_ms: msg.ts_ms, snr_db: msg.snr_db, dt_s: msg.dt_s,
      freq_hz: Number.isFinite(rfHz) ? rfHz : msg.freq_hz,
      message: msg.message,
    },
  };
}

window.onServerFt2Batch = function(messages) {
  if (!Array.isArray(messages) || messages.length === 0) return;
  if (ft2Status) ft2Status.textContent = ft2Paused ? "Paused" : "Receiving";
  const normalized = [];
  for (const msg of messages) {
    const next = normalizeServerFt2Message(msg);
    if (next.grids.length > 0 && window.mapAddLocator) {
      window.mapAddLocator(next.raw, next.grids, "ft2", next.station, { ...msg, freq_hz: next.rfHz, locator_details: next.locatorDetails });
    }
    next.history._tsMs = Number.isFinite(next.history?.ts_ms) ? Number(next.history.ts_ms) : Date.now();
    normalized.push(next.history);
  }
  normalized.reverse();
  ft2MessageHistory = normalized.concat(ft2MessageHistory);
  pruneFt2MessageHistory();
  if (ft2Paused) { ft2BufferedWhilePaused += messages.length; updateFt2PauseUi(); return; }
  scheduleFt2HistoryRender();
};

window.restoreFt2History = function(messages) { window.onServerFt2Batch(messages); };
window.pruneFt2HistoryView = function() { pruneFt2MessageHistory(); renderFt2History(); };

window.resetFt2HistoryView = function() {
  if (ft2MessagesEl) ft2MessagesEl.innerHTML = "";
  ft2MessageHistory = [];
  ft2BufferedWhilePaused = 0;
  renderFt2History();
};

if (ft2FilterInput) {
  ft2FilterInput.addEventListener("input", () => {
    ft2FilterText = ft2FilterInput.value.trim().toUpperCase();
    renderFt2History();
  });
}

if (ft2PauseBtn) {
  ft2PauseBtn.addEventListener("click", () => {
    ft2Paused = !ft2Paused;
    if (!ft2Paused) { ft2BufferedWhilePaused = 0; renderFt2History(); } else { updateFt2PauseUi(); }
  });
}

document.getElementById("ft2-decode-toggle-btn")?.addEventListener("click", async () => {
  try { await postPath("/toggle_ft2_decode"); } catch (e) { console.error("FT2 toggle failed", e); }
});

document.getElementById("ft2-clear-btn")?.addEventListener("click", async () => {
  try {
    await postPath("/clear_ft2_decode");
    window.resetFt2HistoryView();
  } catch (e) { console.error("FT2 clear failed", e); }
});

window.onServerFt2 = function(msg) {
  if (ft2Status) ft2Status.textContent = ft2Paused ? "Paused" : "Receiving";
  const next = normalizeServerFt2Message(msg);
  if (next.grids.length > 0 && window.mapAddLocator) {
    window.mapAddLocator(next.raw, next.grids, "ft2", next.station, { ...msg, freq_hz: next.rfHz, locator_details: next.locatorDetails });
  }
  addFt2Message(next.history);
};

updateFt2PauseUi();
