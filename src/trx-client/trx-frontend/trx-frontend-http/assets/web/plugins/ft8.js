// --- FT8 Decoder Plugin (server-side decode) ---
const ft8Status = document.getElementById("ft8-status");
const ft8PeriodEl = document.getElementById("ft8-period");
const ft8MessagesEl = document.getElementById("ft8-messages");
const ft8FilterInput = document.getElementById("ft8-filter");
const ft8BarOverlay = document.getElementById("ft8-bar-overlay");
const FT8_BAR_WINDOW_MS = 15 * 60 * 1000;
const FT8_PERIOD_SECONDS = 15;
const FT8_MAX_DOM_ROWS = 200;
const FT8_BAR_DECODER_LABELS = {
  ft8: "FT8",
  ft4: "FT4",
  ft2: "FT2",
};
let ft8FilterText = "";
let ft8MessageHistory = [];
let ft8BarActiveDecoder = "ft8";
const ft8BarBuilders = {};
const ft8BarDismissedAtMsByDecoder = {
  ft8: 0,
  ft4: 0,
  ft2: 0,
};

function currentFt8HistoryRetentionMs() {
  return typeof window.getDecodeHistoryRetentionMs === "function"
    ? window.getDecodeHistoryRetentionMs()
    : 24 * 60 * 60 * 1000;
}

function pruneFt8MessageHistory() {
  const cutoffMs = Date.now() - currentFt8HistoryRetentionMs();
  ft8MessageHistory = ft8MessageHistory.filter((msg) => Number(msg?._tsMs ?? msg?.ts_ms) >= cutoffMs);
}

function scheduleFt8Ui(key, job) {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob(key, job);
    return;
  }
  job();
}

function scheduleFt8HistoryRender() {
  scheduleFt8Ui("ft8-history", () => renderFt8History());
}

function scheduleFt8BarUpdate() {
  scheduleFt8Ui("ft8-bar", () => updateFt8Bar());
}

window.registerFt8FamilyBarRenderer = function(decoder, builder) {
  if (!FT8_BAR_DECODER_LABELS[decoder] || typeof builder !== "function") return;
  ft8BarBuilders[decoder] = builder;
};

window.setFt8FamilyBarDecoder = function(decoder) {
  if (!FT8_BAR_DECODER_LABELS[decoder]) return;
  ft8BarActiveDecoder = decoder;
  scheduleFt8BarUpdate();
};

function normalizeFt8DisplayFreqHz(freqHz) {
  const rawHz = Number(freqHz);
  if (!Number.isFinite(rawHz)) return null;
  const baseHz = Number.isFinite(window.ft8BaseHz) ? Number(window.ft8BaseHz) : null;
  if (Number.isFinite(baseHz) && baseHz > 0 && rawHz >= 0 && rawHz < 100000) {
    return baseHz + rawHz;
  }
  return rawHz;
}

