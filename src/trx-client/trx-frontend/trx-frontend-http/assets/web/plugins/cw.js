// --- CW (Morse) Decoder Plugin (server-side decode) ---
const cwStatusEl = document.getElementById("cw-status");
const cwOutputEl = document.getElementById("cw-output");
const cwAutoInput = document.getElementById("cw-auto");
const cwWpmInput = document.getElementById("cw-wpm");
const cwToneInput = document.getElementById("cw-tone");
const cwSignalIndicator = document.getElementById("cw-signal-indicator");
const cwToneCanvas = document.getElementById("cw-tone-waterfall");
const cwToneRangeEl = document.getElementById("cw-tone-range");
const CW_MAX_LINES = 200;
const CW_TONE_MIN_HZ = 300;
const CW_TONE_MAX_HZ = 1200;

function applyCwAutoUi(enabled) {
  if (cwAutoInput) cwAutoInput.checked = enabled;
  if (cwWpmInput) {
    cwWpmInput.disabled = enabled;
    cwWpmInput.readOnly = enabled;
  }
}

function clampCwTone(tone) {
  return Math.max(CW_TONE_MIN_HZ, Math.min(CW_TONE_MAX_HZ, Number(tone)));
}

function currentCwToneRange() {
  const centerHz = Number.isFinite(window.lastFreqHz) ? Number(window.lastFreqHz) : Number.NaN;
  const bandwidthHz = Number.isFinite(window.currentBandwidthHz) ? Number(window.currentBandwidthHz) : Number.NaN;
  if (!Number.isFinite(centerHz) || !Number.isFinite(bandwidthHz) || bandwidthHz <= 0) {
    return null;
  }
  return {
    lowHz: centerHz - bandwidthHz / 2,
    highHz: centerHz + bandwidthHz / 2,
    centerHz,
    bandwidthHz,
  };
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
    if (cwToneRangeEl) cwToneRangeEl.textContent = "Waiting for spectrum";
    ctx.fillStyle = "rgba(130, 150, 165, 0.22)";
    ctx.fillRect(0, 0, width, height);
    return;
  }

  if (cwToneRangeEl) {
    const lowKHz = (range.lowHz / 1000).toFixed(range.bandwidthHz >= 10_000 ? 0 : 1);
    const highKHz = (range.highHz / 1000).toFixed(range.bandwidthHz >= 10_000 ? 0 : 1);
    cwToneRangeEl.textContent = `${lowKHz} - ${highKHz} kHz`;
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
    const power = Math.max(0, Number(bins[idx]) || 0);
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

  const currentTone = clampCwTone(cwToneInput ? cwToneInput.value : 700);
  const markerFrac = (currentTone - CW_TONE_MIN_HZ) / (CW_TONE_MAX_HZ - CW_TONE_MIN_HZ);
  const markerX = Math.max(0, Math.min(width - 1, Math.round(markerFrac * (width - 1))));
  ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
  ctx.fillRect(markerX, 0, 2, height);
}

async function setCwTone(tone, { syncInput = true } = {}) {
  const clamped = clampCwTone(tone);
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
    try { await postPath(`/set_cw_auto?enabled=${enabled ? "true" : "false"}`); }
    catch (e) { console.error("CW auto toggle failed", e); }
  });
}

if (cwWpmInput) {
  cwWpmInput.addEventListener("change", async () => {
    if (cwAutoInput && cwAutoInput.checked) return;
    const wpm = Math.max(5, Math.min(40, Number(cwWpmInput.value)));
    cwWpmInput.value = wpm;
    try { await postPath(`/set_cw_wpm?wpm=${encodeURIComponent(wpm)}`); }
    catch (e) { console.error("CW WPM set failed", e); }
  });
}

if (cwToneInput) {
  cwToneInput.addEventListener("change", async () => {
    await setCwTone(cwToneInput.value);
  });
}

if (cwToneCanvas) {
  cwToneCanvas.addEventListener("click", async (event) => {
    const rect = cwToneCanvas.getBoundingClientRect();
    if (rect.width <= 0) return;
    const frac = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
    const tone = CW_TONE_MIN_HZ + frac * (CW_TONE_MAX_HZ - CW_TONE_MIN_HZ);
    await setCwTone(tone);
  });
}

window.resetCwHistoryView = function() {
  cwOutputEl.innerHTML = "";
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
let cwLastAppendTime = 0;
window.onServerCw = function(evt) {
  cwStatusEl.textContent = "Receiving";
  if (evt.text) {
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
  cwSignalIndicator.className = evt.signal_on ? "cw-signal-on" : "cw-signal-off";
  if (!cwAutoInput || cwAutoInput.checked) {
    cwWpmInput.value = evt.wpm;
  }
  if (cwToneInput && Number.isFinite(Number(evt.tone_hz))) {
    cwToneInput.value = clampCwTone(evt.tone_hz);
  }
  drawCwTonePicker();
};

window.refreshCwTonePicker = drawCwTonePicker;
drawCwTonePicker();
