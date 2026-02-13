// --- FT8 Decoder Plugin (server-side decode) ---
const ft8Status = document.getElementById("ft8-status");
const ft8PeriodEl = document.getElementById("ft8-period");
const ft8MessagesEl = document.getElementById("ft8-messages");
const ft8FilterInput = document.getElementById("ft8-filter");
const FT8_MAX_MESSAGES = 200;
const FT8_PERIOD_SECONDS = 15;
let ft8FilterText = "";

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
  row.dataset.offsetHz = Number.isFinite(msg.freq_hz) ? String(msg.freq_hz) : "";
  const snr = Number.isFinite(msg.snr_db) ? msg.snr_db.toFixed(1) : "--";
  const dt = Number.isFinite(msg.dt_s) ? msg.dt_s.toFixed(2) : "--";
  const baseHz = Number.isFinite(window.ft8BaseHz) ? window.ft8BaseHz : null;
  const rfHz = Number.isFinite(msg.freq_hz) && Number.isFinite(baseHz) ? (baseHz + msg.freq_hz) : null;
  const freq = Number.isFinite(rfHz) ? rfHz.toFixed(0) : "--";
  const renderedMessage = renderFt8Message(rawMessage);
  row.innerHTML = `<span class="ft8-time">${fmtTime(msg.ts_ms)}</span><span class="ft8-snr">${snr}</span><span class="ft8-dt">${dt}</span><span class="ft8-freq">${freq}</span><span class="ft8-msg">${renderedMessage}</span>`;
  applyFt8FilterToRow(row);
  return row;
}

function addFt8Message(msg) {
  ft8MessagesEl.prepend(renderFt8Row(msg));
  while (ft8MessagesEl.children.length > FT8_MAX_MESSAGES) {
    ft8MessagesEl.removeChild(ft8MessagesEl.lastChild);
  }
}

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
      if (/^[A-R]{2}\d{2}(?:[A-X]{2})?$/.test(grid)) {
        out += `<span class="ft8-locator">${grid}</span>`;
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
      if (/^[A-R]{2}\d{2}(?:[A-X]{2})?$/.test(grid) && !seen.has(grid)) {
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
  const parts = String(message || "").toUpperCase().split(/[^A-Z0-9/]+/);
  for (const token of parts) {
    if (!token) continue;
    if (token.length < 3 || token.length > 12) continue;
    if (token === "CQ" || token === "DE" || token === "QRZ" || token === "DX") continue;
    if (/^[A-R]{2}\d{2}(?:[A-X]{2})?$/.test(token)) continue;
    if (/^[A-Z0-9/]{1,5}\d[A-Z0-9/]{1,6}$/.test(token)) return token;
  }
  return null;
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
  const baseHz = Number.isFinite(window.ft8BaseHz) ? window.ft8BaseHz : null;
  const offset = row.dataset.offsetHz ? Number(row.dataset.offsetHz) : NaN;
  if (Number.isFinite(baseHz) && Number.isFinite(offset)) {
    freqEl.textContent = (baseHz + offset).toFixed(0);
  } else {
    freqEl.textContent = "--";
  }
}

window.updateFt8RfDisplay = function() {
  const rows = ft8MessagesEl.querySelectorAll(".ft8-row");
  rows.forEach((row) => updateFt8RowRf(row));
};

if (ft8FilterInput) {
  ft8FilterInput.addEventListener("input", () => {
    ft8FilterText = ft8FilterInput.value.trim().toUpperCase();
    applyFt8FilterToAll();
  });
}

document.getElementById("ft8-decode-toggle-btn").addEventListener("click", async () => {
  try { await postPath("/toggle_ft8_decode"); } catch (e) { console.error("FT8 toggle failed", e); }
});

document.getElementById("ft8-clear-btn").addEventListener("click", async () => {
  ft8MessagesEl.innerHTML = "";
  try { await postPath("/clear_ft8_decode"); } catch (e) { console.error("FT8 clear failed", e); }
});

// --- Server-side FT8 decode handler ---
window.onServerFt8 = function(msg) {
  ft8Status.textContent = "Receiving";
  const raw = (msg.message || "").toString();
  const grids = extractAllGrids(raw);
  const station = extractLikelyCallsign(raw);
  if (grids.length > 0 && window.ft8MapAddLocator) {
    window.ft8MapAddLocator(raw, grids, "ft8", station);
  }
  addFt8Message({
    ts_ms: msg.ts_ms,
    snr_db: msg.snr_db,
    dt_s: msg.dt_s,
    freq_hz: msg.freq_hz,
    message: msg.message,
  });
};
