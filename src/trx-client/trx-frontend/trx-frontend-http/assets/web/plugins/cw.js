// --- CW (Morse) Decoder Plugin (server-side decode) ---
const cwStatusEl = document.getElementById("cw-status");
const cwOutputEl = document.getElementById("cw-output");
const cwAutoInput = document.getElementById("cw-auto");
const cwWpmInput = document.getElementById("cw-wpm");
const cwToneInput = document.getElementById("cw-tone");
const cwSignalIndicator = document.getElementById("cw-signal-indicator");
const cwToneCanvas = document.getElementById("cw-tone-waterfall");
const cwTonePickerEl = document.querySelector(".cw-tone-picker");
const cwToneRangeEl = document.getElementById("cw-tone-range");
const CW_MAX_LINES = 200;
const CW_TONE_MIN_HZ = 300;
const CW_TONE_MAX_HZ = 1200;
const CW_WPM_MIN = 5;
const CW_WPM_MAX = 40;
let cwLastAppendTime = 0;
let cwTonePickerRaf = null;

function applyCwAutoUi(enabled) {
  if (cwAutoInput) cwAutoInput.checked = enabled;
  if (cwWpmInput) {
    cwWpmInput.disabled = enabled;
    cwWpmInput.readOnly = enabled;
  }
  if (cwToneInput) {
    cwToneInput.disabled = enabled;
    cwToneInput.readOnly = enabled;
  }
  if (cwTonePickerEl) {
    cwTonePickerEl.classList.toggle("is-auto", enabled);
  }
}
window.applyCwAutoUi = applyCwAutoUi;

function clampCwWpm(wpm) {
  const numeric = Number(wpm);
  if (!Number.isFinite(numeric)) return 15;
  return Math.round(Math.max(CW_WPM_MIN, Math.min(CW_WPM_MAX, numeric)));
}

function clampCwTone(tone) {
  const numeric = Number(tone);
  if (!Number.isFinite(numeric)) return 700;
  return Math.round(Math.max(CW_TONE_MIN_HZ, Math.min(CW_TONE_MAX_HZ, numeric)));
}

function currentCwToneRange() {
  const centerHz = Number.isFinite(window.lastFreqHz) ? Number(window.lastFreqHz) : NaN;
  const bandwidthHz = Number.isFinite(window.currentBandwidthHz) ? Number(window.currentBandwidthHz) : NaN;
  if (!Number.isFinite(centerHz) || !Number.isFinite(bandwidthHz) || bandwidthHz <= 0) {
    return null;
  }
  const mode = String(document.getElementById("mode")?.value || "").toUpperCase();
  const lowerSideband = mode === "CWR";
  const upperSideband = mode === "CW";
  if (!lowerSideband && !upperSideband) return null;
  const lowHz = lowerSideband ? centerHz - bandwidthHz : centerHz;
  const highHz = lowerSideband ? centerHz : centerHz + bandwidthHz;
  const toneMinHz = CW_TONE_MIN_HZ;
  const toneMaxHz = Math.min(CW_TONE_MAX_HZ, Math.round(bandwidthHz));
  if (toneMaxHz < toneMinHz) {
    return null;
  }
  return {
    lowHz,
    highHz,
    centerHz,
    bandwidthHz,
    toneMinHz,
    toneMaxHz,
    lowerSideband,
    mode,
  };
}

function toneClampForRange(tone, range) {
  const clamped = clampCwTone(tone);
  if (!range) return clamped;
  return Math.max(range.toneMinHz, Math.min(range.toneMaxHz, clamped));
}

function ensureCwToneCanvasResolution() {
  if (!cwToneCanvas) return false;
  const rect = cwToneCanvas.getBoundingClientRect();
  const cssWidth = Math.max(1, Math.round(rect.width));
  const cssHeight = Math.max(1, Math.round(rect.height));
  const dpr = window.devicePixelRatio || 1;
  const nextWidth = Math.max(1, Math.round(cssWidth * dpr));
  const nextHeight = Math.max(1, Math.round(cssHeight * dpr));
  if (cwToneCanvas.width !== nextWidth || cwToneCanvas.height !== nextHeight) {
    cwToneCanvas.width = nextWidth;
    cwToneCanvas.height = nextHeight;
    return true;
  }
  return false;
}

