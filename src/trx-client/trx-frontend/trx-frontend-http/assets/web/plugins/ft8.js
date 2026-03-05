// --- FT8 Decoder Plugin (server-side decode) ---
const ft8Status = document.getElementById("ft8-status");
const ft8PeriodEl = document.getElementById("ft8-period");
const ft8MessagesEl = document.getElementById("ft8-messages");
const ft8FilterInput = document.getElementById("ft8-filter");
const ft8PauseBtn = document.getElementById("ft8-pause-btn");
const ft8BarOverlay = document.getElementById("ft8-bar-overlay");
const FT8_MAX_MESSAGES = 200;
const FT8_BAR_WINDOW_MS = 15 * 60 * 1000;
const FT8_PERIOD_SECONDS = 15;
let ft8FilterText = "";
let ft8MessageHistory = [];
let ft8Paused = false;
let ft8BufferedWhilePaused = 0;

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

function updateFt8PauseUi() {
  if (!ft8PauseBtn) return;
  ft8PauseBtn.textContent = ft8Paused ? "Resume" : "Pause";
  ft8PauseBtn.classList.toggle("active", ft8Paused);
}

function renderFt8History() {
  if (!ft8MessagesEl || ft8Paused) {
    updateFt8PauseUi();
    return;
  }
  ft8MessagesEl.innerHTML = "";
  for (let i = 0; i < ft8MessageHistory.length; i += 1) {
    ft8MessagesEl.appendChild(renderFt8Row(ft8MessageHistory[i]));
  }
  updateFt8PauseUi();
}

function addFt8Message(msg) {
  ft8MessageHistory.unshift(msg);
  if (ft8MessageHistory.length > FT8_MAX_MESSAGES) ft8MessageHistory.length = FT8_MAX_MESSAGES;
  updateFt8Bar();
  if (ft8Paused) {
    ft8BufferedWhilePaused += 1;
    updateFt8PauseUi();
    return;
  }
  renderFt8History();
}

function ft8BarRfText(msg) {
  const displayFreqHz = normalizeFt8DisplayFreqHz(msg.freq_hz);
  if (!Number.isFinite(displayFreqHz)) return null;
  return `${displayFreqHz.toFixed(0)} Hz`;
}

function updateFt8Bar() {
  if (!ft8BarOverlay) return;
  const modeUpper = (document.getElementById("mode")?.value || "").toUpperCase();
  const isFt8Mode = modeUpper === "DIG" || modeUpper === "USB";
  const cutoffMs = Date.now() - FT8_BAR_WINDOW_MS;
  const messages = ft8MessageHistory.filter((msg) => Number(msg.ts_ms) >= cutoffMs).slice(0, 8);
  if (!isFt8Mode || messages.length === 0) {
    ft8BarOverlay.style.display = "none";
    ft8BarOverlay.innerHTML = "";
    return;
  }

  let html = '<div class="aprs-bar-header"><span class="aprs-bar-title"><span class="aprs-bar-title-word">FT8</span><span class="aprs-bar-title-word">Live</span></span><span class="aprs-bar-clear-wrap"><span class="aprs-bar-clear" role="button" tabindex="0" onclick="window.clearFt8Bar()" onkeydown="if(event.key===\'Enter\'||event.key===\' \'){event.preventDefault();window.clearFt8Bar();}" aria-label="Clear FT8 overlay">Clear</span></span><span class="aprs-bar-window">Last 15 minutes</span></div>';
  for (const msg of messages) {
    const ts = msg.ts_ms ? `<span class="aprs-bar-time">${fmtTime(msg.ts_ms)}</span>` : "";
    const snr = Number.isFinite(msg.snr_db) ? `${msg.snr_db.toFixed(1)} dB` : "-- dB";
    const dt = Number.isFinite(msg.dt_s) ? `dt ${msg.dt_s.toFixed(2)}` : null;
    const rf = ft8BarRfText(msg);
    const detail = [snr, dt, rf].filter(Boolean).join(" · ");
    const text = escapeHtml((msg.message || "").toString());
    html += `<div class="aprs-bar-frame"><div class="aprs-bar-frame-main">${ts}<span class="aprs-bar-call">${text}</span>${detail ? ` · ${detail}` : ""}</div></div>`;
  }
  ft8BarOverlay.innerHTML = html;
  ft8BarOverlay.style.display = "flex";
}
window.updateFt8Bar = updateFt8Bar;
window.clearFt8Bar = function() {
  document.getElementById("ft8-clear-btn")?.click();
};

