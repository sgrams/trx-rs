const freqEl = document.getElementById("freq");
const modeEl = document.getElementById("mode");
const bandLabel = document.getElementById("band-label");
const powerBtn = document.getElementById("power-btn");
const powerHint = document.getElementById("power-hint");
const vfoEl = document.getElementById("vfo");
const vfoBtn = document.getElementById("vfo-btn");
const signalBar = document.getElementById("signal-bar");
const signalValue = document.getElementById("signal-value");
const pttBtn = document.getElementById("ptt-btn");
const freqBtn = document.getElementById("freq-apply");
const modeBtn = document.getElementById("mode-apply");
const txLimitInput = document.getElementById("tx-limit");
const txLimitBtn = document.getElementById("tx-limit-btn");
const txLimitRow = document.getElementById("tx-limit-row");
const lockBtn = document.getElementById("lock-btn");
const txMeters = document.getElementById("tx-meters");
const pwrBar = document.getElementById("pwr-bar");
const pwrValue = document.getElementById("pwr-value");
const swrBar = document.getElementById("swr-bar");
const swrValue = document.getElementById("swr-value");
const loadingEl = document.getElementById("loading");
const contentEl = document.getElementById("content");
const callsignEl = document.getElementById("callsign");
const loadingTitle = document.getElementById("loading-title");
const loadingSub = document.getElementById("loading-sub");

let lastControl;
let lastTxEn = null;
let lastRendered = null;
let rigName = "Rig";
let hintTimer = null;

function showHint(msg, duration) {
  powerHint.textContent = msg;
  if (hintTimer) clearTimeout(hintTimer);
  if (duration) hintTimer = setTimeout(() => { powerHint.textContent = "Ready"; }, duration);
}
let supportedModes = [];
let supportedBands = [];
let freqDirty = false;
let modeDirty = false;
let initialized = false;
let lastEventAt = Date.now();
let es;
let esHeartbeat;

function formatFreq(hz) {
  if (!Number.isFinite(hz)) return "--";
  if (hz >= 1_000_000_000) {
    return `${(hz / 1_000_000_000).toFixed(3)} GHz`;
  }
  if (hz >= 10_000_000) {
    return `${(hz / 1_000_000).toFixed(3)} MHz`;
  }
  return `${(hz / 1_000).toFixed(1)} kHz`;
}

function parseFreqInput(val) {
  if (!val) return null;
  const trimmed = val.trim().toLowerCase();
  const match = trimmed.match(/^([0-9]+(?:[.,][0-9]+)?)\s*([kmg]hz|[kmg]|hz)?$/);
  if (!match) return null;
  let num = parseFloat(match[1].replace(",", "."));
  const unit = match[2] || "";
  if (Number.isNaN(num)) return null;
  if (unit.startsWith("gh") || unit === "g") {
    num *= 1_000_000_000;
  } else if (unit.startsWith("mh") || unit === "m") {
    num *= 1_000_000;
  } else if (unit.startsWith("kh") || unit === "k") {
    num *= 1_000;
  } else if (!unit) {
    // Heuristic when no unit is provided: large numbers are kHz/Hz, small numbers are MHz.
    if (num >= 1_000_000) {
      // Assume already Hz.
    } else if (num >= 1_000) {
      num *= 1_000; // treat as kHz
    } else {
      num *= 1_000_000; // treat as MHz
    }
  }
  return Math.round(num);
}

function normalizeMode(modeVal) {
  if (typeof modeVal === "string") return modeVal;
  if (modeVal && typeof modeVal === "object") {
    const entries = Object.entries(modeVal);
    if (entries.length > 0) {
      const [variant, value] = entries[0];
      if (variant === "Other" && typeof value === "string") return value;
      return variant;
    }
  }
  return "";
}

function updateSupportedBands(cap) {
  if (cap && Array.isArray(cap.supported_bands)) {
    supportedBands = cap.supported_bands
      .filter((b) => typeof b.low_hz === "number" && typeof b.high_hz === "number" && b.tx_allowed === true)
      .map((b) => ({ low: b.low_hz, high: b.high_hz }));
  } else {
    supportedBands = [];
  }
}

function freqAllowed(hz) {
  if (!Number.isFinite(hz)) return false;
  if (supportedBands.length === 0) return true; // if unknown, don't block
  return supportedBands.some((b) => hz >= b.low && hz <= b.high);
}