function drawCwTonePicker() {
  if (!cwToneCanvas) return;
  const ctx = cwToneCanvas.getContext("2d");
  if (!ctx) return;

  const width = cwToneCanvas.width;
  const height = cwToneCanvas.height;
  ctx.clearRect(0, 0, width, height);

  const range = currentCwToneRange();
  if (!range || !window.lastSpectrumData || !Array.isArray(window.lastSpectrumData.bins) || !window.lastSpectrumData.bins.length) {
    if (cwToneRangeEl) {
      const mode = String(document.getElementById("mode")?.value || "").toUpperCase();
      if (mode !== "CW" && mode !== "CWR") {
        cwToneRangeEl.textContent = "CW/CWR mode required";
      } else {
        cwToneRangeEl.textContent = "Waiting for spectrum";
      }
    }
    ctx.fillStyle = "rgba(130, 150, 165, 0.22)";
    ctx.fillRect(0, 0, width, height);
    return;
  }

  if (cwToneRangeEl) {
    const side = range.lowerSideband ? "Lower side" : "Upper side";
    cwToneRangeEl.textContent = `${side} · Tone ${range.toneMinHz}-${range.toneMaxHz} Hz`;
  }

  const bins = window.lastSpectrumData.bins;
  const sampleRate = Number(window.lastSpectrumData.sample_rate);
  const centerHz = Number(window.lastSpectrumData.center_hz);
  const maxIdx = Math.max(1, bins.length - 1);
  const fullLoHz = centerHz - sampleRate / 2;
  const tones = new Array(width).fill(0);
  let maxPower = 0;
  let minPower = Number.POSITIVE_INFINITY;
  for (let x = 0; x < width; x += 1) {
    const frac = width <= 1 ? 0 : x / (width - 1);
    const toneHz = range.lowHz + frac * (range.highHz - range.lowHz);
    const idx = Math.max(0, Math.min(maxIdx, Math.round((((toneHz - fullLoHz) / sampleRate) * maxIdx))));
    const power = Number.isFinite(Number(bins[idx])) ? Number(bins[idx]) : -140;
    tones[x] = power;
    if (power > maxPower) maxPower = power;
    if (power < minPower) minPower = power;
  }

  const powerSpan = Math.max(1, maxPower - minPower);
  for (let x = 0; x < width; x += 1) {
    const level = Math.max(0, Math.min(1, (tones[x] - minPower) / powerSpan));
    const hue = 200 - level * 155;
    const light = 14 + Math.pow(level, 0.75) * 58;
    ctx.fillStyle = `hsl(${hue} 85% ${light}%)`;
    ctx.fillRect(x, 0, 1, height);
  }

  const currentTone = toneClampForRange(cwToneInput ? cwToneInput.value : 700, range);
  const markerHz = range.lowerSideband
    ? range.centerHz - currentTone
    : range.centerHz + currentTone;
  const markerFrac = (markerHz - range.lowHz) / Math.max(1, (range.highHz - range.lowHz));
  const markerX = Math.max(0, Math.min(width - 1, Math.round(markerFrac * (width - 1))));
  ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
  ctx.fillRect(markerX, 0, 2, height);

  const lowLimitHz = range.lowerSideband
    ? range.centerHz - range.toneMaxHz
    : range.centerHz + range.toneMinHz;
  const highLimitHz = range.lowerSideband
    ? range.centerHz - range.toneMinHz
    : range.centerHz + range.toneMaxHz;
  const limitLowX = Math.max(0, Math.min(width - 1, Math.round(((lowLimitHz - range.lowHz) / Math.max(1, range.highHz - range.lowHz)) * (width - 1))));
  const limitHighX = Math.max(0, Math.min(width - 1, Math.round(((highLimitHz - range.lowHz) / Math.max(1, range.highHz - range.lowHz)) * (width - 1))));
  ctx.fillStyle = "rgba(255, 255, 255, 0.22)";
  ctx.fillRect(limitLowX, 0, 1, height);
  ctx.fillRect(limitHighX, 0, 1, height);

  if (cwAutoInput?.checked) {
    ctx.fillStyle = "rgba(0, 0, 0, 0.22)";
    ctx.fillRect(0, 0, width, height);
  }
}

