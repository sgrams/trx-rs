// --- WSPR Decoder Plugin (server-side decode) ---
const wsprStatus = document.getElementById("wspr-status");
const wsprMessagesEl = document.getElementById("wspr-messages");
const WSPR_MAX_MESSAGES = 200;

function fmtWsprTime(tsMs) {
  if (!tsMs) return "--:--:--";
  return new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function renderWsprRow(msg) {
  const row = document.createElement("div");
  row.className = "ft8-row";
  const snr = Number.isFinite(msg.snr_db) ? msg.snr_db.toFixed(1) : "--";
  const dt = Number.isFinite(msg.dt_s) ? msg.dt_s.toFixed(2) : "--";
  const baseHz = Number.isFinite(window.ft8BaseHz) ? window.ft8BaseHz : null;
  const rfHz = Number.isFinite(msg.freq_hz) && Number.isFinite(baseHz) ? (baseHz + msg.freq_hz) : null;
  const freq = Number.isFinite(rfHz) ? rfHz.toFixed(0) : "--";
  const message = (msg.message || "").toString();
  row.innerHTML = `<span class="ft8-time">${fmtWsprTime(msg.ts_ms)}</span><span class="ft8-snr">${snr}</span><span class="ft8-dt">${dt}</span><span class="ft8-freq">${freq}</span><span class="ft8-msg">${escapeWsprHtml(message)}</span>`;
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

document.getElementById("wspr-decode-toggle-btn").addEventListener("click", async () => {
  try { await postPath("/toggle_wspr_decode"); } catch (e) { console.error("WSPR toggle failed", e); }
});

document.getElementById("wspr-clear-btn").addEventListener("click", async () => {
  wsprMessagesEl.innerHTML = "";
  try { await postPath("/clear_wspr_decode"); } catch (e) { console.error("WSPR clear failed", e); }
});

window.onServerWspr = function(msg) {
  wsprStatus.textContent = "Receiving";
  addWsprMessage({
    ts_ms: msg.ts_ms,
    snr_db: msg.snr_db,
    dt_s: msg.dt_s,
    freq_hz: msg.freq_hz,
    message: msg.message,
  });
};
