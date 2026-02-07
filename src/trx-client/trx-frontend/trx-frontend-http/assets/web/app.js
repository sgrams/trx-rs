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
    pttBtn.style.background = update.status.tx_en ? "#ffefef" : "#f3f3f3";
    pttBtn.style.borderColor = update.status.tx_en ? "#d22" : "#999";
    pttBtn.style.color = update.status.tx_en ? "#a00" : "#222";
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
  powerHint.textContent = "Sending...";
  try {
    await postPath("/toggle_power");
    powerHint.textContent = "Toggled, waiting for update…";
  } catch (err) {
    powerHint.textContent = "Toggle failed";
    console.error(err);
    setTimeout(() => powerHint.textContent = "Ready", 2000);
  } finally {
    powerBtn.disabled = false;
  }
});

vfoBtn.addEventListener("click", async () => {
  vfoBtn.disabled = true;
  powerHint.textContent = "Toggling VFO…";
  try {
    await postPath("/toggle_vfo");
    powerHint.textContent = "VFO toggled, waiting for update…";
    setTimeout(() => {
      if (powerHint.textContent.includes("VFO toggled")) {
        powerHint.textContent = "Ready";
      }
    }, 1200);
  } catch (err) {
    powerHint.textContent = "VFO toggle failed";
    console.error(err);
    setTimeout(() => powerHint.textContent = "Ready", 2000);
  } finally {
    vfoBtn.disabled = false;
  }
});

pttBtn.addEventListener("click", async () => {
  pttBtn.disabled = true;
  powerHint.textContent = "Toggling PTT…";
  try {
    const desired = lastTxEn ? "false" : "true";
    await postPath(`/set_ptt?ptt=${desired}`);
    powerHint.textContent = "PTT command sent";
  } catch (err) {
    powerHint.textContent = "PTT toggle failed";
    console.error(err);
    setTimeout(() => powerHint.textContent = "Ready", 2000);
  } finally {
    pttBtn.disabled = false;
  }
});

freqBtn.addEventListener("click", async () => {
  const parsed = parseFreqInput(freqEl.value);
  if (parsed === null) {
    powerHint.textContent = "Freq missing";
    return;
  }
  if (!freqAllowed(parsed)) {
    powerHint.textContent = "Out of supported bands";
    setTimeout(() => powerHint.textContent = "Ready", 1500);
    return;
  }
  freqDirty = false;
  freqBtn.disabled = true;
  powerHint.textContent = "Setting frequency…";
  try {
    await postPath(`/set_freq?hz=${parsed}`);
    powerHint.textContent = "Freq set";
  } catch (err) {
    powerHint.textContent = "Set freq failed";
    console.error(err);
    setTimeout(() => powerHint.textContent = "Ready", 2000);
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
    powerHint.textContent = "Mode missing";
    return;
  }
  modeDirty = false;
  modeBtn.disabled = true;
  powerHint.textContent = "Setting mode…";
  try {
    await postPath(`/set_mode?mode=${encodeURIComponent(mode)}`);
    powerHint.textContent = "Mode set";
  } catch (err) {
    powerHint.textContent = "Set mode failed";
    console.error(err);
    setTimeout(() => powerHint.textContent = "Ready", 2000);
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
    powerHint.textContent = "Limit missing";
    return;
  }
  txLimitBtn.disabled = true;
  powerHint.textContent = "Setting TX limit…";
  try {
    await postPath(`/set_tx_limit?limit=${encodeURIComponent(limit)}`);
    powerHint.textContent = "TX limit set";
  } catch (err) {
    powerHint.textContent = "TX limit failed";
    console.error(err);
    setTimeout(() => powerHint.textContent = "Ready", 2000);
  } finally {
    txLimitBtn.disabled = false;
  }
});