async function setCwTone(tone, { syncInput = true } = {}) {
  const range = currentCwToneRange();
  const clamped = toneClampForRange(tone, range);
  if (cwToneInput && syncInput) {
    cwToneInput.value = clamped;
  }
  try {
    await postPath(`/set_cw_tone?tone_hz=${encodeURIComponent(clamped)}`);
  } catch (e) {
    console.error("CW tone set failed", e);
  }
  drawCwTonePicker();
}

if (cwAutoInput) {
  cwAutoInput.addEventListener("change", async () => {
    const enabled = cwAutoInput.checked;
    applyCwAutoUi(enabled);
    try {
      await postPath(`/set_cw_auto?enabled=${enabled ? "true" : "false"}`);
      drawCwTonePicker();
    } catch (e) {
      console.error("CW auto toggle failed", e);
    }
  });
}

if (cwWpmInput) {
  cwWpmInput.addEventListener("change", async () => {
    if (cwAutoInput && cwAutoInput.checked) return;
    const wpm = clampCwWpm(cwWpmInput.value);
    cwWpmInput.value = wpm;
    try { await postPath(`/set_cw_wpm?wpm=${encodeURIComponent(wpm)}`); }
    catch (e) { console.error("CW WPM set failed", e); }
  });
}

if (cwToneInput) {
  cwToneInput.addEventListener("change", async () => {
    if (cwAutoInput?.checked) return;
    await setCwTone(cwToneInput.value);
  });
}

if (cwToneCanvas) {
  cwToneCanvas.addEventListener("click", async (event) => {
    if (cwAutoInput?.checked) return;
    const rect = cwToneCanvas.getBoundingClientRect();
    if (rect.width <= 0) return;
    const range = currentCwToneRange();
    if (!range) return;
    const frac = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
    const rfHz = range.lowHz + frac * (range.highHz - range.lowHz);
    const signedOffsetHz = range.lowerSideband
      ? range.centerHz - rfHz
      : rfHz - range.centerHz;
    const tone = Math.max(range.toneMinHz, Math.min(range.toneMaxHz, signedOffsetHz));
    await setCwTone(tone);
  });
}

window.resetCwHistoryView = function() {
  if (cwOutputEl) cwOutputEl.innerHTML = "";
  cwLastAppendTime = 0;
  drawCwTonePicker();
};

document.getElementById("cw-clear-btn").addEventListener("click", async () => {
  try {
    await postPath("/clear_cw_decode");
    window.resetCwHistoryView();
  } catch (e) {
    console.error("CW clear failed", e);
  }
});

// --- Server-side CW decode handler ---
window.onServerCw = function(evt) {
  if (cwStatusEl) cwStatusEl.textContent = "Receiving";
  if (evt.text && cwOutputEl) {
    // Append decoded text to output
    const now = Date.now();
    if (!cwOutputEl.lastElementChild || now - cwLastAppendTime > 10000 || evt.text === "\n") {
      const line = document.createElement("div");
      line.className = "cw-line";
      cwOutputEl.appendChild(line);
    }
    cwLastAppendTime = now;
    const lastLine = cwOutputEl.lastElementChild;
    if (lastLine) {
      lastLine.textContent += evt.text;
    }
    while (cwOutputEl.children.length > CW_MAX_LINES) {
      cwOutputEl.removeChild(cwOutputEl.firstChild);
    }
    cwOutputEl.scrollTop = cwOutputEl.scrollHeight;
  }
  if (cwSignalIndicator) {
    cwSignalIndicator.className = evt.signal_on ? "cw-signal-on" : "cw-signal-off";
  }
  if (!cwAutoInput || cwAutoInput.checked) {
    if (cwWpmInput && Number.isFinite(Number(evt.wpm))) {
      cwWpmInput.value = clampCwWpm(evt.wpm);
    }
    if (cwToneInput && Number.isFinite(Number(evt.tone_hz))) {
      cwToneInput.value = toneClampForRange(evt.tone_hz, currentCwToneRange());
    }
  }
  if (cwTonePickerRaf != null) return;
  cwTonePickerRaf = requestAnimationFrame(() => {
    cwTonePickerRaf = null;
    drawCwTonePicker();
  });
};

window.refreshCwTonePicker = drawCwTonePicker;
window.addEventListener("resize", () => {
  if (ensureCwToneCanvasResolution()) drawCwTonePicker();
});
applyCwAutoUi(!!cwAutoInput?.checked);
ensureCwToneCanvasResolution();
drawCwTonePicker();
