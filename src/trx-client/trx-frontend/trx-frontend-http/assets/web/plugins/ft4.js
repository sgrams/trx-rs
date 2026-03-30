// --- FT4 Decoder Plugin (server-side decode) ---
// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
// SPDX-License-Identifier: BSD-2-Clause

function ft8RenderMessage(message) {
  if (typeof renderFt8Message === "function") return renderFt8Message(message);
  if (typeof ft8EscapeHtml === "function") return ft8EscapeHtml(message);
  return message;
}

const ft4Status = document.getElementById("ft4-status");
const ft4PeriodEl = document.getElementById("ft4-period");
const ft4MessagesEl = document.getElementById("ft4-messages");
const ft4FilterInput = document.getElementById("ft4-filter");
const FT4_PERIOD_MS = 7500;
const FT4_MAX_DOM_ROWS = 200;
let ft4FilterText = "";
let ft4MessageHistory = [];

function currentFt4HistoryRetentionMs() {
  return typeof window.getDecodeHistoryRetentionMs === "function"
    ? window.getDecodeHistoryRetentionMs()
    : 24 * 60 * 60 * 1000;
}

function pruneFt4MessageHistory() {
  const cutoffMs = Date.now() - currentFt4HistoryRetentionMs();
  ft4MessageHistory = ft4MessageHistory.filter((msg) => Number(msg?._tsMs ?? msg?.ts_ms) >= cutoffMs);
}

function scheduleFt4Ui(key, job) {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob(key, job);
    return;
  }
  job();
}

function scheduleFt4HistoryRender() { scheduleFt4Ui("ft4-history", () => renderFt4History()); }

function normalizeFt4DisplayFreqHz(freqHz) {
  const rawHz = Number(freqHz);
  if (!Number.isFinite(rawHz)) return null;
  const baseHz = Number.isFinite(window.ft8BaseHz) ? Number(window.ft8BaseHz) : null;
  if (Number.isFinite(baseHz) && baseHz > 0 && rawHz >= 0 && rawHz < 100000) {
    return baseHz + rawHz;
  }
  return rawHz;
}

function updateFt4PeriodTimer() {
  if (!ft4PeriodEl) return;
  const nowMs = Date.now();
  const remaining = (FT4_PERIOD_MS - nowMs % FT4_PERIOD_MS) / 1000;
  ft4PeriodEl.textContent = `Next slot ${remaining.toFixed(1)}s`;
}

updateFt4PeriodTimer();
setInterval(updateFt4PeriodTimer, 250);

function renderFt4Row(msg) {
  const row = document.createElement("div");
  row.className = "ft8-row";
  const rawMessage = (msg.message || "").toString();
  row.dataset.message = rawMessage.toUpperCase();
  row.dataset.decoder = "ft4";
  row.dataset.storedFreqHz = Number.isFinite(msg.freq_hz) ? String(msg.freq_hz) : "";
  const snr = Number.isFinite(msg.snr_db) ? msg.snr_db.toFixed(1) : "--";
  const dt = Number.isFinite(msg.dt_s) ? msg.dt_s.toFixed(2) : "--";
  const displayFreqHz = normalizeFt4DisplayFreqHz(msg.freq_hz);
  const freq = Number.isFinite(displayFreqHz) ? displayFreqHz.toFixed(0) : "--";
  const renderedMessage = ft8RenderMessage(rawMessage);
  const tsMs = msg._tsMs ?? msg.ts_ms;
  const timeStr = tsMs ? new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" }) : "--:--:--";
  row.innerHTML = `<span class="ft8-time">${timeStr}</span><span class="ft8-snr">${snr}</span><span class="ft8-dt">${dt}</span><span class="ft8-freq">${freq}</span><span class="ft8-msg">${renderedMessage}</span>`;
  return row;
}

function renderFt4History() {
  pruneFt4MessageHistory();
  if (!ft4MessagesEl) return;
  const filter = ft4FilterText;
  const fragment = document.createDocumentFragment();
  let rendered = 0;
  for (let i = 0; i < ft4MessageHistory.length && rendered < FT4_MAX_DOM_ROWS; i++) {
    const msg = ft4MessageHistory[i];
    if (filter && !(msg.message || "").toString().toUpperCase().includes(filter)) continue;
    fragment.appendChild(renderFt4Row(msg));
    rendered++;
  }
  ft4MessagesEl.replaceChildren(fragment);
}

function addFt4Message(msg) {
  msg._tsMs = Number.isFinite(msg?.ts_ms) ? Number(msg.ts_ms) : Date.now();
  ft4MessageHistory.unshift(msg);
  pruneFt4MessageHistory();
  window.setFt8FamilyBarDecoder?.("ft4");
  window.updateFt8Bar?.();
  scheduleFt4HistoryRender();
}