function setDisabled(disabled) {
  [freqEl, modeEl, freqBtn, modeBtn, pttBtn, vfoBtn, powerBtn, txLimitInput, txLimitBtn, lockBtn].forEach((el) => {
    if (el) el.disabled = disabled;
  });
}

function render(update) {
  if (!update) return;
  if (update.info && update.info.model) {
    rigName = update.info.model;
  }
  document.getElementById("rig-title").textContent = `${rigName} status`;

  initialized = !!update.initialized;
  if (!initialized) {
    const manu = (update.info && update.info.manufacturer) || rigName || "Rig";
    const model = (update.info && update.info.model) || rigName || "Rig";
    const rev = (update.info && update.info.revision) || "";
    const parts = [manu, model, rev].filter(Boolean).join(" ");
    loadingTitle.textContent = `Initializing ${parts}…`;
    loadingSub.textContent = "";
    console.info("Rig initializing:", { manufacturer: manu, model, revision: rev });
    loadingEl.style.display = "";
    if (contentEl) contentEl.style.display = "none";
    powerHint.textContent = "Initializing rig…";
    setDisabled(true);
    return;
  } else {
    loadingEl.style.display = "none";
    if (contentEl) contentEl.style.display = "";
  }
  // Reveal callsign if provided and non-empty.
  if (callsignEl && callsignEl.textContent.trim() !== "") {
    callsignEl.style.display = "";
  }
  setDisabled(false);
  if (update.info && update.info.capabilities && Array.isArray(update.info.capabilities.supported_modes)) {
    const modes = update.info.capabilities.supported_modes.map(normalizeMode).filter(Boolean);
    if (JSON.stringify(modes) !== JSON.stringify(supportedModes)) {
      supportedModes = modes;
      modeEl.innerHTML = "";
      const empty = document.createElement("option");
      empty.value = "";
      empty.textContent = "--";
      modeEl.appendChild(empty);
      supportedModes.forEach((m) => {
        const opt = document.createElement("option");
        opt.value = m;
        opt.textContent = m;
      modeEl.appendChild(opt);
      });
    }
  }
  if (update.info && update.info.capabilities) {
    updateSupportedBands(update.info.capabilities);
  }
  if (!freqDirty && update.status && update.status.freq && typeof update.status.freq.hz === "number") {
    freqEl.value = formatFreq(update.status.freq.hz);
  }
  if (!modeDirty && update.status && update.status.mode) {
    const mode = normalizeMode(update.status.mode);
    modeEl.value = mode ? mode.toUpperCase() : "";
  }
  if (update.status && typeof update.status.tx_en === "boolean") {
    lastTxEn = update.status.tx_en;
    pttBtn.textContent = update.status.tx_en ? "PTT On" : "PTT Off";
    if (update.status.tx_en) {
      pttBtn.style.background = "var(--accent-red)";
      pttBtn.style.borderColor = "var(--accent-red)";
      pttBtn.style.color = "white";
    } else {
      pttBtn.style.background = "";
      pttBtn.style.borderColor = "";
      pttBtn.style.color = "";
    }
  }
  if (update.status && update.status.vfo && Array.isArray(update.status.vfo.entries)) {
    const entries = update.status.vfo.entries;
    const activeIdx = Number.isInteger(update.status.vfo.active) ? update.status.vfo.active : null;
    const parts = entries.map((entry, idx) => {
      const hz = entry && entry.freq && typeof entry.freq.hz === "number" ? entry.freq.hz : null;
      if (hz === null) return null;
      const mark = activeIdx === idx ? " *" : "";
      const mode = entry.mode ? normalizeMode(entry.mode) : "";
      const modeText = mode ? ` [${mode}]` : "";
      return `${entry.name || `VFO ${idx + 1}`}: ${formatFreq(hz)}${modeText}${mark}`;
    }).filter(Boolean);
    vfoEl.textContent = parts.join("\n") || "--";
    const activeLabel = activeIdx !== null
      ? `VFO ${activeIdx + 1}${entries[activeIdx] && entries[activeIdx].name ? ` (${entries[activeIdx].name})` : ""}`
      : "VFO";
    vfoBtn.textContent = activeLabel;
  } else {
    vfoEl.textContent = "--";
    vfoBtn.textContent = "VFO";
  }
  if (update.status && update.status.rx && typeof update.status.rx.sig === "number") {
    const raw = Math.max(0, update.status.rx.sig);
    let pct;
    let label;
    if (raw <= 9) {
      pct = Math.max(0, Math.min(100, (raw / 9) * 100));
      label = `S${raw.toFixed(1)}`;
    } else {
      const overDb = (raw - 9) * 10;
      pct = 100;
      label = `S9 + ${overDb.toFixed(0)}dB`;
    }
    signalBar.style.width = `${pct}%`;
    signalValue.textContent = label;
  } else {
    signalBar.style.width = "0%";
    signalValue.textContent = "--";
  }
  bandLabel.textContent = typeof update.band === "string" ? update.band : "--";
  if (typeof update.enabled === "boolean") {
    powerBtn.disabled = false;
    powerBtn.textContent = update.enabled ? "Power Off" : "Power On";
    powerHint.textContent = "Ready";
  } else {
    powerBtn.disabled = true;
    powerBtn.textContent = "Toggle Power";
    powerHint.textContent = "State unknown";
  }
  lastControl = update.enabled;

  if (update.status && update.status.tx && typeof update.status.tx.limit === "number") {
    txLimitInput.value = update.status.tx.limit;
    txLimitRow.style.display = "";
  } else {
    txLimitInput.value = "";
    txLimitRow.style.display = "none";
  }

  powerHint.textContent = "Ready";
  const locked = update.status && update.status.lock === true;
  lockBtn.textContent = locked ? "Unlock" : "Lock";

  const tx = update.status && update.status.tx ? update.status.tx : null;
  txMeters.style.display = "";
  if (tx && typeof tx.power === "number") {
    const pct = Math.max(0, Math.min(100, tx.power));
    pwrBar.style.width = `${pct}%`;
    pwrValue.textContent = `PWR ${tx.power.toFixed(0)}%`;
  } else {
    pwrBar.style.width = "0%";
    pwrValue.textContent = "PWR --";
  }
  if (tx && typeof tx.swr === "number") {
    const swr = Math.max(1, tx.swr);
    const pct = Math.max(0, Math.min(100, ((swr - 1) / 2) * 100));
    swrBar.style.width = `${pct}%`;
    swrValue.textContent = `SWR ${tx.swr.toFixed(2)}`;
  } else {
    swrBar.style.width = "0%";
    swrValue.textContent = "SWR --";
  }
}