lockBtn.addEventListener("click", async () => {
  lockBtn.disabled = true;
  powerHint.textContent = "Toggling lock…";
  try {
    const nextLock = lockBtn.textContent === "Lock";
    await postPath(nextLock ? "/lock" : "/unlock");
    powerHint.textContent = "Lock toggled";
  } catch (err) {
    powerHint.textContent = "Lock toggle failed";
    console.error(err);
    setTimeout(() => powerHint.textContent = "Ready", 2000);
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

let audioWs = null;
let audioCtx = null;
let rxActive = false;
let txActive = false;
let txStream = null;
let txProcessor = null;
let streamInfo = null;

// Simple ring-buffer based audio player
let playBuffer = [];
let playNode = null;

function startRxAudio() {
  if (rxActive) { stopRxAudio(); return; }
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

    // Show level indicator from packet size (rough estimate)
    const level = Math.min(100, (data.length / 120) * 100);
    audioLevelFill.style.width = `${level}%`;

    // Use WebCodecs AudioDecoder for Opus if available
    if (typeof AudioDecoder !== "undefined" && !window._opusDecoder) {
      try {
        const channels = (streamInfo && streamInfo.channels) || 1;
        const sampleRate = (streamInfo && streamInfo.sample_rate) || 48000;
        window._opusDecoder = new AudioDecoder({
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
            const schedTime = Math.max(now, (window._nextPlayTime || now));
            src.start(schedTime);
            window._nextPlayTime = schedTime + ab.duration;
            frame.close();
          },
          error: (e) => { console.error("AudioDecoder error", e); }
        });
        window._opusDecoder.configure({
          codec: "opus",
          sampleRate: sampleRate,
          numberOfChannels: channels,
        });
      } catch (e) {
        console.warn("WebCodecs AudioDecoder not available for Opus", e);
        window._opusDecoder = null;
      }
    }
    if (window._opusDecoder) {
      try {
        window._opusDecoder.decode(new EncodedAudioChunk({
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
    if (window._opusDecoder) {
      try { window._opusDecoder.close(); } catch(e) {}
      window._opusDecoder = null;
    }
    window._nextPlayTime = 0;
  };

  audioWs.onerror = () => {
    audioStatus.textContent = "Error";
  };
}

function stopRxAudio() {
  rxActive = false;
  if (audioWs) { audioWs.close(); audioWs = null; }
  if (audioCtx) { audioCtx.close(); audioCtx = null; }
  if (window._opusDecoder) {
    try { window._opusDecoder.close(); } catch(e) {}
    window._opusDecoder = null;
  }
  window._nextPlayTime = 0;
  rxAudioBtn.style.borderColor = "";
  rxAudioBtn.style.color = "";
  audioStatus.textContent = "Off";
  audioLevelFill.style.width = "0%";
}

function startTxAudio() {
  if (txActive) { stopTxAudio(); return; }
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

    // Engage PTT automatically
    try { await postPath("/set_ptt?ptt=true"); } catch (e) { console.error("PTT on failed", e); }

    // If WebCodecs AudioEncoder is available, use it for Opus encoding
    if (typeof AudioEncoder !== "undefined") {
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
      window._txEncoder = encoder;

      // Use AudioWorklet or ScriptProcessor to feed encoder
      if (!audioCtx) audioCtx = new AudioContext({ sampleRate: sampleRate });
      const source = audioCtx.createMediaStreamSource(stream);
      const frameDuration = (streamInfo.frame_duration_ms || 20) / 1000;
      const frameSize = Math.floor(sampleRate * frameDuration);
      // Use ScriptProcessorNode (deprecated but widely supported)
      const processor = audioCtx.createScriptProcessor(frameSize, channels, channels);
      let tsCounter = 0;
      processor.onaudioprocess = (e) => {
        if (!txActive || !window._txEncoder) return;
        const input = e.inputBuffer;
        const data = new Float32Array(input.length * input.numberOfChannels);
        for (let ch = 0; ch < input.numberOfChannels; ch++) {
          const chData = input.getChannelData(ch);
          for (let i = 0; i < input.length; i++) {
            data[i * input.numberOfChannels + ch] = chData[i];
          }
        }
        try {
          const frame = new AudioData({
            format: "f32-planar",
            sampleRate: input.sampleRate,
            numberOfFrames: input.length,
            numberOfChannels: input.numberOfChannels,
            timestamp: tsCounter,
            data: input.getChannelData(0),
          });
          tsCounter += (input.length / input.sampleRate) * 1_000_000;
          window._txEncoder.encode(frame);
          frame.close();
        } catch (e) {
          // Ignore
        }
      };
      source.connect(processor);
      processor.connect(audioCtx.destination);
      txProcessor = { source, processor };
    }
  }).catch((err) => {
    console.error("getUserMedia failed:", err);
    audioStatus.textContent = "Mic denied";
  });
}

async function stopTxAudio() {
  if (!txActive) return;
  txActive = false;

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
  if (window._txEncoder) {
    try { window._txEncoder.close(); } catch(e) {}
    window._txEncoder = null;
  }
  txAudioBtn.style.borderColor = "";
  txAudioBtn.style.color = "";
  audioStatus.textContent = rxActive ? "RX" : "Off";
}

rxAudioBtn.addEventListener("click", startRxAudio);
txAudioBtn.addEventListener("click", startTxAudio);
