// --- HF APRS Decoder Plugin (server-side decode, 300 baud) ---
const hfAprsStatus = document.getElementById("hf-aprs-status");
const hfAprsPacketsEl = document.getElementById("hf-aprs-packets");
const hfAprsFilterInput = document.getElementById("hf-aprs-filter");
const hfAprsPauseBtn = document.getElementById("hf-aprs-pause-btn");
const hfAprsOnlyPosBtn = document.getElementById("hf-aprs-only-pos-btn");
const hfAprsHideCrcBtn = document.getElementById("hf-aprs-hide-crc-btn");
const hfAprsCollapseDupBtn = document.getElementById("hf-aprs-collapse-dup-btn");
const hfAprsTotalCountEl = document.getElementById("hf-aprs-total-count");
const hfAprsVisibleCountEl = document.getElementById("hf-aprs-visible-count");
const hfAprsLatestSeenEl = document.getElementById("hf-aprs-latest-seen");
let hfAprsFilterText = "";
let hfAprsPacketHistory = [];
let hfAprsPaused = false;
let hfAprsBufferedWhilePaused = 0;
let hfAprsOnlyPos = false;
let hfAprsHideCrc = false;
let hfAprsCollapseDup = false;
let hfAprsTypeFilter = "all";

function currentHfAprsHistoryRetentionMs() {
  return typeof window.getDecodeHistoryRetentionMs === "function"
    ? window.getDecodeHistoryRetentionMs()
    : 24 * 60 * 60 * 1000;
}

function pruneHfAprsPacketHistory() {
  const cutoffMs = Date.now() - currentHfAprsHistoryRetentionMs();
  hfAprsPacketHistory = hfAprsPacketHistory.filter((pkt) => Number(pkt?._tsMs) >= cutoffMs);
}

function scheduleHfAprsHistoryRender() {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob("hf-aprs-history", () => renderHfAprsHistory());
    return;
  }
  renderHfAprsHistory();
}

function hfAprsPacketCategory(pkt) {
  const type = String(pkt.type || "").toLowerCase();
  const info = String(pkt.info || "").toLowerCase();
  if (pkt.lat != null && pkt.lon != null || type.includes("position")) return "position";
  if (type.includes("message") || info.startsWith(":")) return "message";
  if (type.includes("weather") || info.startsWith("_")) return "weather";
  if (type.includes("telemetry") || info.startsWith("t#")) return "telemetry";
  return "other";
}

function hfAprsCategoryLabel(category) {
  switch (category) {
    case "position": return "Position";
    case "message": return "Message";
    case "weather": return "Weather";
    case "telemetry": return "Telemetry";
    default: return "Other";
  }
}