function connect() {
  if (es) {
    es.close();
  }
  if (esHeartbeat) {
    clearInterval(esHeartbeat);
  }
  es = new EventSource("/events");
  lastEventAt = Date.now();
es.onmessage = (evt) => {
    try {
      if (evt.data === lastRendered) return;
      const data = JSON.parse(evt.data);
      lastRendered = evt.data;
      render(data);
      lastEventAt = Date.now();
      if (data.initialized) {
        powerHint.textContent = "Ready";
      }
    } catch (e) {
      console.error("Bad event data", e);
    }
  };
  es.onerror = () => {
    powerHint.textContent = "Disconnected, retrying…";
    es.close();
    setTimeout(connect, 1000);
  };

  esHeartbeat = setInterval(() => {
    const now = Date.now();
    if (now - lastEventAt > 15000) {
      es.close();
      connect();
    }
  }, 5000);
}

async function postPath(path) {
  const resp = await fetch(path, { method: "POST" });
  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(text || resp.statusText);
  }
  return resp;
}

powerBtn.addEventListener("click", async () => {
  powerBtn.disabled = true;
  showHint("Sending...");
  try {
    await postPath("/toggle_power");
    showHint("Toggled, waiting for update…");
  } catch (err) {
    showHint("Toggle failed", 2000);
    console.error(err);
  } finally {
    powerBtn.disabled = false;
  }
});

vfoBtn.addEventListener("click", async () => {
  vfoBtn.disabled = true;
  showHint("Toggling VFO…");
  try {
    await postPath("/toggle_vfo");
    showHint("VFO toggled", 1200);
  } catch (err) {
    showHint("VFO toggle failed", 2000);
    console.error(err);
  } finally {
    vfoBtn.disabled = false;
  }
});

pttBtn.addEventListener("click", async () => {
  pttBtn.disabled = true;
  showHint("Toggling PTT…");
  try {
    const desired = lastTxEn ? "false" : "true";
    await postPath(`/set_ptt?ptt=${desired}`);
    showHint("PTT command sent", 1500);
  } catch (err) {
    showHint("PTT toggle failed", 2000);
    console.error(err);
  } finally {
    pttBtn.disabled = false;
  }
});

