// --- WSPR Decoder Plugin (server-side decode) ---
const wsprStatus = document.getElementById("wspr-status");
const wsprPeriodEl = document.getElementById("wspr-period");
const wsprMessagesEl = document.getElementById("wspr-messages");
const wsprFilterInput = document.getElementById("wspr-filter");
const WSPR_MAX_MESSAGES = 200;
const WSPR_PERIOD_SECONDS = 120;
let wsprFilterText = "";

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
  const snr = Number.isFinite(msg.snr_db) ? msg.snr_db.toFixed(1) : "--";
  const dt = Number.isFinite(msg.dt_s) ? msg.dt_s.toFixed(2) : "--";
  const baseHz = Number.isFinite(window.ft8BaseHz) ? window.ft8BaseHz : null;
  const rfHz = Number.isFinite(msg.freq_hz) && Number.isFinite(baseHz) ? (baseHz + msg.freq_hz) : null;
  const freq = Number.isFinite(rfHz) ? rfHz.toFixed(0) : "--";
  const message = (msg.message || "").toString();
  row.dataset.message = message.toUpperCase();
  row.innerHTML = `<span class="ft8-time">${fmtWsprTime(msg.ts_ms)}</span><span class="ft8-snr">${snr}</span><span class="ft8-dt">${dt}</span><span class="ft8-freq">${freq}</span><span class="ft8-msg">${escapeWsprHtml(message)}</span>`;
  applyWsprFilterToRow(row);
  return row;
}

function addWsprMessage(msg) {
  wsprMessagesEl.prepend(renderWsprRow(msg));
  while (wsprMessagesEl.children.length > WSPR_MAX_MESSAGES) {
    wsprMessagesEl.removeChild(wsprMessagesEl.lastChild);
  }
}

function escapeWsprHtml(input) {
  return input
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("\"", "&quot;");
}

function extractAllGrids(message) {
  const out = [];
  const seen = new Set();
  const parts = message.toUpperCase().split(/[^A-Z0-9]+/);
  for (const token of parts) {
    if (!token) continue;
    if (/^[A-R]{2}\d{2}(?:[A-X]{2})?$/.test(token) && !seen.has(token)) {
      seen.add(token);
      out.push(token);
    }
  }
  return out;
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

if (wsprFilterInput) {
  wsprFilterInput.addEventListener("input", () => {
    wsprFilterText = wsprFilterInput.value.trim().toUpperCase();
    applyWsprFilterToAll();
  });
}

document.getElementById("wspr-decode-toggle-btn").addEventListener("click", async () => {
  try { await postPath("/toggle_wspr_decode"); } catch (e) { console.error("WSPR toggle failed", e); }
});

document.getElementById("wspr-clear-btn").addEventListener("click", async () => {
  wsprMessagesEl.innerHTML = "";
  try { await postPath("/clear_wspr_decode"); } catch (e) { console.error("WSPR clear failed", e); }
});

window.onServerWspr = function(msg) {
  wsprStatus.textContent = "Receiving";
  const raw = (msg.message || "").toString();
  const grids = extractAllGrids(raw);
  if (grids.length > 0 && window.ft8MapAddLocator) {
    window.ft8MapAddLocator(raw, grids, "wspr");
  }
  addWsprMessage({
    ts_ms: msg.ts_ms,
    snr_db: msg.snr_db,
    dt_s: msg.dt_s,
    freq_hz: msg.freq_hz,
    message: raw,
  });
};
