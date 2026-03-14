// --- AIS Decoder Plugin (server-side decode) ---
const aisStatus = document.getElementById("ais-status");
const aisMessagesEl = document.getElementById("ais-messages");
const aisFilterInput = document.getElementById("ais-filter");
const aisBarOverlay = document.getElementById("ais-bar-overlay");
const aisChannelSummaryEl = document.getElementById("ais-channel-summary");
const aisVesselCountEl = document.getElementById("ais-vessel-count");
const aisLatestSeenEl = document.getElementById("ais-latest-seen");
const AIS_BAR_WINDOW_MS = 15 * 60 * 1000;
const AIS_DEFAULT_A_HZ = 161_975_000;
const AIS_CHANNEL_SPACING_HZ = 50_000;
let aisFilterText = "";
let aisMessageHistory = [];

function currentAisHistoryRetentionMs() {
  return typeof window.getDecodeHistoryRetentionMs === "function"
    ? window.getDecodeHistoryRetentionMs()
    : 24 * 60 * 60 * 1000;
}

function pruneAisMessageHistory() {
  const cutoffMs = Date.now() - currentAisHistoryRetentionMs();
  aisMessageHistory = aisMessageHistory.filter((msg) => Number(msg?._tsMs) >= cutoffMs);
}

function scheduleAisUi(key, job) {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob(key, job);
    return;
  }
  job();
}

function scheduleAisHistoryRender() {
  scheduleAisUi("ais-history", () => renderAisHistory());
}

function scheduleAisBarUpdate() {
  scheduleAisUi("ais-bar", () => updateAisBar());
}

function formatAisMhz(freqHz) {
  return `${(freqHz / 1_000_000).toFixed(3)} MHz`;
}

function currentAisChannelPlan() {
  const raw = (document.getElementById("freq")?.value || "").replace(/[^\d]/g, "");
  const aHz = raw ? Number(raw) : AIS_DEFAULT_A_HZ;
  const safeAHz = Number.isFinite(aHz) && aHz > 0 ? aHz : AIS_DEFAULT_A_HZ;
  return {
    aHz: safeAHz,
    bHz: safeAHz + AIS_CHANNEL_SPACING_HZ,
  };
}

function aisChannelInfo(channel) {
  const plan = currentAisChannelPlan();
  const ch = String(channel || "").trim().toUpperCase();
  if (ch === "B") {
    return {
      label: "AIS-B",
      badgeClass: "ais-badge ais-badge-channel-b",
      freqText: formatAisMhz(plan.bHz),
    };
  }
  return {
    label: "AIS-A",
    badgeClass: "ais-badge ais-badge-channel-a",
    freqText: formatAisMhz(plan.aHz),
  };
}

function aisDisplayName(msg) {
  return msg.vessel_name || msg.callsign || `MMSI ${msg.mmsi}`;
}

function aisDisplayNameHtml(msg) {
  const label = escapeMapHtml(aisDisplayName(msg));
  const url = window.buildAisVesselUrl ? window.buildAisVesselUrl(msg?.mmsi) : null;
  if (!url) return label;
  return `<a class="title-link" href="${escapeMapHtml(url)}" target="_blank" rel="noopener">${label}</a>`;
}

function aisTypeLabel(type) {
  switch (Number(type)) {
    case 1:
    case 2:
    case 3:
      return "Class A Position";
    case 4:
      return "Base Station";
    case 5:
      return "Static/Voyage";
    case 18:
      return "Class B Position";
    case 19:
      return "Class B Extended";
    case 21:
      return "Aid to Nav";
    case 24:
      return "Class B Static";
    default:
      return `Type ${type ?? "--"}`;
  }
}

