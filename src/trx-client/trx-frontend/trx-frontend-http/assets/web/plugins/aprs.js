// --- APRS Decoder Plugin ---
const aprsToggleBtn = document.getElementById("aprs-toggle-btn");
const aprsStatus = document.getElementById("aprs-status");
const aprsPacketsEl = document.getElementById("aprs-packets");
const APRS_MAX_PACKETS = 100;

let aprsActive = false;
let aprsWs = null;
let aprsAudioCtx = null;
let aprsDecoder = null;

// Persistent packet history
let aprsPacketHistory = loadSetting("aprsPackets", []);

// CRC-16-CCITT lookup table
const CRC_CCITT_TABLE = new Uint16Array(256);
(function initCrc() {
  for (let i = 0; i < 256; i++) {
    let crc = i;
    for (let j = 0; j < 8; j++) {
      crc = (crc & 1) ? ((crc >>> 1) ^ 0x8408) : (crc >>> 1);
    }
    CRC_CCITT_TABLE[i] = crc;
  }
})();

function crc16ccitt(bytes) {
  let crc = 0xFFFF;
  for (let i = 0; i < bytes.length; i++) {
    crc = (crc >>> 8) ^ CRC_CCITT_TABLE[(crc ^ bytes[i]) & 0xFF];
  }
  return crc ^ 0xFFFF;
}