function renderFt8Message(message) {
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
        out += escapeHtml(token);
      }
      i = j;
    } else {
      out += escapeHtml(ch);
      i += 1;
    }
  }
  return out;
}

function extractAllGrids(message) {
  const out = [];
  const seen = new Set();
  let i = 0;
  while (i < message.length) {
    if (isAlphaNum(message[i])) {
      let j = i + 1;
      while (j < message.length && isAlphaNum(message[j])) j++;
      const token = message.slice(i, j);
      const grid = token.toUpperCase();
      if (isMaidenheadGridToken(grid) && !seen.has(grid)) {
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

function extractLikelyCallsign(message) {
  const tokens = String(message || "")
    .toUpperCase()
    .split(/[^A-Z0-9/]+/)
    .filter(Boolean);
  if (tokens.length === 0) return null;
  const head = tokens[0];
  if (head === "CQ" || head === "DE" || head === "QRZ") {
    if (isLikelyCallsignToken(tokens[1])) return tokens[1];
    for (let i = 1; i < tokens.length; i += 1) {
      if (isLikelyCallsignToken(tokens[i])) return tokens[i];
    }
    return null;
  }
  // Directed messages are usually "<target> <source> ...".
  if (isLikelyCallsignToken(tokens[0]) && isLikelyCallsignToken(tokens[1])) return tokens[1];
  for (const token of tokens) {
    if (isLikelyCallsignToken(token)) return token;
  }
  return null;
}

function isLikelyCallsignToken(token) {
  if (!token) return false;
  if (token.length < 3 || token.length > 12) return false;
  if (token === "CQ" || token === "DE" || token === "QRZ" || token === "DX") return false;
  if (isMaidenheadGridToken(token)) return false;
  return /^[A-Z0-9/]{1,5}\d[A-Z0-9/]{1,6}$/.test(token);
}

function isFtxFarewellToken(token) {
  const normalized = String(token || "").trim().toUpperCase();
  return normalized === "RR73" || normalized === "73" || normalized === "RR";
}

function isMaidenheadGridToken(token) {
  const normalized = String(token || "").trim().toUpperCase();
  return /^[A-R]{2}\d{2}(?:[A-X]{2})?$/.test(normalized) && !isFtxFarewellToken(normalized);
}

function escapeHtml(input) {
  return input
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("\"", "&quot;");
}

function isAlphaNum(ch) {
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
  ft8BufferedWhilePaused = 0;
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

if (ft8PauseBtn) {
  ft8PauseBtn.addEventListener("click", () => {
    ft8Paused = !ft8Paused;
    if (!ft8Paused) {
      ft8BufferedWhilePaused = 0;
      renderFt8History();
    } else {
      updateFt8PauseUi();
    }
  });
}

document.getElementById("ft8-decode-toggle-btn").addEventListener("click", async () => {
  try { await postPath("/toggle_ft8_decode"); } catch (e) { console.error("FT8 toggle failed", e); }
});

document.getElementById("ft8-clear-btn").addEventListener("click", async () => {
  try {
    await postPath("/clear_ft8_decode");
    window.resetFt8HistoryView();
  } catch (e) {
    console.error("FT8 clear failed", e);
  }
});

// --- Server-side FT8 decode handler ---
window.onServerFt8 = function(msg) {
  ft8Status.textContent = ft8Paused ? "Paused" : "Receiving";
  const raw = (msg.message || "").toString();
  const grids = extractAllGrids(raw);
  const station = extractLikelyCallsign(raw);
  const rfHz = normalizeFt8DisplayFreqHz(msg.freq_hz);
  if (grids.length > 0 && window.ft8MapAddLocator) {
    window.ft8MapAddLocator(raw, grids, "ft8", station, {
      ...msg,
      freq_hz: rfHz,
    });
  }
  addFt8Message({
    receiver: window.getDecodeRigMeta ? window.getDecodeRigMeta() : null,
    ts_ms: msg.ts_ms,
    snr_db: msg.snr_db,
    dt_s: msg.dt_s,
    freq_hz: Number.isFinite(rfHz) ? rfHz : msg.freq_hz,
    message: msg.message,
  });
};

updateFt8PauseUi();