function hfAprsAgeText(tsMs) {
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

function hfAprsDistanceText(pkt) {
  if (serverLat == null || serverLon == null || pkt.lat == null || pkt.lon == null) return "";
  const distKm = haversineKm(serverLat, serverLon, pkt.lat, pkt.lon);
  if (!Number.isFinite(distKm)) return "";
  if (distKm < 1) return `${Math.round(distKm * 1000)} m from TRX`;
  return `${distKm.toFixed(1)} km from TRX`;
}

function hfAprsPacketSignature(pkt) {
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

function hfAprsHexBytes(bytes) {
  if (!Array.isArray(bytes) || bytes.length === 0) return "--";
  return bytes.map((b) => Number(b).toString(16).toUpperCase().padStart(2, "0")).join(" ");
}

function hfAprsFilterMatch(pkt) {
  if (hfAprsOnlyPos && (pkt.lat == null || pkt.lon == null)) return false;
  if (hfAprsHideCrc && !pkt.crcOk) return false;
  if (hfAprsTypeFilter !== "all" && hfAprsPacketCategory(pkt) !== hfAprsTypeFilter) return false;
  if (!hfAprsFilterText) return true;
  const haystack = [
    pkt.srcCall,
    pkt.destCall,
    pkt.path,
    pkt.info,
    pkt.type,
    pkt.lat != null ? pkt.lat.toFixed(4) : "",
    pkt.lon != null ? pkt.lon.toFixed(4) : "",
    hfAprsPacketCategory(pkt),
  ]
    .filter(Boolean)
    .join(" ")
    .toUpperCase();
  return haystack.includes(hfAprsFilterText);
}

function hfAprsVisiblePackets() {
  const packets = hfAprsCollapseDup ? collapseHfAprsDuplicates(hfAprsPacketHistory) : hfAprsPacketHistory;
  return packets.filter(hfAprsFilterMatch);
}

function collapseHfAprsDuplicates(packets) {
  const seen = new Set();
  const out = [];
  for (const pkt of packets) {
    const key = hfAprsPacketSignature(pkt);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(pkt);
  }
  return out;
}

function updateHfAprsSummary() {
  const visible = hfAprsVisiblePackets();
  if (hfAprsTotalCountEl) {
    hfAprsTotalCountEl.textContent = `${hfAprsPacketHistory.length} total`;
  }
  if (hfAprsVisibleCountEl) {
    let text = `${visible.length} shown`;
    if (hfAprsPaused && hfAprsBufferedWhilePaused > 0) {
      text += ` · ${hfAprsBufferedWhilePaused} buffered`;
    }
    hfAprsVisibleCountEl.textContent = text;
  }
  if (hfAprsLatestSeenEl) {
    const latest = hfAprsPacketHistory[0];
    if (!latest) {
      hfAprsLatestSeenEl.textContent = "No packets yet";
    } else {
      hfAprsLatestSeenEl.textContent = `${latest.srcCall} ${hfAprsAgeText(latest._tsMs)}`;
    }
  }
}

function updateHfAprsChipState() {
  document.querySelectorAll("[id^='hf-aprs-type-']").forEach((btn) => {
    btn.classList.toggle("active", btn.id === `hf-aprs-type-${hfAprsTypeFilter}`);
  });
  hfAprsOnlyPosBtn?.classList.toggle("active", hfAprsOnlyPos);
  hfAprsHideCrcBtn?.classList.toggle("active", hfAprsHideCrc);
  hfAprsCollapseDupBtn?.classList.toggle("active", hfAprsCollapseDup);
  if (hfAprsPauseBtn) {
    hfAprsPauseBtn.textContent = hfAprsPaused ? "Resume" : "Pause";
    hfAprsPauseBtn.classList.toggle("active", hfAprsPaused);
  }
}

function renderHfAprsInfo(pkt) {
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

function renderHfAprsRow(pkt, isFresh) {
  const row = document.createElement("div");
  row.className = "aprs-packet";
  if (!pkt.crcOk) row.classList.add("aprs-packet-crc");
  if (isFresh) row.classList.add("aprs-packet-new");

  const ts = pkt._ts || new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const age = hfAprsAgeText(pkt._tsMs);
  const category = hfAprsPacketCategory(pkt);
  const categoryLabel = hfAprsCategoryLabel(category);
  const categoryClass = `aprs-badge aprs-badge-type aprs-badge-type-${category}`;
  const pathBadge = pkt.path ? `<span class="aprs-badge">${escapeMapHtml(pkt.path)}</span>` : "";
  const crcBadge = pkt.crcOk ? "" : '<span class="aprs-badge aprs-badge-crc">CRC Fail</span>';
  const hfBadge = '<span class="aprs-badge" style="background:var(--accent-alt,#f59e0b);color:#000">HF</span>';
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
  const distance = hfAprsDistanceText(pkt);
  const qrzHref = `https://qrzcq.com/call/${encodeURIComponent(pkt.srcCall || "")}`;

  row.innerHTML =
    `<div class="aprs-row-head">` +
      `<span class="aprs-time">${ts}</span>` +
      hfBadge +
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
      `<span title="${escapeMapHtml(pkt.type || "")}">${renderHfAprsInfo(pkt)}</span>` +
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
        `<span class="aprs-detail-label">Info Bytes</span><span class="aprs-detail-value">${escapeMapHtml(hfAprsHexBytes(pkt.info_bytes))}</span>` +
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

function renderHfAprsHistory() {
  pruneHfAprsPacketHistory();
  if (!hfAprsPacketsEl || hfAprsPaused) {
    updateHfAprsSummary();
    updateHfAprsChipState();
    return;
  }
  const visible = hfAprsVisiblePackets();
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < visible.length; i++) {
    fragment.appendChild(renderHfAprsRow(visible[i], i === 0));
  }
  hfAprsPacketsEl.replaceChildren(fragment);
  updateHfAprsSummary();
  updateHfAprsChipState();
}

window.resetHfAprsHistoryView = function() {
  if (hfAprsPacketsEl) hfAprsPacketsEl.innerHTML = "";
  hfAprsPacketHistory = [];
  hfAprsBufferedWhilePaused = 0;
  renderHfAprsHistory();
};

window.pruneHfAprsHistoryView = function() {
  pruneHfAprsPacketHistory();
  renderHfAprsHistory();
};

function addHfAprsPacket(pkt) {
  const tsMs = Number.isFinite(pkt.ts_ms) ? Number(pkt.ts_ms) : Date.now();
  pkt._tsMs = tsMs;
  pkt._ts = new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  hfAprsPacketHistory.unshift(pkt);
  pruneHfAprsPacketHistory();

  if (hfAprsPaused) {
    hfAprsBufferedWhilePaused += 1;
    updateHfAprsSummary();
    updateHfAprsChipState();
    return;
  }

  scheduleHfAprsHistoryRender();
}

function normalizeServerHfAprsPacket(pkt) {
  return {
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
  };
}

window.onServerHfAprsBatch = function(packets) {
  if (!Array.isArray(packets) || packets.length === 0) return;
  if (hfAprsStatus) hfAprsStatus.textContent = hfAprsPaused ? "Paused" : "Receiving";
  const normalized = [];
  for (const pkt of packets) {
    const next = normalizeServerHfAprsPacket(pkt);
    const tsMs = Number.isFinite(next.ts_ms) ? Number(next.ts_ms) : Date.now();
    next._tsMs = tsMs;
    next._ts = new Date(tsMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
    normalized.push(next);
  }
  normalized.reverse();
  hfAprsPacketHistory = normalized.concat(hfAprsPacketHistory);
  pruneHfAprsPacketHistory();
  if (hfAprsPaused) {
    hfAprsBufferedWhilePaused += packets.length;
    updateHfAprsSummary();
    updateHfAprsChipState();
    return;
  }
  scheduleHfAprsHistoryRender();
};

document.getElementById("hf-aprs-decode-toggle-btn")?.addEventListener("click", async () => {
  try { await postPath("/toggle_hf_aprs_decode"); } catch (e) { console.error("HF APRS toggle failed", e); }
});

document.getElementById("hf-aprs-clear-btn")?.addEventListener("click", async () => {
  try {
    await postPath("/clear_hf_aprs_decode");
    window.resetHfAprsHistoryView();
  } catch (e) {
    console.error("HF APRS clear failed", e);
  }
});

if (hfAprsPauseBtn) {
  hfAprsPauseBtn.addEventListener("click", () => {
    hfAprsPaused = !hfAprsPaused;
    if (!hfAprsPaused) {
      hfAprsBufferedWhilePaused = 0;
      renderHfAprsHistory();
    } else {
      updateHfAprsSummary();
      updateHfAprsChipState();
    }
  });
}

if (hfAprsOnlyPosBtn) {
  hfAprsOnlyPosBtn.addEventListener("click", () => {
    hfAprsOnlyPos = !hfAprsOnlyPos;
    renderHfAprsHistory();
  });
}

if (hfAprsHideCrcBtn) {
  hfAprsHideCrcBtn.addEventListener("click", () => {
    hfAprsHideCrc = !hfAprsHideCrc;
    renderHfAprsHistory();
  });
}

if (hfAprsCollapseDupBtn) {
  hfAprsCollapseDupBtn.addEventListener("click", () => {
    hfAprsCollapseDup = !hfAprsCollapseDup;
    renderHfAprsHistory();
  });
}

["all", "position", "message", "weather", "telemetry", "other"].forEach((type) => {
  const btn = document.getElementById(`hf-aprs-type-${type}`);
  if (!btn) return;
  btn.addEventListener("click", () => {
    hfAprsTypeFilter = type;
    renderHfAprsHistory();
  });
});

if (hfAprsFilterInput) {
  hfAprsFilterInput.addEventListener("input", () => {
    hfAprsFilterText = hfAprsFilterInput.value.trim().toUpperCase();
    renderHfAprsHistory();
  });
}

// --- Server-side HF APRS decode handler ---
window.onServerHfAprs = function(pkt) {
  if (hfAprsStatus) hfAprsStatus.textContent = hfAprsPaused ? "Paused" : "Receiving";
  addHfAprsPacket(normalizeServerHfAprsPacket(pkt));
};

renderHfAprsHistory();