// AFSK Bell 202 Demodulator (1200 baud, mark=1200Hz, space=2200Hz)
// Uses mark/space correlation detector (non-coherent FSK matched filter).
function createDemodulator(sampleRate, windowFactor) {
  const BAUD = 1200;
  const MARK = 1200;
  const SPACE = 2200;
  const samplesPerBit = sampleRate / BAUD;
  const corrFactor = windowFactor || 1.0;

  // Debug counters
  let dbgSamples = 0;
  let dbgBits = 0;
  let dbgFlags = 0;
  let dbgFrameAttempts = 0;
  let dbgCrcFails = 0;
  let dbgFramesOk = 0;
  let dbgLastLog = 0;

  // Energy gate — reset demodulator when signal is absent
  let energyAcc = 0;
  let energyCount = 0;
  const ENERGY_WINDOW = Math.round(sampleRate * 0.05);
  const ENERGY_THRESHOLD = 0.001;

  // Mark/space correlation detector
  // Mix input with cos/sin reference oscillators at mark and space frequencies,
  // then integrate over one bit period to get I/Q energy at each frequency.
  const markPhaseInc = 2 * Math.PI * MARK / sampleRate;
  const spacePhaseInc = 2 * Math.PI * SPACE / sampleRate;
  let markPhase = 0;
  let spacePhase = 0;

  // Sliding-window matched filter
  const corrLen = Math.max(2, Math.round(samplesPerBit * corrFactor));
  const markIBuf = new Float32Array(corrLen);
  const markQBuf = new Float32Array(corrLen);
  const spaceIBuf = new Float32Array(corrLen);
  const spaceQBuf = new Float32Array(corrLen);
  let corrIdx = 0;
  let markISum = 0, markQSum = 0, spaceISum = 0, spaceQSum = 0;

  // Clock recovery (PLL)
  let lastBit = 0;
  let bitPhase = 0;
  const PLL_GAIN = 0.4;

  // NRZI state
  let prevSampledBit = 0;

  // HDLC state
  let ones = 0;
  let frameBits = [];
  let inFrame = false;

  const frames = [];

  function resetState() {
    markPhase = 0;
    spacePhase = 0;
    markIBuf.fill(0); markQBuf.fill(0);
    spaceIBuf.fill(0); spaceQBuf.fill(0);
    corrIdx = 0;
    markISum = 0; markQSum = 0;
    spaceISum = 0; spaceQSum = 0;
    lastBit = 0;
    bitPhase = 0;
    prevSampledBit = 0;
    ones = 0;
    frameBits = [];
    inFrame = false;
  }

  function processSample(s) {
    // Energy gate
    energyAcc += s * s;
    energyCount++;
    if (energyCount >= ENERGY_WINDOW) {
      if (Math.sqrt(energyAcc / energyCount) < ENERGY_THRESHOLD) {
        resetState();
      }
      energyAcc = 0;
      energyCount = 0;
    }

    // Mix with mark/space reference oscillators
    const mI = s * Math.cos(markPhase);
    const mQ = s * Math.sin(markPhase);
    const sI = s * Math.cos(spacePhase);
    const sQ = s * Math.sin(spacePhase);
    markPhase += markPhaseInc;
    spacePhase += spacePhaseInc;
    if (markPhase > 6.283185307) markPhase -= 6.283185307;
    if (spacePhase > 6.283185307) spacePhase -= 6.283185307;

    // Sliding-window integration (matched filter over 1 bit period)
    markISum += mI - markIBuf[corrIdx];
    markQSum += mQ - markQBuf[corrIdx];
    spaceISum += sI - spaceIBuf[corrIdx];
    spaceQSum += sQ - spaceQBuf[corrIdx];
    markIBuf[corrIdx] = mI;
    markQBuf[corrIdx] = mQ;
    spaceIBuf[corrIdx] = sI;
    spaceQBuf[corrIdx] = sQ;
    corrIdx = (corrIdx + 1) % corrLen;

    // Compare mark vs space energy (I²+Q²)
    const markEnergy = markISum * markISum + markQSum * markQSum;
    const spaceEnergy = spaceISum * spaceISum + spaceQSum * spaceQSum;
    const bit = markEnergy > spaceEnergy ? 1 : 0;

    // PLL clock recovery
    if (bit !== lastBit) {
      lastBit = bit;
      const error = bitPhase - samplesPerBit / 2;
      bitPhase -= PLL_GAIN * error;
    }

    bitPhase--;
    if (bitPhase <= 0) {
      bitPhase += samplesPerBit;
      dbgBits++;
      processBit(bit);
    }

    dbgSamples++;
  }

  function processBit(rawBit) {
    // NRZI decode: no transition = 1, transition = 0
    const decodedBit = (rawBit === prevSampledBit) ? 1 : 0;
    prevSampledBit = rawBit;

    if (decodedBit === 1) {
      // Don't push yet — buffer in ones counter until we know
      // these aren't part of a flag, stuff, or abort sequence
      ones++;
      return;
    }

    // decodedBit === 0
    if (ones >= 7) {
      // Abort sequence — reset
      inFrame = false;
      frameBits = [];
      ones = 0;
      return;
    }
    if (ones === 6) {
      // Flag (01111110) — frame boundary; the 6 ones are flag bits, not data
      dbgFlags++;
      if (inFrame && frameBits.length >= 136) {
        dbgFrameAttempts++;
        const result = bitsToBytes(frameBits);
        if (result) {
          if (result.crcOk) dbgFramesOk++;
          frames.push(result);
        }
      }
      frameBits = [];
      inFrame = true;
      ones = 0;
      return;
    }
    if (ones === 5) {
      // Bit stuffing — flush the 5 data ones, discard the stuffed zero
      if (inFrame) {
        for (let k = 0; k < 5; k++) frameBits.push(1);
      }
      ones = 0;
      return;
    }

    // Normal data: flush buffered ones then push the zero
    if (inFrame) {
      for (let k = 0; k < ones; k++) frameBits.push(1);
      frameBits.push(0);
    }
    ones = 0;
  }

  function bitsToBytes(bits) {
    const byteLen = Math.floor(bits.length / 8);
    if (byteLen < 17) return null;
    const bytes = new Uint8Array(byteLen);
    for (let i = 0; i < byteLen; i++) {
      let b = 0;
      for (let j = 0; j < 8; j++) {
        b |= (bits[i * 8 + j] << j);
      }
      bytes[i] = b;
    }

    // Verify FCS (last 2 bytes)
    const payload = bytes.subarray(0, byteLen - 2);
    const fcs = bytes[byteLen - 2] | (bytes[byteLen - 1] << 8);
    const computed = crc16ccitt(payload);
    if (computed !== fcs) {
      dbgCrcFails++;
      // Try to decode addresses for diagnostics
      let addrInfo = "";
      if (payload.length >= 14) {
        const dstCall = Array.from(payload.subarray(0, 6)).map(b => String.fromCharCode(b >> 1)).join("").trim();
        const srcCall = Array.from(payload.subarray(7, 13)).map(b => String.fromCharCode(b >> 1)).join("").trim();
        addrInfo = ` dst="${dstCall}" src="${srcCall}"`;
      }
      console.debug("[APRS-DBG] CRC fail:", byteLen, "bytes, fcs=0x" + fcs.toString(16),
        "computed=0x" + computed.toString(16), "bits:", bits.length, addrInfo,
        "hex:", Array.from(bytes.subarray(0, Math.min(20, byteLen))).map(b => b.toString(16).padStart(2, "0")).join(" "));
      // Return as suspect frame for display
      return { payload, crcOk: false };
    }

    return { payload, crcOk: true };
  }

  function processBuffer(samples) {
    for (let i = 0; i < samples.length; i++) {
      processSample(samples[i]);
    }
    // Periodic debug log every 3 seconds
    const now = Date.now();
    if (now - dbgLastLog >= 3000) {
      console.log("[APRS-DBG] samples:", dbgSamples, "bits:", dbgBits, "flags:", dbgFlags,
        "frameAttempts:", dbgFrameAttempts, "crcFails:", dbgCrcFails, "ok:", dbgFramesOk);
      dbgLastLog = now;
    }
    const result = frames.splice(0);
    return result;
  }

  return { processBuffer };
}

