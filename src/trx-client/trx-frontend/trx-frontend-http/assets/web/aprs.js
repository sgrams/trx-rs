// --- APRS Decoder Plugin ---
const aprsToggleBtn = document.getElementById("aprs-toggle-btn");
const aprsStatus = document.getElementById("aprs-status");
const aprsPacketsEl = document.getElementById("aprs-packets");
const APRS_MAX_PACKETS = 100;

let aprsActive = false;
let aprsWs = null;
let aprsAudioCtx = null;
let aprsDecoder = null;

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
// Uses delay-and-multiply frequency discriminator for robust non-coherent decoding.
function createDemodulator(sampleRate) {
  const BAUD = 1200;
  const MARK = 1200;
  const SPACE = 2200;
  const samplesPerBit = sampleRate / BAUD;

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

  // Bandpass pre-filter: biquad centered at 1700 Hz (AFSK midpoint), Q=1.2
  // Passes ~1000-2500 Hz, removes out-of-band noise
  const bpfW0 = 2 * Math.PI * ((MARK + SPACE) / 2) / sampleRate;
  const bpfAlpha = Math.sin(bpfW0) / (2 * 1.2);
  const bpfA0 = 1 + bpfAlpha;
  const bpfCoeffs = {
    b0: bpfAlpha / bpfA0, b1: 0, b2: -bpfAlpha / bpfA0,
    a1: (-2 * Math.cos(bpfW0)) / bpfA0, a2: (1 - bpfAlpha) / bpfA0,
  };
  let bpfState = [0, 0, 0, 0]; // x1, x2, y1, y2

  // Delay-and-multiply discriminator
  // Delay = Fs / (2 * Δf) where Δf = space - mark = 1000 Hz
  // At this delay: mark tone → negative product, space tone → positive product
  const DELAY = Math.round(sampleRate / (2 * (SPACE - MARK)));
  const delayBuf = new Float32Array(DELAY);
  let delayIdx = 0;

  // Moving-average LPF over half a bit period
  // First null at 2*mark freq (2400 Hz), cleanly removing discriminator artifacts
  const avgLen = Math.round(samplesPerBit / 2);
  const avgBuf = new Float32Array(avgLen);
  let avgIdx = 0;
  let avgSum = 0;

  // Clock recovery (PLL)
  let lastTone = 0;
  let bitPhase = 0;
  const PLL_GAIN = 0.7;

  // NRZI state
  let prevSampledBit = 0;

  // HDLC state
  let ones = 0;
  let frameBits = [];
  let inFrame = false;

  const frames = [];

  function resetState() {
    bpfState = [0, 0, 0, 0];
    delayBuf.fill(0);
    delayIdx = 0;
    avgBuf.fill(0);
    avgIdx = 0;
    avgSum = 0;
    lastTone = 0;
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

    // Bandpass pre-filter
    const c = bpfCoeffs;
    const filtered = c.b0 * s + c.b1 * bpfState[0] + c.b2 * bpfState[1]
                   - c.a1 * bpfState[2] - c.a2 * bpfState[3];
    bpfState[1] = bpfState[0]; bpfState[0] = s;
    bpfState[3] = bpfState[2]; bpfState[2] = filtered;

    // Delay-and-multiply frequency discriminator
    const delayed = delayBuf[delayIdx];
    delayBuf[delayIdx] = filtered;
    delayIdx = (delayIdx + 1) % DELAY;

    const disc = filtered * delayed;

    // Moving-average LPF
    avgSum += disc - avgBuf[avgIdx];
    avgBuf[avgIdx] = disc;
    avgIdx = (avgIdx + 1) % avgLen;

    // mark (1200 Hz) → negative, space (2200 Hz) → positive
    const bit = avgSum < 0 ? 1 : 0;

    // PLL clock recovery
    if (bit !== lastTone) {
      lastTone = bit;
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

  return { srcCall, destCall, path, info: infoStr, type };
}

function addAprsPacket(pkt) {
  const tag = pkt.crcOk ? "[APRS]" : "[APRS-CRC-FAIL]";
  console.log(tag, `${pkt.srcCall}>${pkt.destCall}${pkt.path ? "," + pkt.path : ""}: ${pkt.info}`, pkt);
  const row = document.createElement("div");
  row.className = "aprs-packet";
  if (!pkt.crcOk) row.style.opacity = "0.5";
  const now = new Date();
  const ts = now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const crcTag = pkt.crcOk ? "" : ' <span style="color:var(--accent-red);">[CRC]</span>';
  row.innerHTML = `<span class="aprs-time">${ts}</span><span class="aprs-call">${pkt.srcCall}</span>&gt;${pkt.destCall}${pkt.path ? "," + pkt.path : ""}: <span title="${pkt.type}">${pkt.info}</span>${crcTag}`;
  aprsPacketsEl.prepend(row);
  while (aprsPacketsEl.children.length > APRS_MAX_PACKETS) {
    aprsPacketsEl.removeChild(aprsPacketsEl.lastChild);
  }
}

function startAprs() {
  if (aprsActive) { stopAprs(); return; }
  if (!hasWebCodecs) {
    aprsStatus.textContent = "Requires Chrome/Edge";
    return;
  }

  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  aprsWs = new WebSocket(`${proto}//${location.host}/audio`);
  aprsWs.binaryType = "arraybuffer";
  aprsStatus.textContent = "Connecting…";

  let demodulator = null;

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
        demodulator = createDemodulator(sr);

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
            const frames = demodulator.processBuffer(mono);
            for (const result of frames) {
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
    stopAprs();
  };

  aprsWs.onerror = () => {
    aprsStatus.textContent = "Connection error";
  };
}

function stopAprs() {
  aprsActive = false;
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

aprsToggleBtn.addEventListener("click", startAprs);
