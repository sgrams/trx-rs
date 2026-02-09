// --- FT8 Decoder Plugin (server-side decode) ---
const ft8Status = document.getElementById("ft8-status");
const ft8MessagesEl = document.getElementById("ft8-messages");
const FT8_MAX_MESSAGES = 200;

function fmtTime(tsMs) {
  if (!tsMs) return "--:--:--";
  return new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function renderFt8Row(msg) {
  const row = document.createElement("div");
  row.className = "ft8-row";
  const snr = Number.isFinite(msg.snr_db) ? msg.snr_db.toFixed(1) : "--";
  const dt = Number.isFinite(msg.dt_s) ? msg.dt_s.toFixed(2) : "--";
  const baseHz = Number.isFinite(window.ft8BaseHz) ? window.ft8BaseHz : null;
  const rfHz = Number.isFinite(msg.freq_hz) && Number.isFinite(baseHz) ? (baseHz + msg.freq_hz) : null;
  const freq = Number.isFinite(rfHz) ? rfHz.toFixed(0) : "--";
  const renderedMessage = renderFt8Message(msg.message || "");
  row.innerHTML = `<span class="ft8-time">${fmtTime(msg.ts_ms)}</span><span class="ft8-snr">${snr}</span><span class="ft8-dt">${dt}</span><span class="ft8-freq">${freq}</span><span class="ft8-msg">${renderedMessage}</span>`;
  return row;
}

function addFt8Message(msg) {
  ft8MessagesEl.prepend(renderFt8Row(msg));
  while (ft8MessagesEl.children.length > FT8_MAX_MESSAGES) {
    ft8MessagesEl.removeChild(ft8MessagesEl.lastChild);
  }
}

function renderFt8Message(message) {
  const parts = message.split(/(\\s+)/);
  return parts.map((part) => {
    if (/^\\s+$/.test(part)) return part;
    const m = part.match(/^([^A-Za-z0-9]*)([A-Za-z0-9]+)([^A-Za-z0-9]*)$/);
    if (!m) return escapeHtml(part);
    const [, lead, core, tail] = m;
    const grid = core.toUpperCase();
    if (/^[A-R]{2}\\d{2}(?:[A-X]{2})?$/.test(grid)) {
      return `${escapeHtml(lead)}<span class="ft8-locator">[${grid}]</span>${escapeHtml(tail)}`;
    }
    return escapeHtml(part);
  }).join("");
}

function extractFirstGrid(message) {
  const parts = message.split(/\\s+/);
  for (const part of parts) {
    const m = part.match(/[A-Za-z0-9]+/);
    if (!m) continue;
    const grid = m[0].toUpperCase();
    if (/^[A-R]{2}\\d{2}(?:[A-X]{2})?$/.test(grid)) {
      return grid;
    }
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
  const grid = extractFirstGrid(msg.message || "");
  if (grid && window.ft8MapAddLocator) {
    window.ft8MapAddLocator(msg.message, grid);
  }
  addFt8Message({
    ts_ms: msg.ts_ms,
    snr_db: msg.snr_db,
    dt_s: msg.dt_s,
    freq_hz: msg.freq_hz,
    message: msg.message,
  });
};