// AX.25 address extraction
function decodeAX25Address(bytes, offset) {
  let call = "";
  for (let i = 0; i < 6; i++) {
    const ch = bytes[offset + i] >> 1;
    if (ch > 32) call += String.fromCharCode(ch);
  }
  call = call.trimEnd();
  const ssid = (bytes[offset + 6] >> 1) & 0x0F;
  const last = (bytes[offset + 6] & 0x01) === 1;
  return { call, ssid, last };
}

function parseAX25(frame) {
  if (frame.length < 16) return null;
  const dest = decodeAX25Address(frame, 0);
  const src = decodeAX25Address(frame, 7);

  let offset = 14;
  const digis = [];
  let lastAddr = src.last;
  while (!lastAddr && offset + 7 <= frame.length) {
    const digi = decodeAX25Address(frame, offset);
    digis.push(digi);
    lastAddr = digi.last;
    offset += 7;
  }

  if (offset + 2 > frame.length) return null;
  const control = frame[offset];
  const pid = frame[offset + 1];
  const info = frame.subarray(offset + 2);

  return { src, dest, digis, control, pid, info };
}

function parseAPRS(ax25) {
  const srcCall = ax25.src.ssid ? `${ax25.src.call}-${ax25.src.ssid}` : ax25.src.call;
  const destCall = ax25.dest.ssid ? `${ax25.dest.call}-${ax25.dest.ssid}` : ax25.dest.call;
  const path = ax25.digis.map((d) => d.ssid ? `${d.call}-${d.ssid}` : d.call).join(",");
  const infoStr = new TextDecoder().decode(ax25.info);

  let type = "Unknown";
  if (infoStr.length > 0) {
    const dt = infoStr[0];
    if (dt === "!" || dt === "=" || dt === "/" || dt === "@") type = "Position";
    else if (dt === ":") type = "Message";
    else if (dt === ">") type = "Status";
    else if (dt === "T") type = "Telemetry";
    else if (dt === ";") type = "Object";
    else if (dt === ")") type = "Item";
    else if (dt === "`" || dt === "'") type = "Mic-E";
  }

  const result = { srcCall, destCall, path, info: infoStr, type };

  if (type === "Position") {
    const pos = parseAprsPosition(infoStr);
    if (pos) {
      result.lat = pos.lat;
      result.lon = pos.lon;
      result.symbolTable = pos.symbolTable;
      result.symbolCode = pos.symbolCode;
    }
  }

  return result;
}

function parseAprsPosition(infoStr) {
  if (infoStr.length < 1) return null;
  const dt = infoStr[0];
  let posStr;

  if (dt === "!" || dt === "=") {
    posStr = infoStr.substring(1);
  } else if (dt === "/" || dt === "@") {
    if (infoStr.length < 8) return null;
    posStr = infoStr.substring(8);
  } else {
    return null;
  }

  if (posStr.length < 1) return null;

  // Compressed format: first char is symbol table (not a digit)
  // Layout: T YYYY XXXX C [cs T] — 10 chars minimum
  const firstChar = posStr[0];
  if (firstChar < "0" || firstChar > "9") {
    return parseAprsCompressed(posStr);
  }

  // Uncompressed: DDMM.MMN/DDDMM.MMEsYYY...
  // Need at least: 8 lat + 1 table + 9 lon + 1 code = 19 chars
  if (posStr.length < 19) return null;

  const latStr = posStr.substring(0, 8);  // DDMM.MMN
  const symbolTable = posStr[8];
  const lonStr = posStr.substring(9, 18); // DDDMM.MME
  const symbolCode = posStr[18];

  const lat = parseAprsLat(latStr);
  const lon = parseAprsLon(lonStr);
  if (lat === null || lon === null) return null;

  return { lat, lon, symbolTable, symbolCode };
}

