// --- APRS Decoder Plugin (server-side decode) ---
const aprsStatus = document.getElementById("aprs-status");
const aprsPacketsEl = document.getElementById("aprs-packets");
const APRS_MAX_PACKETS = 100;

// Persistent packet history
let aprsPacketHistory = loadSetting("aprsPackets", []);

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
  row.innerHTML = `<span class="aprs-time">${ts}</span>${symbolHtml}<span class="aprs-call">${pkt.srcCall}</span>&gt;${pkt.destCall}${pkt.path ? "," + pkt.path : ""}: <span title="${pkt.type}">${renderAprsInfo(pkt)}</span>${posHtml}${crcTag}`;
  return row;
}

function addAprsPacket(pkt) {
  const tag = pkt.crcOk ? "[APRS]" : "[APRS-CRC-FAIL]";
  console.log(tag, `${pkt.srcCall}>${pkt.destCall}${pkt.path ? "," + pkt.path : ""}: ${pkt.info}`, pkt);

  // Stamp timestamp for persistence
  pkt._ts = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  // Persist to history
  aprsPacketHistory.unshift(pkt);
  if (aprsPacketHistory.length > APRS_MAX_PACKETS) aprsPacketHistory.length = APRS_MAX_PACKETS;
  saveSetting("aprsPackets", aprsPacketHistory);

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

// --- Server-side APRS decode handler ---
window.onServerAprs = function(pkt) {
  aprsStatus.textContent = "Receiving";
  addAprsPacket({
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