freqBtn.addEventListener("click", async () => {
  const parsed = parseFreqInput(freqEl.value);
  if (parsed === null) {
    showHint("Freq missing", 1500);
    return;
  }
  if (!freqAllowed(parsed)) {
    showHint("Out of supported bands", 1500);
    return;
  }
  freqDirty = false;
  freqBtn.disabled = true;
  showHint("Setting frequency…");
  try {
    await postPath(`/set_freq?hz=${parsed}`);
    showHint("Freq set", 1500);
  } catch (err) {
    showHint("Set freq failed", 2000);
    console.error(err);
  } finally {
    freqBtn.disabled = false;
  }
});
freqEl.addEventListener("keydown", (e) => {
  freqDirty = true;
  if (e.key === "Enter") {
    e.preventDefault();
    freqBtn.click();
  }
});

modeBtn.addEventListener("click", async () => {
  const mode = modeEl.value || "";
  if (!mode) {
    showHint("Mode missing", 1500);
    return;
  }
  modeDirty = false;
  modeBtn.disabled = true;
  showHint("Setting mode…");
  try {
    await postPath(`/set_mode?mode=${encodeURIComponent(mode)}`);
    showHint("Mode set", 1500);
  } catch (err) {
    showHint("Set mode failed", 2000);
    console.error(err);
  } finally {
    modeBtn.disabled = false;
  }
});

modeEl.addEventListener("input", () => {
  modeDirty = true;
});

txLimitBtn.addEventListener("click", async () => {
  const limit = txLimitInput.value;
  if (limit === "" || limit === "--") {
    showHint("Limit missing", 1500);
    return;
  }
  txLimitBtn.disabled = true;
  showHint("Setting TX limit…");
  try {
    await postPath(`/set_tx_limit?limit=${encodeURIComponent(limit)}`);
    showHint("TX limit set", 1500);
  } catch (err) {
    showHint("TX limit failed", 2000);
    console.error(err);
  } finally {
    txLimitBtn.disabled = false;
  }
});

lockBtn.addEventListener("click", async () => {
  lockBtn.disabled = true;
  showHint("Toggling lock…");
  try {
    const nextLock = lockBtn.textContent === "Lock";
    await postPath(nextLock ? "/lock" : "/unlock");
    showHint("Lock toggled", 1500);
  } catch (err) {
    showHint("Lock toggle failed", 2000);
    console.error(err);
  } finally {
    lockBtn.disabled = false;
  }
});

connect();

// --- Audio streaming ---
const rxAudioBtn = document.getElementById("rx-audio-btn");
const txAudioBtn = document.getElementById("tx-audio-btn");
const audioStatus = document.getElementById("audio-status");
const audioLevelFill = document.getElementById("audio-level-fill");
const audioRow = document.getElementById("audio-row");

// Hide audio row if audio is not configured on the server
fetch("/audio", { method: "GET" }).then((r) => {
  if (r.status === 404) audioRow.style.display = "none";
}).catch(() => {});

let audioWs = null;
let audioCtx = null;
let rxActive = false;
let txActive = false;
let txStream = null;
let txProcessor = null;
let streamInfo = null;
let opusDecoder = null;
let txEncoder = null;
let nextPlayTime = 0;
let lastLevelUpdate = 0;
const TX_TIMEOUT_SECS = 120;
let txTimeoutTimer = null;
let txTimeoutRemaining = 0;
let txTimeoutInterval = null;
const hasWebCodecs = typeof AudioDecoder !== "undefined" && typeof AudioEncoder !== "undefined";

// Show compatibility warning for non-Chromium browsers
if (!hasWebCodecs) {
  rxAudioBtn.disabled = true;
  txAudioBtn.disabled = true;
  audioStatus.textContent = "Audio requires Chrome/Edge";
}

function resetTxTimeout() {
  txTimeoutRemaining = TX_TIMEOUT_SECS;
  if (txTimeoutTimer) clearTimeout(txTimeoutTimer);
  txTimeoutTimer = setTimeout(() => {
    console.warn("PTT safety timeout — stopping TX");
    stopTxAudio();
  }, TX_TIMEOUT_SECS * 1000);
}

function startTxTimeoutCountdown() {
  txTimeoutRemaining = TX_TIMEOUT_SECS;
  if (txTimeoutInterval) clearInterval(txTimeoutInterval);
  txTimeoutInterval = setInterval(() => {
    txTimeoutRemaining--;
    if (txTimeoutRemaining <= 10 && txTimeoutRemaining > 0 && txActive) {
      audioStatus.textContent = `TX timeout ${txTimeoutRemaining}s`;
    }
  }, 1000);
}