function parseAprsCompressed(posStr) {
  // Compressed position: SymTable(1) Lat(4) Lon(4) SymCode(1) = 10 chars min
  if (posStr.length < 10) return null;

  const symbolTable = posStr[0];
  const latChars = posStr.substring(1, 5);
  const lonChars = posStr.substring(5, 9);
  const symbolCode = posStr[9];

  // Base-91 decode: each char value = (ASCII - 33)
  let latVal = 0;
  let lonVal = 0;
  for (let i = 0; i < 4; i++) {
    const lc = latChars.charCodeAt(i) - 33;
    const xc = lonChars.charCodeAt(i) - 33;
    if (lc < 0 || lc > 90 || xc < 0 || xc > 90) return null;
    latVal = latVal * 91 + lc;
    lonVal = lonVal * 91 + xc;
  }

  const lat = 90 - latVal / 380926;
  const lon = -180 + lonVal / 190463;

  if (lat < -90 || lat > 90 || lon < -180 || lon > 180) return null;

  return {
    lat: Math.round(lat * 1e6) / 1e6,
    lon: Math.round(lon * 1e6) / 1e6,
    symbolTable,
    symbolCode,
  };
}

function parseAprsLat(s) {
  // DDMM.MMN
  if (s.length < 8) return null;
  const deg = parseInt(s.substring(0, 2), 10);
  const min = parseFloat(s.substring(2, 7));
  const ns = s[7];
  if (isNaN(deg) || isNaN(min)) return null;
  let lat = deg + min / 60;
  if (ns === "S" || ns === "s") lat = -lat;
  else if (ns !== "N" && ns !== "n") return null;
  return Math.round(lat * 1e6) / 1e6;
}

function parseAprsLon(s) {
  // DDDMM.MME
  if (s.length < 9) return null;
  const deg = parseInt(s.substring(0, 3), 10);
  const min = parseFloat(s.substring(3, 8));
  const ew = s[8];
  if (isNaN(deg) || isNaN(min)) return null;
  let lon = deg + min / 60;
  if (ew === "W" || ew === "w") lon = -lon;
  else if (ew !== "E" && ew !== "e") return null;
  return Math.round(lon * 1e6) / 1e6;
}

