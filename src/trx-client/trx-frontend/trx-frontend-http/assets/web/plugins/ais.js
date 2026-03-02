// --- AIS Decoder Plugin (server-side decode) ---
const aisStatus = document.getElementById("ais-status");
const aisMessagesEl = document.getElementById("ais-messages");
const aisFilterInput = document.getElementById("ais-filter");
const aisClearBtn = document.getElementById("ais-clear-btn");
const aisBarOverlay = document.getElementById("ais-bar-overlay");
const AIS_MAX_MESSAGES = 200;
const AIS_BAR_WINDOW_MS = 15 * 60 * 1000;
let aisFilterText = "";
let aisMessageHistory = [];

function aisDisplayName(msg) {
  return msg.vessel_name || msg.callsign || `MMSI ${msg.mmsi}`;
}

function renderAisRow(msg) {
  const row = document.createElement("div");
  row.className = "ais-message";
  const ts = msg._ts || new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const name = aisDisplayName(msg);
  const channel = msg.channel ? `AIS-${msg.channel}` : "AIS";
  const pos = msg.lat != null && msg.lon != null
    ? ` <a class="aprs-pos" href="javascript:void(0)" onclick="window.navigateToAprsMap(${msg.lat},${msg.lon})">${msg.lat.toFixed(4)}, ${msg.lon.toFixed(4)}</a>`
    : "";
  const motion = [
    msg.sog_knots != null ? `${Number(msg.sog_knots).toFixed(1)} kn` : null,
    msg.cog_deg != null ? `${Number(msg.cog_deg).toFixed(1)}°` : null,
  ].filter(Boolean).join(" · ");
  row.dataset.filterText = [
    name,
    msg.mmsi,
    msg.channel,
    msg.vessel_name,
    msg.callsign,
    msg.destination,
  ]
    .filter(Boolean)
    .join(" ")
    .toUpperCase();
  row.innerHTML =
    `<span class="ais-time">${ts}</span>` +
    `<span class="ais-call">${escapeMapHtml(name)}</span> ` +
    `<span class="aprs-time">[${escapeMapHtml(channel)}]</span> ` +
    `<span>MMSI ${escapeMapHtml(String(msg.mmsi))}</span>` +
    (motion ? ` <span>${escapeMapHtml(motion)}</span>` : "") +
    pos;
  applyAisFilterToRow(row);
  return row;
}

function applyAisFilterToRow(row) {
  if (!aisFilterText) {
    row.style.display = "";
    return;
  }
  const message = row.dataset.filterText || "";
  row.style.display = message.includes(aisFilterText) ? "" : "none";
}

function applyAisFilterToAll() {
  if (!aisMessagesEl) return;
  const rows = aisMessagesEl.querySelectorAll(".ais-message");
  rows.forEach((row) => applyAisFilterToRow(row));
}

function updateAisBar() {
  if (!aisBarOverlay) return;
  const isAis = (document.getElementById("mode")?.value || "").toUpperCase() === "AIS";
  const cutoffMs = Date.now() - AIS_BAR_WINDOW_MS;
  const messages = aisMessageHistory.filter((msg) => msg._tsMs >= cutoffMs);
  if (!isAis || messages.length === 0) {
    aisBarOverlay.style.display = "none";
    aisBarOverlay.innerHTML = "";
    return;
  }

  let html = '<div class="aprs-bar-header"><span class="aprs-bar-title"><span class="aprs-bar-title-word">AIS</span><span class="aprs-bar-title-word">Live</span></span><span class="aprs-bar-clear-wrap"><span class="aprs-bar-clear" role="button" tabindex="0" onclick="window.clearAisBar()" onkeydown="if(event.key===\'Enter\'||event.key===\' \'){event.preventDefault();window.clearAisBar();}" aria-label="Clear AIS overlay">Clear</span></span><span class="aprs-bar-window">Last 15 minutes</span></div>';
  for (const msg of messages) {
    const ts = msg._ts ? `<span class="aprs-bar-time">${msg._ts}</span>` : "";
    const pin = msg.lat != null && msg.lon != null
      ? `<button class="aprs-bar-pin" title="${msg.lat.toFixed(4)}, ${msg.lon.toFixed(4)}" onclick="window.navigateToAprsMap(${msg.lat},${msg.lon})">📍</button>`
      : "";
    const name = `<span class="ais-call">${escapeMapHtml(aisDisplayName(msg))}</span>`;
    const channel = msg.channel ? ` AIS-${escapeMapHtml(msg.channel)}` : "";
    const details = [
      `MMSI ${escapeMapHtml(String(msg.mmsi))}`,
      msg.sog_knots != null ? `${Number(msg.sog_knots).toFixed(1)} kn` : null,
      msg.cog_deg != null ? `${Number(msg.cog_deg).toFixed(1)}°` : null,
    ].filter(Boolean).join(" · ");
    html += `<div class="aprs-bar-frame">` +
      `<div class="aprs-bar-frame-main">${ts}${pin}${name}${channel}: ${details}</div>` +
      `</div>`;
  }
  aisBarOverlay.innerHTML = html;
  aisBarOverlay.style.display = "flex";
}
window.updateAisBar = updateAisBar;
window.clearAisBar = function() {
  window.resetAisHistoryView();
};

window.resetAisHistoryView = function() {
  if (aisMessagesEl) aisMessagesEl.innerHTML = "";
  aisMessageHistory = [];
  updateAisBar();
  if (window.clearMapMarkersByType) window.clearMapMarkersByType("ais");
};

function addAisMessage(msg) {
  const tsMs = Number.isFinite(msg.ts_ms) ? Number(msg.ts_ms) : Date.now();
  msg._tsMs = tsMs;
  msg._ts = new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  aisMessageHistory.unshift(msg);
  if (aisMessageHistory.length > AIS_MAX_MESSAGES) aisMessageHistory.length = AIS_MAX_MESSAGES;
  updateAisBar();

  if (aisMessagesEl) {
    const row = renderAisRow(msg);
    aisMessagesEl.prepend(row);
    while (aisMessagesEl.children.length > AIS_MAX_MESSAGES) {
      aisMessagesEl.removeChild(aisMessagesEl.lastChild);
    }
  }

  if (msg.lat != null && msg.lon != null && window.aisMapAddVessel) {
    window.aisMapAddVessel(msg);
  }
}

if (aisClearBtn) {
  aisClearBtn.addEventListener("click", async () => {
    try {
      await postPath("/clear_ais_decode");
      window.resetAisHistoryView();
    } catch (e) {
      console.error("AIS clear failed", e);
    }
  });
}

if (aisFilterInput) {
  aisFilterInput.addEventListener("input", () => {
    aisFilterText = aisFilterInput.value.trim().toUpperCase();
    applyAisFilterToAll();
  });
}

window.onServerAis = function(msg) {
  if (aisStatus) aisStatus.textContent = "Receiving";
  addAisMessage({
    channel: msg.channel,
    message_type: msg.message_type,
    mmsi: msg.mmsi,
    lat: msg.lat,
    lon: msg.lon,
    sog_knots: msg.sog_knots,
    cog_deg: msg.cog_deg,
    heading_deg: msg.heading_deg,
    vessel_name: msg.vessel_name,
    callsign: msg.callsign,
    destination: msg.destination,
    ts_ms: msg.ts_ms,
  });
};