function clearTxTimeout() {
  if (txTimeoutTimer) { clearTimeout(txTimeoutTimer); txTimeoutTimer = null; }
  if (txTimeoutInterval) { clearInterval(txTimeoutInterval); txTimeoutInterval = null; }
  txTimeoutRemaining = 0;
}

function startRxAudio() {
  if (rxActive) { stopRxAudio(); return; }
  if (!hasWebCodecs) {
    audioStatus.textContent = "Audio requires Chrome/Edge";
    return;
  }
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  audioWs = new WebSocket(`${proto}//${location.host}/audio`);
  audioWs.binaryType = "arraybuffer";
  audioStatus.textContent = "Connecting…";

  audioWs.onopen = () => {
    audioStatus.textContent = "Connected";
  };

  audioWs.onmessage = (evt) => {
    if (typeof evt.data === "string") {
      // Stream info JSON
      try {
        streamInfo = JSON.parse(evt.data);
        audioCtx = new AudioContext({ sampleRate: streamInfo.sample_rate || 48000 });
        rxActive = true;
        rxAudioBtn.style.borderColor = "#00d17f";
        rxAudioBtn.style.color = "#00d17f";
        audioStatus.textContent = "RX";
      } catch (e) {
        console.error("Audio stream info parse error", e);
      }
      return;
    }

    // Binary Opus data — decode via WebCodecs AudioDecoder if available
    if (!audioCtx) return;
    const data = new Uint8Array(evt.data);

    // Throttle level indicator updates to max 10/sec
    const now = Date.now();
    if (now - lastLevelUpdate >= 100) {
      const level = Math.min(100, (data.length / 120) * 100);
      audioLevelFill.style.width = `${level}%`;
      lastLevelUpdate = now;
    }

    // Use WebCodecs AudioDecoder for Opus if available
    if (typeof AudioDecoder !== "undefined" && !opusDecoder) {
      try {
        const channels = (streamInfo && streamInfo.channels) || 1;
        const sampleRate = (streamInfo && streamInfo.sample_rate) || 48000;
        opusDecoder = new AudioDecoder({
          output: (frame) => {
            const buf = new Float32Array(frame.numberOfFrames * frame.numberOfChannels);
            frame.copyTo(buf, { planeIndex: 0 });
            const ab = audioCtx.createBuffer(frame.numberOfChannels, frame.numberOfFrames, frame.sampleRate);
            for (let ch = 0; ch < frame.numberOfChannels; ch++) {
              const chData = new Float32Array(frame.numberOfFrames);
              for (let i = 0; i < frame.numberOfFrames; i++) {
                chData[i] = buf[i * frame.numberOfChannels + ch];
              }
              ab.copyToChannel(chData, ch);
            }
            const src = audioCtx.createBufferSource();
            src.buffer = ab;
            src.connect(audioCtx.destination);
            const now = audioCtx.currentTime;
            const schedTime = Math.max(now, (nextPlayTime || now));
            src.start(schedTime);
            nextPlayTime = schedTime + ab.duration;
            frame.close();
          },
          error: (e) => { console.error("AudioDecoder error", e); }
        });
        opusDecoder.configure({
          codec: "opus",
          sampleRate: sampleRate,
          numberOfChannels: channels,
        });
      } catch (e) {
        console.warn("WebCodecs AudioDecoder not available for Opus", e);
        opusDecoder = null;
      }
    }
    if (opusDecoder) {
      try {
        opusDecoder.decode(new EncodedAudioChunk({
          type: "key",
          timestamp: performance.now() * 1000,
          data: data,
        }));
      } catch (e) {
        // Ignore decode errors for individual frames
      }
    }
  };

  audioWs.onclose = () => {
    // If TX was active when WS closed, release PTT
    if (txActive) { stopTxAudio(); }
    rxActive = false;
    rxAudioBtn.style.borderColor = "";
    rxAudioBtn.style.color = "";
    audioStatus.textContent = "Off";
    audioLevelFill.style.width = "0%";
    if (opusDecoder) {
      try { opusDecoder.close(); } catch(e) {}
      opusDecoder = null;
    }
    nextPlayTime = 0;
  };

  audioWs.onerror = () => {
    audioStatus.textContent = "Error";
  };
}

function stopRxAudio() {
  rxActive = false;
  if (audioWs) { audioWs.close(); audioWs = null; }
  if (audioCtx) { audioCtx.close(); audioCtx = null; }
  if (opusDecoder) {
    try { opusDecoder.close(); } catch(e) {}
    opusDecoder = null;
  }
  nextPlayTime = 0;
  rxAudioBtn.style.borderColor = "";
  rxAudioBtn.style.color = "";
  audioStatus.textContent = "Off";
  audioLevelFill.style.width = "0%";
}