function fmtTime(tsMs) {
  if (!tsMs) return "--:--:--";
  return new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function updateFt8PeriodTimer() {
  if (!ft8PeriodEl) return;
  const nowSec = Math.floor(Date.now() / 1000);
  const remaining = FT8_PERIOD_SECONDS - (nowSec % FT8_PERIOD_SECONDS);
  ft8PeriodEl.textContent = `Next slot ${String(remaining).padStart(2, "0")}s`;
}

updateFt8PeriodTimer();
setInterval(updateFt8PeriodTimer, 500);

function renderFt8Row(msg) {
  const row = document.createElement("div");
  row.className = "ft8-row";
  const rawMessage = (msg.message || "").toString();
  row.dataset.message = rawMessage.toUpperCase();
  row.dataset.decoder = "ft8";
  row.dataset.storedFreqHz = Number.isFinite(msg.freq_hz) ? String(msg.freq_hz) : "";
  const snr = Number.isFinite(msg.snr_db) ? msg.snr_db.toFixed(1) : "--";
  const dt = Number.isFinite(msg.dt_s) ? msg.dt_s.toFixed(2) : "--";
  const displayFreqHz = normalizeFt8DisplayFreqHz(msg.freq_hz);
  const freq = Number.isFinite(displayFreqHz) ? displayFreqHz.toFixed(0) : "--";
  const renderedMessage = renderFt8Message(rawMessage);
  row.innerHTML = `<span class="ft8-time">${fmtTime(msg.ts_ms)}</span><span class="ft8-snr">${snr}</span><span class="ft8-dt">${dt}</span><span class="ft8-freq">${freq}</span><span class="ft8-msg">${renderedMessage}</span>`;
  applyFt8FilterToRow(row);
  return row;
}

function renderFt8History() {
  pruneFt8MessageHistory();
  if (!ft8MessagesEl) return;
  const fragment = document.createDocumentFragment();
  const limit = Math.min(ft8MessageHistory.length, FT8_MAX_DOM_ROWS);
  for (let i = 0; i < limit; i += 1) {
    fragment.appendChild(renderFt8Row(ft8MessageHistory[i]));
  }
  ft8MessagesEl.replaceChildren(fragment);
}

function addFt8Message(msg) {
  msg._tsMs = Number.isFinite(msg?.ts_ms) ? Number(msg.ts_ms) : Date.now();
  ft8MessageHistory.unshift(msg);
  pruneFt8MessageHistory();
  ft8BarActiveDecoder = "ft8";
  scheduleFt8BarUpdate();
  scheduleFt8HistoryRender();
}

function normalizeServerFt8Message(msg) {
  const raw = (msg.message || "").toString();
  const locatorDetails = ft8ExtractLocatorDetails(raw);
  const grids = locatorDetails.length > 0
    ? locatorDetails.map((detail) => detail.grid)
    : ft8ExtractAllGrids(raw);
  const station = ft8ExtractLikelyCallsign(raw);
  const rfHz = normalizeFt8DisplayFreqHz(msg.freq_hz);
  return {
    raw,
    grids,
    station,
    rfHz,
    locatorDetails,
    history: {
      receiver: window.getDecodeRigMeta ? window.getDecodeRigMeta() : null,
      ts_ms: msg.ts_ms,
      snr_db: msg.snr_db,
      dt_s: msg.dt_s,
      freq_hz: Number.isFinite(rfHz) ? rfHz : msg.freq_hz,
      message: msg.message,
    },
  };
}

window.onServerFt8Batch = function(messages) {
  if (!Array.isArray(messages) || messages.length === 0) return;
  ft8Status.textContent = "Receiving";
  const normalized = [];
  for (const msg of messages) {
    const next = normalizeServerFt8Message(msg);
    if (next.grids.length > 0 && window.mapAddLocator) {
      window.mapAddLocator(next.raw, next.grids, "ft8", next.station, {
        ...msg,
        freq_hz: next.rfHz,
        locator_details: next.locatorDetails,
      });
    }
    next.history._tsMs = Number.isFinite(next.history?.ts_ms) ? Number(next.history.ts_ms) : Date.now();
    normalized.push(next.history);
  }
  normalized.reverse();
  ft8MessageHistory = normalized.concat(ft8MessageHistory);
  pruneFt8MessageHistory();
  ft8BarActiveDecoder = "ft8";
  scheduleFt8BarUpdate();
  scheduleFt8HistoryRender();
};

window.restoreFt8History = function(messages) {
  window.onServerFt8Batch(messages);
};

window.pruneFt8HistoryView = function() {
  pruneFt8MessageHistory();
  updateFt8Bar();
  renderFt8History();
};

function ft8BarRfText(msg) {
  const displayFreqHz = normalizeFt8DisplayFreqHz(msg.freq_hz);
  if (!Number.isFinite(displayFreqHz)) return null;
  return `${displayFreqHz.toFixed(0)} Hz`;
}

function buildFt8BarFrames() {
  const cutoffMs = Date.now() - FT8_BAR_WINDOW_MS;
  const messages = ft8MessageHistory.filter((msg) => Number(msg.ts_ms) >= cutoffMs).slice(0, 8);
  const newestTsMs = messages.reduce((latest, msg) => Math.max(latest, Number(msg.ts_ms) || 0), 0);
  if (messages.length === 0) {
    return { count: 0, newestTsMs: 0, html: "" };
  }
  let html = "";
  for (const msg of messages) {
    const ts = msg.ts_ms ? `<span class="aprs-bar-time">${fmtTime(msg.ts_ms)}</span>` : "";
    const snr = Number.isFinite(msg.snr_db) ? `${msg.snr_db.toFixed(1)} dB` : "-- dB";
    const dt = Number.isFinite(msg.dt_s) ? `dt ${msg.dt_s.toFixed(2)}` : null;
    const rf = ft8BarRfText(msg);
    const detail = [snr, dt, rf].filter(Boolean).join(" · ");
    const text = ft8EscapeHtml((msg.message || "").toString());
    html += `<div class="aprs-bar-frame"><div class="aprs-bar-frame-main">${ts}<span class="aprs-bar-call">${text}</span>${detail ? ` · ${detail}` : ""}</div></div>`;
  }
  return { count: messages.length, newestTsMs, html };
}

function updateFt8Bar() {
  if (!ft8BarOverlay) return;
  const modeUpper = (document.getElementById("mode")?.value || "").toUpperCase();
  const isFt8Mode = modeUpper === "DIG" || modeUpper === "USB";
  const decoder = ft8BarActiveDecoder;
  const builder = ft8BarBuilders[decoder];
  const label = FT8_BAR_DECODER_LABELS[decoder] || "FT8";
  const result = typeof builder === "function" ? builder() : null;
  const newestTsMs = Number(result?.newestTsMs) || 0;
  if (!isFt8Mode || !result || result.count === 0 || newestTsMs <= (ft8BarDismissedAtMsByDecoder[decoder] || 0)) {
    ft8BarOverlay.style.display = "none";
    ft8BarOverlay.innerHTML = "";
    return;
  }

  ft8BarOverlay.innerHTML = `<div class="aprs-bar-header"><span class="aprs-bar-title"><span class="aprs-bar-title-word">${label}</span><span class="aprs-bar-title-word">Live</span></span><span class="aprs-bar-actions"><span class="aprs-bar-window">Last 15 minutes</span><span class="aprs-bar-clear-wrap"><span class="aprs-bar-clear" role="button" tabindex="0" onclick="window.clearFt8Bar()" onkeydown="if(event.key===\'Enter\'||event.key===\' \'){event.preventDefault();window.clearFt8Bar();}" aria-label="Clear ${label} overlay">Clear</span></span><button class="aprs-bar-close" type="button" onclick="window.closeFt8Bar()" aria-label="Close ${label} overlay">&times;</button></span></div>${result.html}`;
  ft8BarOverlay.style.display = "flex";
}
window.updateFt8Bar = updateFt8Bar;
window.clearFt8Bar = function() {
  const decoder = ft8BarActiveDecoder;
  if (decoder === "ft4") {
    window.resetFt4HistoryView?.();
    return;
  }
  if (decoder === "ft2") {
    window.resetFt2HistoryView?.();
    return;
  }
  window.resetFt8HistoryView?.();
};
window.closeFt8Bar = function() {
  ft8BarDismissedAtMsByDecoder[ft8BarActiveDecoder] = Date.now();
  if (ft8BarOverlay) {
    ft8BarOverlay.style.display = "none";
    ft8BarOverlay.innerHTML = "";
  }
};
window.registerFt8FamilyBarRenderer("ft8", buildFt8BarFrames);

function renderFt8Message(message) {
  let out = "";
  let i = 0;
  while (i < message.length) {
    const ch = message[i];
    if (ft8IsAlphaNum(ch)) {
      let j = i + 1;
      while (j < message.length && ft8IsAlphaNum(message[j])) j++;
      const token = message.slice(i, j);
      const grid = token.toUpperCase();
      if (ft8IsMaidenheadGridToken(grid)) {
        out += `<span class="ft8-locator" data-locator-grid="${grid}" role="button" tabindex="0" aria-label="Show locator ${grid} on map">${grid}</span>`;
      } else {
        out += ft8EscapeHtml(token);
      }
      i = j;
    } else {
      out += ft8EscapeHtml(ch);
      i += 1;
    }
  }
  return out;
}

function ft8TokenizeMessage(message) {
  return String(message || "")
    .toUpperCase()
    .split(/[^A-Z0-9/]+/)
    .filter(Boolean);
}

function ft8ExtractAllGrids(message) {
  const out = [];
  const seen = new Set();
  let i = 0;
  while (i < message.length) {
    if (ft8IsAlphaNum(message[i])) {
      let j = i + 1;
      while (j < message.length && ft8IsAlphaNum(message[j])) j++;
      const token = message.slice(i, j);
      const grid = token.toUpperCase();
      if (ft8IsMaidenheadGridToken(grid) && !seen.has(grid)) {
        seen.add(grid);
        out.push(grid);
      }
      i = j;
    } else {
      i += 1;
    }
  }
  return out;
}

function ft8ExtractLocatorDetails(message) {
  const tokens = ft8TokenizeMessage(message);
  const grids = ft8ExtractAllGrids(String(message || ""));
  if (tokens.length === 0 || grids.length === 0) return [];
  const firstGridIdx = tokens.findIndex((token) => ft8IsMaidenheadGridToken(token));
  const limit = firstGridIdx >= 0 ? firstGridIdx : tokens.length;
  const callsigns = [];
  for (let i = 0; i < limit; i += 1) {
    if (ft8IsLikelyCallsignToken(tokens[i])) callsigns.push(tokens[i]);
  }

  let source = null;
  let target = null;
  const head = tokens[0];
  if (callsigns.length > 0) {
    if (head === "CQ" || head === "DE" || head === "QRZ") {
      source = callsigns[0];
    } else if (callsigns.length >= 2) {
      target = callsigns[0];
      source = callsigns[1];
    } else {
      source = callsigns[0];
    }
  }

  return grids.map((grid) => ({
    grid,
    station: source || null,
    source: source || null,
    target: target || null,
  }));
}

function ft8ExtractLikelyCallsign(message) {
  const locatorDetails = ft8ExtractLocatorDetails(message);
  if (locatorDetails.length > 0 && locatorDetails[0].station) {
    return locatorDetails[0].station;
  }
  const tokens = ft8TokenizeMessage(message);
  for (const token of tokens) {
    if (ft8IsLikelyCallsignToken(token)) return token;
  }
  return null;
}

function ft8IsLikelyCallsignToken(token) {
  if (!token) return false;
  if (token.length < 3 || token.length > 12) return false;
  if (token === "CQ" || token === "DE" || token === "QRZ" || token === "DX") return false;
  if (ft8IsMaidenheadGridToken(token)) return false;
  return /^[A-Z0-9/]{1,5}\d[A-Z0-9/]{1,6}$/.test(token);
}

function ft8IsFarewellToken(token) {
  const normalized = String(token || "").trim().toUpperCase();
  return normalized === "RR73" || normalized === "73" || normalized === "RR";
}

function ft8IsMaidenheadGridToken(token) {
  const normalized = String(token || "").trim().toUpperCase();
  return /^[A-R]{2}\d{2}(?:[A-X]{2})?$/.test(normalized) && !ft8IsFarewellToken(normalized);
}

function ft8EscapeHtml(input) {
  return input
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("\"", "&quot;");
}

function ft8IsAlphaNum(ch) {
  return /[A-Za-z0-9]/.test(ch);
}

function activateFt8HistoryLocator(targetEl) {
  const locatorEl = targetEl?.closest?.(".ft8-locator[data-locator-grid]");
  if (!locatorEl) return false;
  const grid = String(locatorEl.dataset.locatorGrid || "").toUpperCase();
  if (!grid) return false;
  if (typeof window.navigateToMapLocator === "function") {
    window.navigateToMapLocator(grid, "ft8");
  }
  return true;
}

function applyFt8FilterToRow(row) {
  if (!ft8FilterText) {
    row.style.display = "";
    return;
  }
  const message = row.dataset.message || "";
  row.style.display = message.includes(ft8FilterText) ? "" : "none";
}

function applyFt8FilterToAll() {
  const rows = ft8MessagesEl.querySelectorAll(".ft8-row");
  rows.forEach((row) => applyFt8FilterToRow(row));
}

function updateFt8RowRf(row) {
  const freqEl = row.querySelector(".ft8-freq");
  if (!freqEl) return;
  const storedFreqHz = row.dataset.storedFreqHz ? Number(row.dataset.storedFreqHz) : NaN;
  const displayFreqHz = normalizeFt8DisplayFreqHz(storedFreqHz);
  if (Number.isFinite(displayFreqHz)) {
    freqEl.textContent = displayFreqHz.toFixed(0);
  } else {
    freqEl.textContent = "--";
  }
}

window.updateFt8RfDisplay = function() {
  const rows = ft8MessagesEl.querySelectorAll(".ft8-row");
  rows.forEach((row) => updateFt8RowRf(row));
  updateFt8Bar();
};

window.resetFt8HistoryView = function() {
  ft8MessagesEl.innerHTML = "";
  ft8MessageHistory = [];
  updateFt8Bar();
  renderFt8History();
  if (window.clearMapMarkersByType) window.clearMapMarkersByType("ft8");
};

if (ft8FilterInput) {
  ft8FilterInput.addEventListener("input", () => {
    ft8FilterText = ft8FilterInput.value.trim().toUpperCase();
    renderFt8History();
  });
}

if (ft8MessagesEl) {
  ft8MessagesEl.addEventListener("click", (event) => {
    if (!activateFt8HistoryLocator(event.target)) return;
    event.preventDefault();
    event.stopPropagation();
  });
  ft8MessagesEl.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    if (!activateFt8HistoryLocator(event.target)) return;
    event.preventDefault();
    event.stopPropagation();
  });
}

const ft8DecodeToggleBtn = document.getElementById("ft8-decode-toggle-btn");
ft8DecodeToggleBtn?.addEventListener("click", async () => {
  try {
    await window.takeSchedulerControlForDecoderDisable?.(ft8DecodeToggleBtn);
    await postPath("/toggle_ft8_decode");
  } catch (e) {
    console.error("FT8 toggle failed", e);
  }
});

document.getElementById("settings-clear-ft8-history")?.addEventListener("click", async () => {
  if (!confirm("Clear all FT8 decode history? This cannot be undone.")) return;
  try {
    await postPath("/clear_ft8_decode");
    window.resetFt8HistoryView();
  } catch (e) {
    console.error("FT8 history clear failed", e);
  }
});

// --- Server-side FT8 decode handler ---
window.onServerFt8 = function(msg) {
  ft8Status.textContent = "Receiving";
  const next = normalizeServerFt8Message(msg);
  if (next.grids.length > 0 && window.mapAddLocator) {
    window.mapAddLocator(next.raw, next.grids, "ft8", next.station, {
      ...msg,
      freq_hz: next.rfHz,
      locator_details: next.locatorDetails,
    });
  }
  addFt8Message(next.history);
};