function escapeAprsInfo(str) {
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
      out += `<span style="color:var(--accent-yellow);">[0x${hex}]</span>`;
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
  row.innerHTML = `<span class="aprs-time">${ts}</span>${symbolHtml}<span class="aprs-call">${pkt.srcCall}</span>&gt;${pkt.destCall}${pkt.path ? "," + pkt.path : ""}: <span title="${pkt.type}">${escapeAprsInfo(pkt.info)}</span>${posHtml}${crcTag}`;
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

function startAprs() {
  if (aprsActive) return;
  if (!hasWebCodecs) {
    aprsStatus.textContent = "Requires Chrome/Edge";
    return;
  }

  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  aprsWs = new WebSocket(`${proto}//${location.host}/audio`);
  aprsWs.binaryType = "arraybuffer";
  aprsStatus.textContent = "Connecting…";

  let demodulators = null;

  aprsWs.onopen = () => {
    aprsStatus.textContent = "Waiting for stream info…";
  };

  aprsWs.onmessage = (evt) => {
    if (typeof evt.data === "string") {
      try {
        const info = JSON.parse(evt.data);
        const sr = info.sample_rate || 48000;
        const ch = info.channels || 1;
        aprsAudioCtx = new AudioContext({ sampleRate: sr });
        // Multiple decoders with different correlation window lengths
        // for robustness — different windows produce different error patterns
        demodulators = [
          createDemodulator(sr, 1.0),
          createDemodulator(sr, 0.5),
        ];

        let aprsFrameCount = 0;
        aprsDecoder = new AudioDecoder({
          output: (frame) => {
            if (aprsFrameCount++ === 0) {
              console.log("[APRS-DBG] First PCM frame:", frame.numberOfFrames, "samples,", frame.numberOfChannels, "ch, format:", frame.format, "sr:", frame.sampleRate);
            }
            const buf = new Float32Array(frame.numberOfFrames * frame.numberOfChannels);
            frame.copyTo(buf, { planeIndex: 0 });
            // Use first channel only
            let mono;
            if (frame.numberOfChannels === 1) {
              mono = buf;
            } else {
              mono = new Float32Array(frame.numberOfFrames);
              for (let i = 0; i < frame.numberOfFrames; i++) {
                mono[i] = buf[i * frame.numberOfChannels];
              }
            }
            // Run all decoders and merge results, preferring CRC-ok frames
            const seen = new Set();
            const allResults = [];
            for (const demod of demodulators) {
              for (const result of demod.processBuffer(mono)) {
                const hex = Array.from(result.payload.subarray(0, Math.min(14, result.payload.length)))
                  .map(b => b.toString(16).padStart(2, "0")).join("");
                const key = hex + ":" + result.payload.length;
                if (seen.has(key)) continue;
                seen.add(key);
                allResults.push(result);
              }
            }
            // Show CRC-ok frames first, then CRC-fail frames
            allResults.sort((a, b) => (b.crcOk ? 1 : 0) - (a.crcOk ? 1 : 0));
            for (const result of allResults) {
              const ax25 = parseAX25(result.payload);
              if (!ax25) continue;
              const pkt = parseAPRS(ax25);
              pkt.crcOk = result.crcOk;
              addAprsPacket(pkt);
            }
            frame.close();
          },
          error: (e) => { console.error("APRS AudioDecoder error", e); }
        });
        aprsDecoder.configure({
          codec: "opus",
          sampleRate: sr,
          numberOfChannels: ch,
        });

        aprsActive = true;
        saveSetting("aprsRunning", true);
        aprsToggleBtn.style.borderColor = "#00d17f";
        aprsToggleBtn.style.color = "#00d17f";
        aprsToggleBtn.textContent = "Stop APRS";
        aprsStatus.textContent = "Listening…";
      } catch (e) {
        console.error("APRS stream info error", e);
        aprsStatus.textContent = "Error";
      }
      return;
    }

    // Binary Opus data
    if (!aprsDecoder) return;
    try {
      aprsDecoder.decode(new EncodedAudioChunk({
        type: "key",
        timestamp: performance.now() * 1000,
        data: new Uint8Array(evt.data),
      }));
    } catch (e) {
      // Ignore individual decode errors
    }
  };

  aprsWs.onclose = () => {
    stopAprs(false);
  };

  aprsWs.onerror = () => {
    aprsStatus.textContent = "Connection error";
  };
}

function stopAprs(explicit) {
  aprsActive = false;
  if (explicit) saveSetting("aprsRunning", false);
  if (aprsWs) { aprsWs.close(); aprsWs = null; }
  if (aprsAudioCtx) { aprsAudioCtx.close(); aprsAudioCtx = null; }
  if (aprsDecoder) {
    try { aprsDecoder.close(); } catch (e) {}
    aprsDecoder = null;
  }
  aprsToggleBtn.style.borderColor = "";
  aprsToggleBtn.style.color = "";
  aprsToggleBtn.textContent = "Start APRS";
  aprsStatus.textContent = "Stopped";
}

aprsToggleBtn.addEventListener("click", () => {
  if (aprsActive) { stopAprs(true); } else { startAprs(); }
});

document.getElementById("aprs-clear-btn").addEventListener("click", () => {
  aprsPacketsEl.innerHTML = "";
  aprsPacketHistory = [];
  saveSetting("aprsPackets", []);
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
  addAprsPacket({
    srcCall: pkt.src_call,
    destCall: pkt.dest_call,
    path: pkt.path,
    info: pkt.info,
    type: pkt.packet_type,
    crcOk: pkt.crc_ok,
    lat: pkt.lat,
    lon: pkt.lon,
    symbolTable: pkt.symbol_table,
    symbolCode: pkt.symbol_code,
  });
};

// Update status display based on server decode availability
function updateAprsStatus() {
  if (typeof decodeConnected !== "undefined" && decodeConnected) {
    if (!aprsActive) {
      aprsStatus.textContent = "Server decode active";
      aprsToggleBtn.textContent = "Start APRS (browser)";
    }
  }
}
setInterval(updateAprsStatus, 2000);

// Auto-start APRS if it was running before page refresh (browser fallback)
if (loadSetting("aprsRunning", false) && hasWebCodecs) {
  startAprs();
}