function startTxAudio() {
  if (txActive) { stopTxAudio(); return; }
  if (!hasWebCodecs) {
    audioStatus.textContent = "Audio requires Chrome/Edge";
    return;
  }
  if (!audioWs || audioWs.readyState !== WebSocket.OPEN) {
    audioStatus.textContent = "RX first";
    return;
  }
  if (!streamInfo) return;

  navigator.mediaDevices.getUserMedia({
    audio: { sampleRate: streamInfo.sample_rate || 48000, channelCount: streamInfo.channels || 1 }
  }).then(async (stream) => {
    txStream = stream;
    txActive = true;
    txAudioBtn.style.borderColor = "#e55353";
    txAudioBtn.style.color = "#e55353";
    audioStatus.textContent = "RX+TX";

    // Start PTT safety timeout
    resetTxTimeout();
    startTxTimeoutCountdown();

    // Engage PTT automatically
    try { await postPath("/set_ptt?ptt=true"); } catch (e) { console.error("PTT on failed", e); }

    const sampleRate = streamInfo.sample_rate || 48000;
    const channels = streamInfo.channels || 1;
    const encoder = new AudioEncoder({
      output: (chunk) => {
        const buf = new ArrayBuffer(chunk.byteLength);
        chunk.copyTo(buf);
        if (audioWs && audioWs.readyState === WebSocket.OPEN) {
          audioWs.send(buf);
        }
      },
      error: (e) => { console.error("AudioEncoder error", e); }
    });
    encoder.configure({
      codec: "opus",
      sampleRate: sampleRate,
      numberOfChannels: channels,
      bitrate: (streamInfo.bitrate_bps || 24000),
    });
    txEncoder = encoder;

    // Use AudioWorklet or ScriptProcessor to feed encoder
    if (!audioCtx) audioCtx = new AudioContext({ sampleRate: sampleRate });
    const source = audioCtx.createMediaStreamSource(stream);
    const frameDuration = (streamInfo.frame_duration_ms || 20) / 1000;
    const frameSize = Math.floor(sampleRate * frameDuration);
    // Use ScriptProcessorNode (deprecated but widely supported)
    const processor = audioCtx.createScriptProcessor(frameSize, channels, channels);
    let tsCounter = 0;
    processor.onaudioprocess = (e) => {
      if (!txActive || !txEncoder) return;
      const input = e.inputBuffer;
      // Reset PTT safety timeout on each audio callback
      resetTxTimeout();
      // Use mono (channel 0) for f32-planar format
      const monoData = input.getChannelData(0);
      try {
        const frame = new AudioData({
          format: "f32-planar",
          sampleRate: input.sampleRate,
          numberOfFrames: input.length,
          numberOfChannels: 1,
          timestamp: tsCounter,
          data: monoData,
        });
        tsCounter += (input.length / input.sampleRate) * 1_000_000;
        txEncoder.encode(frame);
        frame.close();
      } catch (e) {
        // Ignore
      }
    };
    source.connect(processor);
    processor.connect(audioCtx.destination);
    txProcessor = { source, processor };
  }).catch((err) => {
    console.error("getUserMedia failed:", err);
    audioStatus.textContent = "Mic denied";
  });
}

async function stopTxAudio() {
  if (!txActive) return;
  txActive = false;
  clearTxTimeout();

  // Release PTT automatically
  try { await postPath("/set_ptt?ptt=false"); } catch (e) { console.error("PTT off failed", e); }

  if (txStream) {
    txStream.getTracks().forEach(t => t.stop());
    txStream = null;
  }
  if (txProcessor) {
    txProcessor.source.disconnect();
    txProcessor.processor.disconnect();
    txProcessor = null;
  }
  if (txEncoder) {
    try { txEncoder.close(); } catch(e) {}
    txEncoder = null;
  }
  txAudioBtn.style.borderColor = "";
  txAudioBtn.style.color = "";
  audioStatus.textContent = rxActive ? "RX" : "Off";
}

rxAudioBtn.addEventListener("click", startRxAudio);
txAudioBtn.addEventListener("click", startTxAudio);

// Release PTT on page unload to prevent stuck transmit
window.addEventListener("beforeunload", () => {
  if (txActive) {
    navigator.sendBeacon("/set_ptt?ptt=false", "");
  }
});