function aisAgeText(tsMs) {
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

function aisMotionText(msg) {
  const parts = [
    msg.sog_knots != null ? `${Number(msg.sog_knots).toFixed(1)} kn` : null,
    msg.cog_deg != null ? `${Number(msg.cog_deg).toFixed(1)}° COG` : null,
    msg.heading_deg != null ? `${Number(msg.heading_deg).toFixed(0)}° HDG` : null,
  ].filter(Boolean);
  return parts.join(" · ");
}

function aisRouteText(msg) {
  return [msg.callsign, msg.destination].filter(Boolean).join(" -> ");
}

function aisDistanceText(msg) {
  if (serverLat == null || serverLon == null || msg?.lat == null || msg?.lon == null) {
    return "";
  }
  const distKm = haversineKm(serverLat, serverLon, msg.lat, msg.lon);
  if (!Number.isFinite(distKm)) return "";
  if (distKm < 1) return `${Math.round(distKm * 1000)} m from TRX`;
  return `${distKm.toFixed(1)} km from TRX`;
}

function aisLatestByVessel(messages) {
  const byMmsi = new Map();
  for (const msg of messages) {
    const key = Number.isFinite(msg.mmsi) ? String(msg.mmsi) : `${msg.channel || "?"}:${msg._tsMs || 0}`;
    if (!byMmsi.has(key)) byMmsi.set(key, msg);
  }
  return Array.from(byMmsi.values());
}

function updateAisSummary() {
  const plan = currentAisChannelPlan();
  if (aisChannelSummaryEl) {
    aisChannelSummaryEl.textContent = `A ${formatAisMhz(plan.aHz)} · B ${formatAisMhz(plan.bHz)}`;
  }

  const vessels = aisLatestByVessel(aisMessageHistory);
  if (aisVesselCountEl) {
    const count = vessels.length;
    aisVesselCountEl.textContent = `${count} vessel${count === 1 ? "" : "s"}`;
  }

  if (aisLatestSeenEl) {
    const latest = aisMessageHistory[0];
    if (!latest) {
      aisLatestSeenEl.textContent = "No traffic yet";
    } else {
      const channel = aisChannelInfo(latest.channel);
      aisLatestSeenEl.textContent = `${channel.label} ${aisAgeText(latest._tsMs)}`;
    }
  }
}

function renderAisRow(msg) {
  const row = document.createElement("div");
  row.className = "ais-message";
  const ts = msg._ts || new Date().toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
  const name = aisDisplayName(msg);
  const nameHtml = aisDisplayNameHtml(msg);
  const channel = aisChannelInfo(msg.channel);
  const motion = aisMotionText(msg);
  const route = aisRouteText(msg);
  const distance = aisDistanceText(msg);
  const pos = msg.lat != null && msg.lon != null
    ? `<a class="ais-pos-link" href="javascript:void(0)" onclick="window.navigateToAprsMap(${msg.lat},${msg.lon})">${msg.lat.toFixed(4)}, ${msg.lon.toFixed(4)}</a>`
    : "";
  row.dataset.filterText = [
    name,
    msg.mmsi,
    msg.channel,
    channel.label,
    msg.vessel_name,
    msg.callsign,
    msg.destination,
    aisTypeLabel(msg.message_type),
  ]
    .filter(Boolean)
    .join(" ")
    .toUpperCase();
  row.innerHTML =
    `<div class="ais-row-head">` +
      `<span class="ais-time">${ts}</span>` +
      `<span class="ais-call">${nameHtml}</span>` +
      `<span class="${channel.badgeClass}">${escapeMapHtml(channel.label)}</span>` +
      `<span class="ais-badge ais-badge-type">${escapeMapHtml(aisTypeLabel(msg.message_type))}</span>` +
    `</div>` +
    `<div class="ais-row-meta">` +
      `<span>MMSI ${escapeMapHtml(String(msg.mmsi))}</span>` +
      (route ? `<span class="ais-meta-text">${escapeMapHtml(route)}</span>` : "") +
      `<span class="ais-meta-text">${escapeMapHtml(channel.freqText)}</span>` +
    `</div>` +
    `<div class="ais-row-detail">` +
      (motion ? `<span>${escapeMapHtml(motion)}</span>` : `<span>No motion data</span>`) +
      (distance ? `<span>${escapeMapHtml(distance)}</span>` : "") +
      (pos ? `<span>${pos}</span>` : "") +
      `<span>${escapeMapHtml(aisAgeText(msg._tsMs))}</span>` +
    `</div>`;
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
  updateAisSummary();

  const isAis = (document.getElementById("mode")?.value || "").toUpperCase() === "AIS";
  const cutoffMs = Date.now() - AIS_BAR_WINDOW_MS;
  const recent = aisMessageHistory.filter((msg) => msg._tsMs >= cutoffMs);
  const messages = aisLatestByVessel(recent).slice(0, 8);
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
    const name = `<span class="ais-call">${aisDisplayNameHtml(msg)}</span>`;
    const channel = aisChannelInfo(msg.channel);
    const distance = aisDistanceText(msg);
    const details = [
      `MMSI ${escapeMapHtml(String(msg.mmsi))}`,
      escapeMapHtml(channel.label),
      msg.sog_knots != null ? `${Number(msg.sog_knots).toFixed(1)} kn` : null,
      msg.cog_deg != null ? `${Number(msg.cog_deg).toFixed(1)}°` : null,
      distance ? escapeMapHtml(distance) : null,
      escapeMapHtml(aisAgeText(msg._tsMs)),
    ]
      .filter(Boolean)
      .join(" · ");
    html += `<div class="aprs-bar-frame">` +
      `<div class="aprs-bar-frame-main">${ts}${pin}${name}: ${details}</div>` +
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
  renderAisHistory();
  if (window.clearMapMarkersByType) window.clearMapMarkersByType("ais");
};

function renderAisHistory() {
  pruneAisMessageHistory();
  if (!aisMessagesEl) {
    updateAisSummary();
    return;
  }
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < aisMessageHistory.length; i += 1) {
    fragment.appendChild(renderAisRow(aisMessageHistory[i]));
  }
  aisMessagesEl.replaceChildren(fragment);
  updateAisSummary();
}

