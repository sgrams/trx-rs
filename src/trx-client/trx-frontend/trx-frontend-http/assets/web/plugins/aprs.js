// --- APRS Decoder Plugin (server-side decode) ---
const aprsStatus = document.getElementById("aprs-status");
const aprsPacketsEl = document.getElementById("aprs-packets");
const aprsFilterInput = document.getElementById("aprs-filter");
const aprsBarOverlay = document.getElementById("aprs-bar-overlay");
const aprsPauseBtn = document.getElementById("aprs-pause-btn");
const aprsOnlyPosBtn = document.getElementById("aprs-only-pos-btn");
const aprsHideCrcBtn = document.getElementById("aprs-hide-crc-btn");
const aprsCollapseDupBtn = document.getElementById("aprs-collapse-dup-btn");
const aprsTotalCountEl = document.getElementById("aprs-total-count");
const aprsVisibleCountEl = document.getElementById("aprs-visible-count");
const aprsLatestSeenEl = document.getElementById("aprs-latest-seen");
const APRS_MAX_PACKETS = 100;
const APRS_BAR_WINDOW_MS = 15 * 60 * 1000;
let aprsFilterText = "";
let aprsPacketHistory = [];
let aprsPaused = false;
let aprsBufferedWhilePaused = 0;
let aprsOnlyPos = false;
let aprsHideCrc = false;
let aprsCollapseDup = false;
let aprsTypeFilter = "all";

function renderAprsInfo(pkt) {
  const bytes = Array.isArray(pkt.info_bytes) ? pkt.info_bytes : null;
  if (bytes && bytes.length > 0) {
    let out = "";
    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i];
      if (b >= 0x20 && b <= 0x7e) {
        const ch = String.fromCharCode(b);
        if (ch === "<") out += "&lt;";
        else if (ch === ">") out += "&gt;";
        else if (ch === "&") out += "&amp;";
        else if (ch === '"') out += "&quot;";
        else out += ch;
      } else {
        const hex = b.toString(16).toUpperCase().padStart(2, "0");
        out += `<span class="aprs-byte">0x${hex}</span>`;
      }
    }
    return out;
  }
  const str = pkt.info || "";
  let out = "";
  for (let i = 0; i < str.length; i++) {
    const code = str.charCodeAt(i);
    if (code >= 0x20 && code <= 0x7e) {
      const ch = str[i];
      if (ch === "<") out += "&lt;";
      else if (ch === ">") out += "&gt;";
      else if (ch === "&") out += "&amp;";
      else if (ch === '"') out += "&quot;";
      else out += ch;
    } else {
      const hex = code.toString(16).toUpperCase().padStart(2, "0");
      out += `<span class="aprs-byte">0x${hex}</span>`;
    }
  }
  return out;
}

function aprsPacketCategory(pkt) {
  const type = String(pkt.type || "").toLowerCase();
  const info = String(pkt.info || "").toLowerCase();
  if (pkt.lat != null && pkt.lon != null || type.includes("position")) return "position";
  if (type.includes("message") || info.startsWith(":")) return "message";
  if (type.includes("weather") || info.startsWith("_")) return "weather";
  if (type.includes("telemetry") || info.startsWith("t#")) return "telemetry";
  return "other";
}

function aprsCategoryLabel(category) {
  switch (category) {
    case "position": return "Position";
    case "message": return "Message";
    case "weather": return "Weather";
    case "telemetry": return "Telemetry";
    default: return "Other";
  }
}

