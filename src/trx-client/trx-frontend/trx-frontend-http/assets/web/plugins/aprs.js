// --- APRS Decoder Plugin (server-side decode) ---
const aprsStatus = document.getElementById("aprs-status");
const aprsPacketsEl = document.getElementById("aprs-packets");
const aprsFilterInput = document.getElementById("aprs-filter");
const aprsBarOverlay = document.getElementById("aprs-bar-overlay");
const APRS_MAX_PACKETS = 100;
const APRS_BAR_MAX = 5;
let aprsFilterText = "";

// Persistent packet history
let aprsPacketHistory = loadSetting("aprsPackets", []);
// Ring buffer of last N packets for the overview bar
let aprsBarFrames = [];

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

function renderAprsRow(pkt) {
  const row = document.createElement("div");
  row.className = "aprs-packet";
  if (!pkt.crcOk) row.style.opacity = "0.5";
  const ts = pkt._ts || new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const crcTag = pkt.crcOk ? "" : ' <span style="color:var(--accent-red);">[CRC]</span>';
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
  let posHtml = "";
  if (pkt.lat != null && pkt.lon != null) {
    const osmUrl = `https://www.openstreetmap.org/?mlat=${pkt.lat}&mlon=${pkt.lon}#map=15/${pkt.lat}/${pkt.lon}`;
    posHtml = ` <a class="aprs-pos" href="${osmUrl}" target="_blank">${pkt.lat.toFixed(4)}, ${pkt.lon.toFixed(4)}</a>`;
  }
  const receiverHtml = pkt.receiver
    ? `<span class="decode-rig-badge" style="--decode-rig-color:${pkt.receiver.color};">${pkt.receiver.label}</span> `
    : "";
  row.dataset.filterText = [
    pkt.receiver ? pkt.receiver.label : "",
    pkt.srcCall,
    pkt.destCall,
    pkt.path,
    pkt.info,
    pkt.type,
    pkt.lat != null ? pkt.lat.toFixed(4) : "",
    pkt.lon != null ? pkt.lon.toFixed(4) : "",
  ]
    .filter(Boolean)
    .join(" ")
    .toUpperCase();
  row.innerHTML = `<span class="aprs-time">${ts}</span>${receiverHtml}${symbolHtml}<span class="aprs-call">${pkt.srcCall}</span>&gt;${pkt.destCall}${pkt.path ? "," + pkt.path : ""}: <span title="${pkt.type}">${renderAprsInfo(pkt)}</span>${posHtml}${crcTag}`;
  applyAprsFilterToRow(row);
  return row;
}

function applyAprsFilterToRow(row) {
  if (!aprsFilterText) {
    row.style.display = "";
    return;
  }
  const message = row.dataset.filterText || "";
  row.style.display = message.includes(aprsFilterText) ? "" : "none";
}

function applyAprsFilterToAll() {
  const rows = aprsPacketsEl.querySelectorAll(".aprs-packet");
  rows.forEach((row) => applyAprsFilterToRow(row));
}

function updateAprsBar() {
  if (!aprsBarOverlay) return;
  const isPkt = (document.getElementById("mode")?.value || "").toUpperCase() === "PKT";
  if (!isPkt || aprsBarFrames.length === 0) {
    aprsBarOverlay.style.display = "none";
    return;
  }
  let html = '<div class="aprs-bar-header">APRS</div>';
  for (const pkt of aprsBarFrames) {
    const ts = pkt._ts ? `<span class="aprs-bar-time">${pkt._ts}</span>` : "";
    const call = `<span class="aprs-bar-call">${escapeMapHtml(pkt.srcCall)}</span>`;
    const dest = escapeMapHtml(pkt.destCall || "");
    const info = escapeMapHtml((pkt.info || "").slice(0, 60));
    html += `<div class="aprs-bar-frame">${ts}${call}>${dest}: ${info}</div>`;
  }
  aprsBarOverlay.innerHTML = html;
  aprsBarOverlay.style.display = "flex";
}
window.updateAprsBar = updateAprsBar;

function addAprsPacket(pkt) {
  const tag = pkt.crcOk ? "[APRS]" : "[APRS-CRC-FAIL]";
  console.log(tag, `${pkt.srcCall}>${pkt.destCall}${pkt.path ? "," + pkt.path : ""}: ${pkt.info}`, pkt);

  // Stamp timestamp for persistence
  pkt._ts = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  // Persist to history
  aprsPacketHistory.unshift(pkt);
  if (aprsPacketHistory.length > APRS_MAX_PACKETS) aprsPacketHistory.length = APRS_MAX_PACKETS;
  saveSetting("aprsPackets", aprsPacketHistory);

  // Update overview bar (CRC-failed frames excluded)
  if (pkt.crcOk) {
    aprsBarFrames.unshift(pkt);
    if (aprsBarFrames.length > APRS_BAR_MAX) aprsBarFrames.length = APRS_BAR_MAX;
    updateAprsBar();
  }

  const row = renderAprsRow(pkt);
  if (pkt.lat != null && pkt.lon != null && window.aprsMapAddStation) {
    window.aprsMapAddStation(pkt.srcCall, pkt.lat, pkt.lon, pkt.info, pkt.symbolTable, pkt.symbolCode);
  }
  aprsPacketsEl.prepend(row);
  while (aprsPacketsEl.children.length > APRS_MAX_PACKETS) {
    aprsPacketsEl.removeChild(aprsPacketsEl.lastChild);
  }
}

document.getElementById("aprs-clear-btn").addEventListener("click", async () => {
  aprsPacketsEl.innerHTML = "";
  aprsPacketHistory = [];
  saveSetting("aprsPackets", []);
  aprsBarFrames = [];
  updateAprsBar();
  try { await postPath("/clear_aprs_decode"); } catch (e) { console.error("APRS clear failed", e); }
});

// Restore saved packets and map markers on page load
for (let i = aprsPacketHistory.length - 1; i >= 0; i--) {
  const pkt = aprsPacketHistory[i];
  aprsPacketsEl.prepend(renderAprsRow(pkt));
  if (pkt.lat != null && pkt.lon != null && window.aprsMapAddStation) {
    window.aprsMapAddStation(pkt.srcCall, pkt.lat, pkt.lon, pkt.info, pkt.symbolTable, pkt.symbolCode);
  }
}
// Pre-populate bar from history (most recent first, CRC-ok only)
aprsBarFrames = aprsPacketHistory.filter((p) => p.crcOk).slice(0, APRS_BAR_MAX);
updateAprsBar();

if (aprsFilterInput) {
  aprsFilterInput.addEventListener("input", () => {
    aprsFilterText = aprsFilterInput.value.trim().toUpperCase();
    applyAprsFilterToAll();
  });
}

// --- Server-side APRS decode handler ---
window.onServerAprs = function(pkt) {
  aprsStatus.textContent = "Receiving";
  addAprsPacket({
    receiver: window.getDecodeRigMeta ? window.getDecodeRigMeta() : null,
    srcCall: pkt.src_call,
    destCall: pkt.dest_call,
    path: pkt.path,
    info: pkt.info,
    info_bytes: pkt.info_bytes,
    type: pkt.packet_type,
    crcOk: pkt.crc_ok,
    lat: pkt.lat,
    lon: pkt.lon,
    symbolTable: pkt.symbol_table,
    symbolCode: pkt.symbol_code,
  });
};
