const freqEl = document.getElementById("freq");
const modeEl = document.getElementById("mode");
const bandLabel = document.getElementById("band-label");
const powerBtn = document.getElementById("power-btn");
const powerHint = document.getElementById("power-hint");
const vfoPicker = document.getElementById("vfo-picker");
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
const serverSubtitle = document.getElementById("server-subtitle");
const loadingTitle = document.getElementById("loading-title");
const loadingSub = document.getElementById("loading-sub");

let lastControl;
let lastTxEn = null;
let lastRendered = null;
let rigName = "Rig";
let hintTimer = null;
let sigMeasuring = false;
let sigSamples = [];
let lastFreqHz = null;
let jogStep = 1000; // default 1 kHz
let jogAngle = 0;
let lastClientCount = null;
let lastLocked = false;
const originalTitle = document.title;

function readyText() {
  return lastClientCount !== null ? `Ready \u00b7 ${lastClientCount} user${lastClientCount !== 1 ? "s" : ""}` : "Ready";
}

function showHint(msg, duration) {
  powerHint.textContent = msg;
  if (hintTimer) clearTimeout(hintTimer);
  if (duration) hintTimer = setTimeout(() => { powerHint.textContent = readyText(); }, duration);
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

// Convert dBm (wire format) to S-units (S1=-121dBm, S9=-73dBm, 6dB/S-unit).
// Above S9, returns 9 + (overshoot in S-unit-equivalent, i.e. dB/10).
function dbmToSUnits(dbm) {
  if (dbm <= -121) return 0;
  if (dbm >= -73) return 9 + (dbm + 73) / 10;
  return (dbm + 121) / 6;
}

function formatSignal(sUnits) {
  if (sUnits <= 9) return `S${sUnits.toFixed(1)}`;
  const overDb = (sUnits - 9) * 10;
  return `S9 + ${overDb.toFixed(0)}dB`;
}

function setDisabled(disabled) {
  [freqEl, modeEl, freqBtn, modeBtn, pttBtn, powerBtn, txLimitInput, txLimitBtn, lockBtn].forEach((el) => {
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
  // Server subtitle: "trx-server vX.Y.Z hosted by CALL"
  if (update.server_version || update.server_callsign) {
    let parts = "trx-server";
    if (update.server_version) parts += ` v${update.server_version}`;
    if (update.server_callsign) {
      const cs = update.server_callsign;
      serverSubtitle.innerHTML = `${parts} hosted by <a href="https://www.qrzcq.com/call/${encodeURIComponent(cs)}" target="_blank" rel="noopener">${cs}</a>`;
      document.title = `${cs} - ${originalTitle}`;
    } else {
      serverSubtitle.textContent = parts;
    }
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
  if (update.status && update.status.freq && typeof update.status.freq.hz === "number") {
    lastFreqHz = update.status.freq.hz;
    if (!freqDirty) {
      freqEl.value = formatFreq(update.status.freq.hz);
    }
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
    vfoPicker.innerHTML = "";
    entries.forEach((entry, idx) => {
      const hz = entry && entry.freq && typeof entry.freq.hz === "number" ? entry.freq.hz : null;
      if (hz === null) return;
      const mode = entry.mode ? normalizeMode(entry.mode) : "";
      const modeText = mode ? ` [${mode}]` : "";
      const label = `${entry.name || String.fromCharCode(65 + idx)}: ${formatFreq(hz)}${modeText}`;
      const btn = document.createElement("button");
      btn.type = "button";
      btn.textContent = label;
      if (activeIdx === idx) btn.classList.add("active");
      else btn.addEventListener("click", async () => {
        btn.disabled = true;
        showHint("Toggling VFO…");
        try {
          await postPath("/toggle_vfo");
          showHint("VFO toggled", 1200);
        } catch (err) {
          showHint("VFO toggle failed", 2000);
          console.error(err);
        } finally {
          btn.disabled = false;
        }
      });
      vfoPicker.appendChild(btn);
    });
  } else {
    vfoPicker.innerHTML = "<button type=\"button\" class=\"active\">--</button>";
  }
  if (update.status && update.status.rx && typeof update.status.rx.sig === "number") {
    const sUnits = dbmToSUnits(update.status.rx.sig);
    const pct = sUnits <= 9 ? Math.max(0, Math.min(100, (sUnits / 9) * 100)) : 100;
    signalBar.style.width = `${pct}%`;
    signalValue.textContent = formatSignal(sUnits);
    if (sigMeasuring) {
      sigSamples.push(sUnits);
      sigMeasureBtn.textContent = `Stop (${sigSamples.length})`;
    }
  } else {
    signalBar.style.width = "0%";
    signalValue.textContent = "--";
  }
  bandLabel.textContent = typeof update.band === "string" ? update.band : "--";
  if (typeof update.enabled === "boolean") {
    powerBtn.disabled = false;
    powerBtn.textContent = update.enabled ? "Power Off" : "Power On";
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

  if (typeof update.clients === "number") lastClientCount = update.clients;
  // Populate About tab
  if (update.server_version) {
    document.getElementById("about-server-ver").textContent = `trx-server v${update.server_version}`;
  }
  document.getElementById("about-server-addr").textContent = location.host;
  if (update.server_callsign) {
    document.getElementById("about-server-call").textContent = update.server_callsign;
  }
  if (update.info) {
    const parts = [update.info.manufacturer, update.info.model, update.info.revision].filter(Boolean).join(" ");
    if (parts) document.getElementById("about-rig-info").textContent = parts;
    const access = update.info.access;
    if (access) {
      if (access.Serial) {
        document.getElementById("about-rig-access").textContent = `Serial (${access.Serial.port || "?"}, ${access.Serial.baud || "?"} baud)`;
      } else if (access.Tcp) {
        document.getElementById("about-rig-access").textContent = `TCP (${access.Tcp.host || "?"}:${access.Tcp.port || "?"})`;
      } else {
        const key = Object.keys(access)[0];
        if (key) document.getElementById("about-rig-access").textContent = key;
      }
    }
    if (update.info.capabilities) {
      const cap = update.info.capabilities;
      if (Array.isArray(cap.supported_modes) && cap.supported_modes.length) {
        document.getElementById("about-modes").textContent = cap.supported_modes.map(normalizeMode).filter(Boolean).join(", ");
      }
      if (typeof cap.num_vfos === "number") {
        document.getElementById("about-vfos").textContent = cap.num_vfos;
      }
    }
  }
  if (typeof update.clients === "number") {
    document.getElementById("about-clients").textContent = update.clients;
  }
  powerHint.textContent = readyText();
  lastLocked = update.status && update.status.lock === true;
  lockBtn.textContent = lastLocked ? "Unlock" : "Lock";

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
        powerHint.textContent = readyText();
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

// --- Jog wheel ---
const jogWheel = document.getElementById("jog-wheel");
const jogIndicator = document.getElementById("jog-indicator");
const jogDownBtn = document.getElementById("jog-down");
const jogUpBtn = document.getElementById("jog-up");
const jogStepEl = document.getElementById("jog-step");

async function jogFreq(direction) {
  if (lastLocked) { showHint("Locked", 1500); return; }
  if (lastFreqHz === null) return;
  const newHz = lastFreqHz + direction * jogStep;
  if (!freqAllowed(newHz)) {
    showHint("Out of supported bands", 1500);
    return;
  }
  jogAngle = (jogAngle + direction * 15) % 360;
  jogIndicator.style.transform = `translateX(-50%) rotate(${jogAngle}deg)`;
  showHint("Setting frequency…");
  try {
    await postPath(`/set_freq?hz=${newHz}`);
    showHint("Freq set", 1000);
  } catch (err) {
    showHint("Set freq failed", 2000);
    console.error(err);
  }
}

jogDownBtn.addEventListener("click", () => jogFreq(-1));
jogUpBtn.addEventListener("click", () => jogFreq(1));

jogWheel.addEventListener("wheel", (e) => {
  e.preventDefault();
  const direction = e.deltaY < 0 ? 1 : -1;
  jogFreq(direction);
}, { passive: false });

// Touch drag on jog wheel
let jogTouchY = null;
jogWheel.addEventListener("touchstart", (e) => {
  e.preventDefault();
  jogTouchY = e.touches[0].clientY;
}, { passive: false });
jogWheel.addEventListener("touchmove", (e) => {
  e.preventDefault();
  if (jogTouchY === null) return;
  const dy = jogTouchY - e.touches[0].clientY;
  if (Math.abs(dy) > 12) {
    jogFreq(dy > 0 ? 1 : -1);
    jogTouchY = e.touches[0].clientY;
  }
}, { passive: false });
jogWheel.addEventListener("touchend", () => { jogTouchY = null; });

// Mouse drag on jog wheel
let jogMouseY = null;
jogWheel.addEventListener("mousedown", (e) => {
  e.preventDefault();
  jogMouseY = e.clientY;
  jogWheel.style.cursor = "grabbing";
});
window.addEventListener("mousemove", (e) => {
  if (jogMouseY === null) return;
  const dy = jogMouseY - e.clientY;
  if (Math.abs(dy) > 10) {
    jogFreq(dy > 0 ? 1 : -1);
    jogMouseY = e.clientY;
  }
});
window.addEventListener("mouseup", () => {
  jogMouseY = null;
  if (jogWheel) jogWheel.style.cursor = "grab";
});

// Step selector
jogStepEl.addEventListener("click", (e) => {
  const btn = e.target.closest("button[data-step]");
  if (!btn) return;
  jogStep = parseInt(btn.dataset.step, 10);
  jogStepEl.querySelectorAll("button").forEach((b) => b.classList.remove("active"));
  btn.classList.add("active");
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

txLimitInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    txLimitBtn.click();
  }
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

// --- Tab navigation ---
document.querySelector(".tab-bar").addEventListener("click", (e) => {
  const btn = e.target.closest(".tab[data-tab]");
  if (!btn) return;
  document.querySelectorAll(".tab-bar .tab").forEach((t) => t.classList.remove("active"));
  btn.classList.add("active");
  document.querySelectorAll(".tab-panel").forEach((p) => p.style.display = "none");
  document.getElementById(`tab-${btn.dataset.tab}`).style.display = "";
});

connect();

// --- Sub-tab navigation (Plugins tab) ---
document.querySelectorAll(".sub-tab-bar").forEach((bar) => {
  bar.addEventListener("click", (e) => {
    const btn = e.target.closest(".sub-tab[data-subtab]");
    if (!btn) return;
    bar.querySelectorAll(".sub-tab").forEach((t) => t.classList.remove("active"));
    btn.classList.add("active");
    const parent = bar.parentElement;
    parent.querySelectorAll(".sub-tab-panel").forEach((p) => p.style.display = "none");
    parent.querySelector(`#subtab-${btn.dataset.subtab}`).style.display = "";
  });
});

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
function createDemodulator(sampleRate) {
  const BAUD = 1200;
  const MARK = 1200;
  const SPACE = 2200;
  const samplesPerBit = sampleRate / BAUD;
  const windowLen = Math.round(samplesPerBit);

  // Correlation buffers
  let markI = 0, markQ = 0, spaceI = 0, spaceQ = 0;
  const ringLen = windowLen;
  const ringMarkI = new Float32Array(ringLen);
  const ringMarkQ = new Float32Array(ringLen);
  const ringSpaceI = new Float32Array(ringLen);
  const ringSpaceQ = new Float32Array(ringLen);
  let ringIdx = 0;

  // Clock recovery
  let sampleCount = 0;
  let lastBit = 0;
  let bitPhase = 0;

  // HDLC state
  let ones = 0;
  let frameBits = [];
  let inFrame = false;
  let shiftReg = 0;
  let bitCount = 0;

  const frames = [];

  function processSample(s) {
    const t = sampleCount / sampleRate;
    const mI = s * Math.cos(2 * Math.PI * MARK * t);
    const mQ = s * Math.sin(2 * Math.PI * MARK * t);
    const sI = s * Math.cos(2 * Math.PI * SPACE * t);
    const sQ = s * Math.sin(2 * Math.PI * SPACE * t);

    // Sliding window correlation
    markI += mI - ringMarkI[ringIdx];
    markQ += mQ - ringMarkQ[ringIdx];
    spaceI += sI - ringSpaceI[ringIdx];
    spaceQ += sQ - ringSpaceQ[ringIdx];

    ringMarkI[ringIdx] = mI;
    ringMarkQ[ringIdx] = mQ;
    ringSpaceI[ringIdx] = sI;
    ringSpaceQ[ringIdx] = sQ;
    ringIdx = (ringIdx + 1) % ringLen;

    const markEnergy = markI * markI + markQ * markQ;
    const spaceEnergy = spaceI * spaceI + spaceQ * spaceQ;
    const bit = markEnergy > spaceEnergy ? 1 : 0;

    // Clock recovery via zero-crossing
    if (bit !== lastBit) {
      lastBit = bit;
      // Nudge phase toward center of bit
      bitPhase = samplesPerBit / 2;
    }

    bitPhase--;
    if (bitPhase <= 0) {
      bitPhase += samplesPerBit;
      processBit(bit);
    }

    sampleCount++;
  }

  function processBit(rawBit) {
    // NRZI decode: no transition = 1, transition = 0
    // We track previous raw bit; same = 1, different = 0
    const nrziBit = (rawBit === lastBit) ? 1 : 0;

    // Check for flag (0x7E = 01111110 in bit order)
    shiftReg = ((shiftReg >> 1) | (rawBit << 7)) & 0xFF;
    bitCount++;

    if (nrziBit === 1) {
      ones++;
    } else {
      if (ones === 5) {
        // Bit stuffing — skip this zero
        ones = 0;
        return;
      }
      ones = 0;
    }

    if (shiftReg === 0x7E) {
      // Flag detected
      if (inFrame && frameBits.length >= 136) {
        // Minimum AX.25 frame: 14 addr + 1 ctrl + 1 pid + 1 info + 2 fcs = 19 bytes = 152 bits
        // But we check >= 136 bits (17 bytes) to be lenient
        const frame = bitsToBytes(frameBits);
        if (frame) frames.push(frame);
      }
      frameBits = [];
      inFrame = true;
      ones = 0;
      return;
    }

    if (inFrame) {
      frameBits.push(nrziBit);
    }
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
    if (computed !== fcs) return null;

    return payload;
  }

  function processBuffer(samples) {
    for (let i = 0; i < samples.length; i++) {
      processSample(samples[i]);
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
  const row = document.createElement("div");
  row.className = "aprs-packet";
  const now = new Date();
  const ts = now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  row.innerHTML = `<span class="aprs-time">${ts}</span><span class="aprs-call">${pkt.srcCall}</span>&gt;${pkt.destCall}${pkt.path ? "," + pkt.path : ""}: <span title="${pkt.type}">${pkt.info}</span>`;
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

        aprsDecoder = new AudioDecoder({
          output: (frame) => {
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
            for (const f of frames) {
              const ax25 = parseAX25(f);
              if (!ax25) continue;
              const pkt = parseAPRS(ax25);
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

// --- Signal measurement ---
const sigMeasureBtn = document.getElementById("sig-measure-btn");
const sigClearBtn = document.getElementById("sig-clear-btn");
const sigResult = document.getElementById("sig-result");

sigMeasureBtn.addEventListener("click", () => {
  if (!sigMeasuring) {
    sigSamples = [];
    sigMeasuring = true;
    sigMeasureBtn.textContent = "Stop (0)";
    sigMeasureBtn.style.borderColor = "#00d17f";
    sigMeasureBtn.style.color = "#00d17f";
  } else {
    sigMeasuring = false;
    sigMeasureBtn.textContent = "Measure";
    sigMeasureBtn.style.borderColor = "";
    sigMeasureBtn.style.color = "";
    if (sigSamples.length > 0) {
      const avg = sigSamples.reduce((a, b) => a + b, 0) / sigSamples.length;
      const peak = Math.max(...sigSamples);
      sigResult.textContent = `Avg ${formatSignal(avg)} / Peak ${formatSignal(peak)} (${sigSamples.length} samples)`;
    }
  }
});

sigClearBtn.addEventListener("click", () => {
  sigResult.textContent = "";
});

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
let rxGainNode = null;
let txGainNode = null;
const rxVolSlider = document.getElementById("rx-vol");
const txVolSlider = document.getElementById("tx-vol");
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
        rxGainNode = audioCtx.createGain();
        rxGainNode.gain.value = rxVolSlider.value / 100;
        rxGainNode.connect(audioCtx.destination);
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
            src.connect(rxGainNode);
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
    rxGainNode = null;
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
  rxGainNode = null;
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
    txGainNode = audioCtx.createGain();
    txGainNode.gain.value = txVolSlider.value / 100;
    source.connect(txGainNode);
    txGainNode.connect(processor);
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
  txGainNode = null;
  txAudioBtn.style.borderColor = "";
  txAudioBtn.style.color = "";
  audioStatus.textContent = rxActive ? "RX" : "Off";
}

rxAudioBtn.addEventListener("click", startRxAudio);
txAudioBtn.addEventListener("click", startTxAudio);

const rxVolPct = document.getElementById("rx-vol-pct");
const txVolPct = document.getElementById("tx-vol-pct");

function updateVolSlider(slider, pctEl, gainNode) {
  pctEl.textContent = `${slider.value}%`;
  if (gainNode) gainNode.gain.value = slider.value / 100;
}

rxVolSlider.addEventListener("input", () => updateVolSlider(rxVolSlider, rxVolPct, rxGainNode));
txVolSlider.addEventListener("input", () => updateVolSlider(txVolSlider, txVolPct, txGainNode));

function volWheel(slider, pctEl, getGain) {
  slider.addEventListener("wheel", (e) => {
    e.preventDefault();
    const step = e.deltaY < 0 ? 2 : -2;
    slider.value = Math.max(0, Math.min(100, Number(slider.value) + step));
    updateVolSlider(slider, pctEl, getGain());
  }, { passive: false });
}
volWheel(rxVolSlider, rxVolPct, () => rxGainNode);
volWheel(txVolSlider, txVolPct, () => txGainNode);

document.getElementById("copyright-year").textContent = new Date().getFullYear();

// Release PTT on page unload to prevent stuck transmit
window.addEventListener("beforeunload", () => {
  if (txActive) {
    navigator.sendBeacon("/set_ptt?ptt=false", "");
  }
});