function addAisMessage(msg) {
  const tsMs = Number.isFinite(msg.ts_ms) ? Number(msg.ts_ms) : Date.now();
  msg._tsMs = tsMs;
  msg._ts = new Date(tsMs).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });

  aisMessageHistory.unshift(msg);
  pruneAisMessageHistory();
  scheduleAisBarUpdate();
  scheduleAisHistoryRender();

  if (msg.lat != null && msg.lon != null && window.aisMapAddVessel) {
    window.aisMapAddVessel(msg);
  }
}

function normalizeServerAisMessage(msg) {
  return {
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
  };
}

window.onServerAisBatch = function(messages) {
  if (!Array.isArray(messages) || messages.length === 0) return;
  if (aisStatus) aisStatus.textContent = "Receiving";
  const normalized = [];
  for (const msg of messages) {
    const next = normalizeServerAisMessage(msg);
    const tsMs = Number.isFinite(next.ts_ms) ? Number(next.ts_ms) : Date.now();
    next._tsMs = tsMs;
    next._ts = new Date(tsMs).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
    if (next.lat != null && next.lon != null && window.aisMapAddVessel) {
      window.aisMapAddVessel(next);
    }
    normalized.push(next);
  }
  normalized.reverse();
  aisMessageHistory = normalized.concat(aisMessageHistory);
  pruneAisMessageHistory();
  scheduleAisBarUpdate();
  scheduleAisHistoryRender();
};

window.restoreAisHistory = function(messages) {
  window.onServerAisBatch(messages);
};

window.pruneAisHistoryView = function() {
  pruneAisMessageHistory();
  updateAisBar();
  renderAisHistory();
};

document.getElementById("settings-clear-ais-history")?.addEventListener("click", async () => {
  try {
    await postPath("/clear_ais_decode");
    window.resetAisHistoryView();
  } catch (e) {
    console.error("AIS history clear failed", e);
  }
});

if (aisFilterInput) {
  aisFilterInput.addEventListener("input", () => {
    aisFilterText = aisFilterInput.value.trim().toUpperCase();
    renderAisHistory();
  });
}

window.onServerAis = function(msg) {
  if (aisStatus) aisStatus.textContent = "Receiving";
  addAisMessage(normalizeServerAisMessage(msg));
};

updateAisSummary();