function normalizeServerFt4Message(msg) {
  const raw = (msg.message || "").toString();
  const locatorDetails = typeof ft8ExtractLocatorDetails === "function" ? ft8ExtractLocatorDetails(raw) : [];
  const grids = locatorDetails.length > 0
    ? locatorDetails.map((d) => d.grid)
    : (typeof ft8ExtractAllGrids === "function" ? ft8ExtractAllGrids(raw) : []);
  const station = typeof ft8ExtractLikelyCallsign === "function" ? ft8ExtractLikelyCallsign(raw) : null;
  const rfHz = normalizeFt4DisplayFreqHz(msg.freq_hz);
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

window.onServerFt4Batch = function(messages) {
  if (!Array.isArray(messages) || messages.length === 0) return;
  if (ft4Status) ft4Status.textContent = "Receiving";
  const normalized = [];
  for (const msg of messages) {
    const next = normalizeServerFt4Message(msg);
    if (next.grids.length > 0 && window.mapAddLocator) {
      window.mapAddLocator(next.raw, next.grids, "ft4", next.station, { ...msg, freq_hz: next.rfHz, locator_details: next.locatorDetails });
    }
    next.history._tsMs = Number.isFinite(next.history?.ts_ms) ? Number(next.history.ts_ms) : Date.now();
    normalized.push(next.history);
  }
  normalized.reverse();
  ft4MessageHistory = normalized.concat(ft4MessageHistory);
  pruneFt4MessageHistory();
  window.setFt8FamilyBarDecoder?.("ft4");
  window.updateFt8Bar?.();
  scheduleFt4HistoryRender();
};

window.restoreFt4History = function(messages) { window.onServerFt4Batch(messages); };
window.pruneFt4HistoryView = function() { pruneFt4MessageHistory(); renderFt4History(); };

window.resetFt4HistoryView = function() {
  if (ft4MessagesEl) ft4MessagesEl.innerHTML = "";
  ft4MessageHistory = [];
  window.updateFt8Bar?.();
  renderFt4History();
};

function buildFt4BarFrames() {
  const cutoffMs = Date.now() - 15 * 60 * 1000;
  const messages = ft4MessageHistory.filter((msg) => Number(msg._tsMs ?? msg.ts_ms) >= cutoffMs).slice(0, 8);
  const newestTsMs = messages.reduce((latest, msg) => Math.max(latest, Number(msg._tsMs ?? msg.ts_ms) || 0), 0);
  if (messages.length === 0) {
    return { count: 0, newestTsMs: 0, html: "" };
  }
  let html = "";
  for (const msg of messages) {
    const tsMs = msg._tsMs ?? msg.ts_ms;
    const ts = tsMs ? `<span class="aprs-bar-time">${fmtTime(tsMs)}</span>` : "";
    const snr = Number.isFinite(msg.snr_db) ? `${msg.snr_db.toFixed(1)} dB` : "-- dB";
    const dt = Number.isFinite(msg.dt_s) ? `dt ${msg.dt_s.toFixed(2)}` : null;
    const displayFreqHz = normalizeFt4DisplayFreqHz(msg.freq_hz);
    const rf = Number.isFinite(displayFreqHz) ? `${displayFreqHz.toFixed(0)} Hz` : null;
    const detail = [snr, dt, rf].filter(Boolean).join(" · ");
    const text = ft8RenderMessage((msg.message || "").toString());
    html += `<div class="aprs-bar-frame"><div class="aprs-bar-frame-main">${ts}<span class="aprs-bar-call">${text}</span>${detail ? ` · ${detail}` : ""}</div></div>`;
  }
  return { count: messages.length, newestTsMs, html };
}
window.registerFt8FamilyBarRenderer?.("ft4", buildFt4BarFrames);

if (ft4FilterInput) {
  ft4FilterInput.addEventListener("input", () => {
    ft4FilterText = ft4FilterInput.value.trim().toUpperCase();
    renderFt4History();
  });
}

const ft4DecodeToggleBtn = document.getElementById("ft4-decode-toggle-btn");
ft4DecodeToggleBtn?.addEventListener("click", async () => {
  try {
    await window.takeSchedulerControlForDecoderDisable?.(ft4DecodeToggleBtn);
    await postPath("/toggle_ft4_decode");
  } catch (e) {
    console.error("FT4 toggle failed", e);
  }
});

document.getElementById("settings-clear-ft4-history")?.addEventListener("click", async () => {
  if (!confirm("Clear all FT4 decode history? This cannot be undone.")) return;
  try {
    await postPath("/clear_ft4_decode");
    window.resetFt4HistoryView();
  } catch (e) { console.error("FT4 history clear failed", e); }
});

window.onServerFt4 = function(msg) {
  if (ft4Status) ft4Status.textContent = "Receiving";
  const next = normalizeServerFt4Message(msg);
  if (next.grids.length > 0 && window.mapAddLocator) {
    window.mapAddLocator(next.raw, next.grids, "ft4", next.station, { ...msg, freq_hz: next.rfHz, locator_details: next.locatorDetails });
  }
  addFt4Message(next.history);
};
