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
  const freq = Number.isFinite(msg.freq_hz) ? msg.freq_hz.toFixed(0) : "--";
  row.innerHTML = `<span class="ft8-time">${fmtTime(msg.ts_ms)}</span><span class="ft8-snr">${snr}</span><span class="ft8-dt">${dt}</span><span class="ft8-freq">${freq}</span><span class="ft8-msg">${msg.message || ""}</span>`;
  return row;
}

function addFt8Message(msg) {
  ft8MessagesEl.prepend(renderFt8Row(msg));
  while (ft8MessagesEl.children.length > FT8_MAX_MESSAGES) {
    ft8MessagesEl.removeChild(ft8MessagesEl.lastChild);
  }
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
  addFt8Message({
    ts_ms: msg.ts_ms,
    snr_db: msg.snr_db,
    dt_s: msg.dt_s,
    freq_hz: msg.freq_hz,
    message: msg.message,
  });
};