function aprsAgeText(tsMs) {
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

function aprsDistanceText(pkt) {
  if (serverLat == null || serverLon == null || pkt.lat == null || pkt.lon == null) return "";
  const distKm = haversineKm(serverLat, serverLon, pkt.lat, pkt.lon);
  if (!Number.isFinite(distKm)) return "";
  if (distKm < 1) return `${Math.round(distKm * 1000)} m from TRX`;
  return `${distKm.toFixed(1)} km from TRX`;
}

function aprsPacketSignature(pkt) {
  return [
    pkt.srcCall || "",
    pkt.destCall || "",
    pkt.path || "",
    pkt.info || "",
    pkt.type || "",
    pkt.lat != null ? pkt.lat.toFixed(4) : "",
    pkt.lon != null ? pkt.lon.toFixed(4) : "",
  ].join("|");
}

function aprsHexBytes(bytes) {
  if (!Array.isArray(bytes) || bytes.length === 0) return "--";
  return bytes.map((b) => Number(b).toString(16).toUpperCase().padStart(2, "0")).join(" ");
}

function aprsFilterMatch(pkt) {
  if (aprsOnlyPos && (pkt.lat == null || pkt.lon == null)) return false;
  if (aprsHideCrc && !pkt.crcOk) return false;
  if (aprsTypeFilter !== "all" && aprsPacketCategory(pkt) !== aprsTypeFilter) return false;
  if (!aprsFilterText) return true;
  const haystack = [
    pkt.srcCall,
    pkt.destCall,
    pkt.path,
    pkt.info,
    pkt.type,
    pkt.lat != null ? pkt.lat.toFixed(4) : "",
    pkt.lon != null ? pkt.lon.toFixed(4) : "",
    aprsPacketCategory(pkt),
  ]
    .filter(Boolean)
    .join(" ")
    .toUpperCase();
  return haystack.includes(aprsFilterText);
}

function aprsVisiblePackets() {
  const packets = aprsCollapseDup ? collapseAprsDuplicates(aprsPacketHistory) : aprsPacketHistory;
  return packets.filter(aprsFilterMatch);
}

function collapseAprsDuplicates(packets) {
  const seen = new Set();
  const out = [];
  for (const pkt of packets) {
    const key = aprsPacketSignature(pkt);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(pkt);
  }
  return out;
}

function updateAprsSummary() {
  const visible = aprsVisiblePackets();
  if (aprsTotalCountEl) {
    aprsTotalCountEl.textContent = `${aprsPacketHistory.length} total`;
  }
  if (aprsVisibleCountEl) {
    let text = `${visible.length} shown`;
    if (aprsPaused && aprsBufferedWhilePaused > 0) {
      text += ` · ${aprsBufferedWhilePaused} buffered`;
    }
    aprsVisibleCountEl.textContent = text;
  }
  if (aprsLatestSeenEl) {
    const latest = aprsPacketHistory[0];
    if (!latest) {
      aprsLatestSeenEl.textContent = "No packets yet";
    } else {
      aprsLatestSeenEl.textContent = `${latest.srcCall} ${aprsAgeText(latest._tsMs)}`;
    }
  }
}

function updateAprsChipState() {
  document.querySelectorAll("[id^='aprs-type-']").forEach((btn) => {
    btn.classList.toggle("active", btn.id === `aprs-type-${aprsTypeFilter}`);
  });
  aprsOnlyPosBtn?.classList.toggle("active", aprsOnlyPos);
  aprsHideCrcBtn?.classList.toggle("active", aprsHideCrc);
  aprsCollapseDupBtn?.classList.toggle("active", aprsCollapseDup);
  if (aprsPauseBtn) {
    aprsPauseBtn.textContent = aprsPaused ? "Resume" : "Pause";
    aprsPauseBtn.classList.toggle("active", aprsPaused);
  }
}

function renderAprsRow(pkt, isFresh) {
  const row = document.createElement("div");
  row.className = "aprs-packet";
  if (!pkt.crcOk) row.classList.add("aprs-packet-crc");
  if (isFresh) row.classList.add("aprs-packet-new");

  const ts = pkt._ts || new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const age = aprsAgeText(pkt._tsMs);
  const category = aprsPacketCategory(pkt);
  const categoryLabel = aprsCategoryLabel(category);
  const categoryClass = `aprs-badge aprs-badge-type aprs-badge-type-${category}`;
  const pathBadge = pkt.path ? `<span class="aprs-badge">${escapeMapHtml(pkt.path)}</span>` : "";
  const crcBadge = pkt.crcOk ? "" : '<span class="aprs-badge aprs-badge-crc">CRC Fail</span>';
  let symbolHtml = "";
  if (pkt.symbolTable && pkt.symbolCode) {
    const sheet = pkt.symbolTable === "/" ? 0 : 1;
    const code = pkt.symbolCode.charCodeAt(0) - 33;
    const col = code % 16;
    const row2 = Math.floor(code / 16);
    const bgX = -(col * 24);
    const bgY = -(row2 * 24);
    symbolHtml = `<span class="aprs-symbol" style="background-image:url('https://raw.githubusercontent.com/hessu/aprs-symbols/master/png/aprs-symbols-24-${sheet}.png');background-position:${bgX}px ${bgY}px"></span>`;
  }
  const posLink = pkt.lat != null && pkt.lon != null
    ? `<a class="aprs-pos" href="javascript:void(0)" data-aprs-map="${pkt.lat},${pkt.lon}">${pkt.lat.toFixed(4)}, ${pkt.lon.toFixed(4)}</a>`
    : "";
  const distance = aprsDistanceText(pkt);
  const qrzHref = `https://qrzcq.com/call/${encodeURIComponent(pkt.srcCall || "")}`;

  row.innerHTML =
    `<div class="aprs-row-head">` +
      `<span class="aprs-time">${ts}</span>` +
      symbolHtml +
      `<span class="aprs-call">${escapeMapHtml(pkt.srcCall)}</span>` +
      `<span>&gt;${escapeMapHtml(pkt.destCall || "")}</span>` +
      `<span class="${categoryClass}">${escapeMapHtml(categoryLabel)}</span>` +
      pathBadge +
      crcBadge +
    `</div>` +
    `<div class="aprs-row-meta">` +
      `<span class="aprs-meta-text">${escapeMapHtml(age)}</span>` +
      (distance ? `<span class="aprs-meta-text">${escapeMapHtml(distance)}</span>` : "") +
      `<span class="aprs-meta-text">${escapeMapHtml(pkt.type || "--")}</span>` +
    `</div>` +
    `<div class="aprs-row-detail">` +
      `<span title="${escapeMapHtml(pkt.type || "")}">${renderAprsInfo(pkt)}</span>` +
      (posLink ? `<span>${posLink}</span>` : "") +
    `</div>` +
    `<div class="aprs-row-actions">` +
      (pkt.lat != null && pkt.lon != null ? `<button class="aprs-inline-btn" type="button" data-aprs-map="${pkt.lat},${pkt.lon}">Map</button>` : "") +
      (pkt.lat != null && pkt.lon != null ? `<button class="aprs-inline-btn" type="button" data-aprs-copy="${pkt.lat},${pkt.lon}">Copy Coords</button>` : "") +
      `<a class="aprs-inline-btn" href="${qrzHref}" target="_blank" rel="noopener">QRZ</a>` +
    `</div>` +
    `<details class="aprs-details">` +
      `<summary>Details</summary>` +
      `<div class="aprs-details-grid">` +
        `<span class="aprs-detail-label">Source</span><span class="aprs-detail-value">${escapeMapHtml(pkt.srcCall || "--")}</span>` +
        `<span class="aprs-detail-label">Destination</span><span class="aprs-detail-value">${escapeMapHtml(pkt.destCall || "--")}</span>` +
        `<span class="aprs-detail-label">Type</span><span class="aprs-detail-value">${escapeMapHtml(pkt.type || "--")}</span>` +
        `<span class="aprs-detail-label">Path</span><span class="aprs-detail-value">${escapeMapHtml(pkt.path || "--")}</span>` +
        `<span class="aprs-detail-label">Age</span><span class="aprs-detail-value">${escapeMapHtml(age)}</span>` +
        `<span class="aprs-detail-label">CRC</span><span class="aprs-detail-value">${pkt.crcOk ? "OK" : "Failed"}</span>` +
        `<span class="aprs-detail-label">Position</span><span class="aprs-detail-value">${pkt.lat != null && pkt.lon != null ? `${pkt.lat.toFixed(5)}, ${pkt.lon.toFixed(5)}` : "--"}</span>` +
        `<span class="aprs-detail-label">Info</span><span class="aprs-detail-value">${escapeMapHtml(pkt.info || "--")}</span>` +
        `<span class="aprs-detail-label">Info Bytes</span><span class="aprs-detail-value">${escapeMapHtml(aprsHexBytes(pkt.info_bytes))}</span>` +
      `</div>` +
    `</details>`;

  row.querySelectorAll("[data-aprs-map]").forEach((el) => {
    el.addEventListener("click", (evt) => {
      evt.preventDefault();
      const raw = String(el.dataset.aprsMap || "");
      const [lat, lon] = raw.split(",").map(Number);
      if (window.navigateToAprsMap && Number.isFinite(lat) && Number.isFinite(lon)) {
        window.navigateToAprsMap(lat, lon);
      }
    });
  });

  const copyBtn = row.querySelector("[data-aprs-copy]");
  if (copyBtn) {
    copyBtn.addEventListener("click", async () => {
      const raw = String(copyBtn.dataset.aprsCopy || "");
      try {
        if (navigator.clipboard?.writeText) {
          await navigator.clipboard.writeText(raw);
          showHint("Coordinates copied", 1200);
        }
      } catch (_e) {
        showHint("Copy failed", 1500);
      }
    });
  }

  return row;
}

function renderAprsHistory() {
  if (!aprsPacketsEl || aprsPaused) {
    updateAprsSummary();
    updateAprsChipState();
    return;
  }
  const visible = aprsVisiblePackets();
  aprsPacketsEl.innerHTML = "";
  for (let i = 0; i < visible.length; i++) {
    aprsPacketsEl.appendChild(renderAprsRow(visible[i], i === 0));
  }
  updateAprsSummary();
  updateAprsChipState();
}

function updateAprsBar() {
  if (!aprsBarOverlay) return;
  const isPkt = (document.getElementById("mode")?.value || "").toUpperCase() === "PKT";
  const cutoffMs = Date.now() - APRS_BAR_WINDOW_MS;
  const okFrames = aprsPacketHistory.filter((p) => p.crcOk && p._tsMs >= cutoffMs);
  const frames = collapseAprsDuplicates(okFrames).slice(0, 8);
  if (!isPkt || frames.length === 0) {
    aprsBarOverlay.style.display = "none";
    return;
  }
  let html = '<div class="aprs-bar-header"><span class="aprs-bar-title"><span class="aprs-bar-title-word">APRS</span><span class="aprs-bar-title-word">Live</span></span><span class="aprs-bar-clear-wrap"><span class="aprs-bar-clear" role="button" tabindex="0" onclick="window.clearAprsBar()" onkeydown="if(event.key===\'Enter\'||event.key===\' \'){event.preventDefault();window.clearAprsBar();}" aria-label="Clear APRS overlay">Clear</span></span><span class="aprs-bar-window">Last 15 minutes</span></div>';
  for (const pkt of frames) {
    const ts = pkt._ts ? `<span class="aprs-bar-time">${pkt._ts}</span>` : "";
    const call = `<span class="aprs-bar-call">${escapeMapHtml(pkt.srcCall)}</span>`;
    const dest = escapeMapHtml(pkt.destCall || "");
    const info = escapeMapHtml(pkt.info || "");
    const pin = pkt.lat != null && pkt.lon != null
      ? `<button class="aprs-bar-pin" title="${pkt.lat.toFixed(4)}, ${pkt.lon.toFixed(4)}" onclick="window.navigateToAprsMap(${pkt.lat},${pkt.lon})">📍</button>`
      : "";
    html += `<div class="aprs-bar-frame">` +
      `<div class="aprs-bar-frame-main">${ts}${pin}${call}>${dest}: ${info}</div>` +
      `</div>`;
  }
  aprsBarOverlay.innerHTML = html;
  aprsBarOverlay.style.display = "flex";
}
window.updateAprsBar = updateAprsBar;
window.clearAprsBar = function() {
  document.getElementById("aprs-clear-btn")?.click();
};

window.resetAprsHistoryView = function() {
  if (aprsPacketsEl) aprsPacketsEl.innerHTML = "";
  aprsPacketHistory = [];
  aprsBufferedWhilePaused = 0;
  updateAprsBar();
  renderAprsHistory();
  if (window.clearMapMarkersByType) window.clearMapMarkersByType("aprs");
};

function addAprsPacket(pkt) {
  const tag = pkt.crcOk ? "[APRS]" : "[APRS-CRC-FAIL]";
  console.log(tag, `${pkt.srcCall}>${pkt.destCall}${pkt.path ? "," + pkt.path : ""}: ${pkt.info}`, pkt);

  const tsMs = Number.isFinite(pkt.ts_ms) ? Number(pkt.ts_ms) : Date.now();
  pkt._tsMs = tsMs;
  pkt._ts = new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  aprsPacketHistory.unshift(pkt);
  if (aprsPacketHistory.length > APRS_MAX_PACKETS) aprsPacketHistory.length = APRS_MAX_PACKETS;

  if (pkt.lat != null && pkt.lon != null && window.aprsMapAddStation) {
    window.aprsMapAddStation(pkt.srcCall, pkt.lat, pkt.lon, pkt.info, pkt.symbolTable, pkt.symbolCode, pkt);
  }

  if (pkt.crcOk) updateAprsBar();

  if (aprsPaused) {
    aprsBufferedWhilePaused += 1;
    updateAprsSummary();
    updateAprsChipState();
    return;
  }

  renderAprsHistory();
}

document.getElementById("aprs-clear-btn").addEventListener("click", async () => {
  try {
    await postPath("/clear_aprs_decode");
    window.resetAprsHistoryView();
  } catch (e) {
    console.error("APRS clear failed", e);
  }
});

if (aprsPauseBtn) {
  aprsPauseBtn.addEventListener("click", () => {
    aprsPaused = !aprsPaused;
    if (!aprsPaused) {
      aprsBufferedWhilePaused = 0;
      renderAprsHistory();
    } else {
      updateAprsSummary();
      updateAprsChipState();
    }
  });
}

if (aprsOnlyPosBtn) {
  aprsOnlyPosBtn.addEventListener("click", () => {
    aprsOnlyPos = !aprsOnlyPos;
    renderAprsHistory();
  });
}

if (aprsHideCrcBtn) {
  aprsHideCrcBtn.addEventListener("click", () => {
    aprsHideCrc = !aprsHideCrc;
    renderAprsHistory();
  });
}

if (aprsCollapseDupBtn) {
  aprsCollapseDupBtn.addEventListener("click", () => {
    aprsCollapseDup = !aprsCollapseDup;
    renderAprsHistory();
  });
}

["all", "position", "message", "weather", "telemetry", "other"].forEach((type) => {
  const btn = document.getElementById(`aprs-type-${type}`);
  if (!btn) return;
  btn.addEventListener("click", () => {
    aprsTypeFilter = type;
    renderAprsHistory();
  });
});

if (aprsFilterInput) {
  aprsFilterInput.addEventListener("input", () => {
    aprsFilterText = aprsFilterInput.value.trim().toUpperCase();
    renderAprsHistory();
  });
}

// --- Server-side APRS decode handler ---
window.onServerAprs = function(pkt) {
  aprsStatus.textContent = aprsPaused ? "Paused" : "Receiving";
  addAprsPacket({
    receiver: window.getDecodeRigMeta ? window.getDecodeRigMeta() : null,
    srcCall: pkt.src_call,
    destCall: pkt.dest_call,
    path: pkt.path,
    info: pkt.info,
    info_bytes: pkt.info_bytes,
    type: pkt.packet_type,
    crcOk: pkt.crc_ok,
    ts_ms: pkt.ts_ms,
    lat: pkt.lat,
    lon: pkt.lon,
    symbolTable: pkt.symbol_table,
    symbolCode: pkt.symbol_code,
  });
};

renderAprsHistory();
