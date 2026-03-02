// --- VDES Decoder Plugin (server-side decode) ---
const vdesStatus = document.getElementById("vdes-status");
const vdesMessagesEl = document.getElementById("vdes-messages");
const vdesFilterInput = document.getElementById("vdes-filter");
const vdesClearBtn = document.getElementById("vdes-clear-btn");
const vdesBarOverlay = document.getElementById("vdes-bar-overlay");
const vdesChannelSummaryEl = document.getElementById("vdes-channel-summary");
const vdesFrameCountEl = document.getElementById("vdes-frame-count");
const vdesLatestSeenEl = document.getElementById("vdes-latest-seen");
const VDES_MAX_MESSAGES = 200;
const VDES_BAR_WINDOW_MS = 15 * 60 * 1000;
let vdesFilterText = "";
let vdesMessageHistory = [];

function currentVdesCenterText() {
  const raw = (document.getElementById("freq")?.value || "").replace(/[^\d]/g, "");
  const hz = raw ? Number(raw) : 0;
  if (!Number.isFinite(hz) || hz <= 0) return "100 kHz centered on tuned frequency";
  return `100 kHz @ ${(hz / 1_000_000).toFixed(3)} MHz`;
}

function vdesAgeText(tsMs) {
  if (!Number.isFinite(tsMs)) return "just now";
  const deltaMs = Math.max(0, Date.now() - tsMs);
  const seconds = Math.round(deltaMs / 1000);
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  return `${hours}h ago`;
}

function vdesHexPreview(rawBytes) {
  if (!Array.isArray(rawBytes) || rawBytes.length === 0) return "--";
  return rawBytes
    .slice(0, 20)
    .map((value) => Number(value).toString(16).padStart(2, "0"))
    .join(" ")
    .toUpperCase();
}

function updateVdesSummary() {
  if (vdesChannelSummaryEl) {
    vdesChannelSummaryEl.textContent = currentVdesCenterText();
  }
  if (vdesFrameCountEl) {
    const count = vdesMessageHistory.length;
    vdesFrameCountEl.textContent = `${count} burst${count === 1 ? "" : "s"}`;
  }
  if (vdesLatestSeenEl) {
    const latest = vdesMessageHistory[0];
    vdesLatestSeenEl.textContent = latest ? vdesAgeText(latest._tsMs) : "No traffic yet";
  }
}

function applyVdesFilterToRow(row) {
  if (!vdesFilterText) {
    row.style.display = "";
    return;
  }
  const text = row.dataset.filterText || "";
  row.style.display = text.includes(vdesFilterText) ? "" : "none";
}

function applyVdesFilterToAll() {
  if (!vdesMessagesEl) return;
  vdesMessagesEl.querySelectorAll(".vdes-message").forEach((row) => applyVdesFilterToRow(row));
}

function renderVdesRow(msg) {
  const row = document.createElement("div");
  row.className = "vdes-message";
  const ts = msg._ts || new Date().toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
  const title = msg.vessel_name || "VDES Burst";
  const label = msg.callsign || "VDES";
  const info = msg.destination || "";
  const linkText = Number.isFinite(msg.link_id) ? `LID ${msg.link_id}` : "";
  const syncText = Number.isFinite(msg.sync_score) ? `Sync ${(Number(msg.sync_score) * 100).toFixed(0)}%` : "";
  const phaseText = Number.isFinite(msg.phase_rotation) ? `R${Number(msg.phase_rotation)}` : "";
  const fecText = msg.fec_state || "";
  const rawHex = vdesHexPreview(msg.raw_bytes);
  row.dataset.filterText = [
    title,
    label,
    info,
    linkText,
    syncText,
    phaseText,
    fecText,
    rawHex,
    msg.message_type,
    msg.bit_len,
  ]
    .filter(Boolean)
    .join(" ")
    .toUpperCase();
  row.innerHTML =
    `<div class="vdes-row-head">` +
      `<span class="vdes-time">${ts}</span>` +
      `<span class="vdes-call">${escapeMapHtml(title)}</span>` +
      `<span class="vdes-badge">${escapeMapHtml(label)}</span>` +
      (linkText ? `<span class="vdes-badge">${escapeMapHtml(linkText)}</span>` : "") +
      (syncText ? `<span class="vdes-badge">${escapeMapHtml(syncText)}</span>` : "") +
      (phaseText ? `<span class="vdes-badge">${escapeMapHtml(phaseText)}</span>` : "") +
      `<span class="vdes-badge">T${escapeMapHtml(String(msg.message_type ?? "--"))}</span>` +
    `</div>` +
    `<div class="vdes-row-meta">` +
      `<span>${escapeMapHtml(currentVdesCenterText())}</span>` +
      `<span>${escapeMapHtml(`${msg.bit_len || 0} bits`)}</span>` +
      (info ? `<span>${escapeMapHtml(info)}</span>` : "") +
      (fecText ? `<span>${escapeMapHtml(fecText)}</span>` : "") +
      `<span>${escapeMapHtml(vdesAgeText(msg._tsMs))}</span>` +
    `</div>` +
    `<div class="vdes-row-detail">` +
      `<span class="vdes-raw">${escapeMapHtml(rawHex)}</span>` +
    `</div>`;
  applyVdesFilterToRow(row);
  return row;
}

function updateVdesBar() {
  if (!vdesBarOverlay) return;
  updateVdesSummary();
  const isVdes = (document.getElementById("mode")?.value || "").toUpperCase() === "VDES";
  const cutoffMs = Date.now() - VDES_BAR_WINDOW_MS;
  const messages = vdesMessageHistory.filter((msg) => msg._tsMs >= cutoffMs).slice(0, 6);
  if (!isVdes || messages.length === 0) {
    vdesBarOverlay.style.display = "none";
    vdesBarOverlay.innerHTML = "";
    return;
  }

  let html = '<div class="aprs-bar-header"><span class="aprs-bar-title"><span class="aprs-bar-title-word">VDES</span><span class="aprs-bar-title-word">Live</span></span><span class="aprs-bar-clear-wrap"><span class="aprs-bar-clear" role="button" tabindex="0" onclick="window.clearVdesBar()" onkeydown="if(event.key===\'Enter\'||event.key===\' \'){event.preventDefault();window.clearVdesBar();}" aria-label="Clear VDES overlay">Clear</span></span><span class="aprs-bar-window">Last 15 minutes</span></div>';
  for (const msg of messages) {
    const ts = msg._ts ? `<span class="aprs-bar-time">${msg._ts}</span>` : "";
    const label = escapeMapHtml(msg.callsign || "VDES");
    const title = escapeMapHtml(msg.vessel_name || "Burst");
    const detail = [
      `${msg.bit_len || 0} bits`,
      Number.isFinite(msg.link_id) ? `LID ${Number(msg.link_id)}` : null,
      Number.isFinite(msg.sync_score) ? `sync ${(Number(msg.sync_score) * 100).toFixed(0)}%` : null,
      Number.isFinite(msg.phase_rotation) ? `rot ${Number(msg.phase_rotation)}` : null,
      msg.destination ? escapeMapHtml(msg.destination) : null,
      escapeMapHtml(vdesAgeText(msg._tsMs)),
    ]
      .filter(Boolean)
      .join(" · ");
    html += `<div class="aprs-bar-frame"><div class="aprs-bar-frame-main">${ts}<span class="vdes-call">${title}</span> <span class="vdes-badge">${label}</span>: ${detail}</div></div>`;
  }
  vdesBarOverlay.innerHTML = html;
  vdesBarOverlay.style.display = "flex";
}
window.updateVdesBar = updateVdesBar;
window.clearVdesBar = function() {
  document.getElementById("vdes-clear-btn")?.click();
};

window.resetVdesHistoryView = function() {
  if (vdesMessagesEl) vdesMessagesEl.innerHTML = "";
  vdesMessageHistory = [];
  updateVdesBar();
};

function addVdesMessage(msg) {
  const tsMs = Number.isFinite(msg.ts_ms) ? Number(msg.ts_ms) : Date.now();
  msg._tsMs = tsMs;
  msg._ts = new Date(tsMs).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });

  vdesMessageHistory.unshift(msg);
  if (vdesMessageHistory.length > VDES_MAX_MESSAGES) vdesMessageHistory.length = VDES_MAX_MESSAGES;
  updateVdesBar();

  if (vdesMessagesEl) {
    const row = renderVdesRow(msg);
    vdesMessagesEl.prepend(row);
    while (vdesMessagesEl.children.length > VDES_MAX_MESSAGES) {
      vdesMessagesEl.removeChild(vdesMessagesEl.lastChild);
    }
  }
}

if (vdesClearBtn) {
  vdesClearBtn.addEventListener("click", async () => {
    try {
      await postPath("/clear_vdes_decode");
      window.resetVdesHistoryView();
    } catch (e) {
      console.error("VDES clear failed", e);
    }
  });
}

if (vdesFilterInput) {
  vdesFilterInput.addEventListener("input", () => {
    vdesFilterText = vdesFilterInput.value.trim().toUpperCase();
    applyVdesFilterToAll();
  });
}

window.onServerVdes = function(msg) {
  if (vdesStatus) vdesStatus.textContent = "Receiving";
  addVdesMessage({
    message_type: msg.message_type,
    bit_len: msg.bit_len,
    raw_bytes: msg.raw_bytes,
    vessel_name: msg.vessel_name,
    callsign: msg.callsign,
    destination: msg.destination,
    link_id: msg.link_id,
    sync_score: msg.sync_score,
    sync_errors: msg.sync_errors,
    phase_rotation: msg.phase_rotation,
    fec_state: msg.fec_state,
    ts_ms: msg.ts_ms,
  });
};

updateVdesSummary();
