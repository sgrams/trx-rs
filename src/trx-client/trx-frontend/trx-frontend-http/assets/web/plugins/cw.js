// --- CW (Morse) Decoder Plugin (server-side decode) ---
const cwStatusEl = document.getElementById("cw-status");
const cwOutputEl = document.getElementById("cw-output");
const cwPauseBtn = document.getElementById("cw-pause-btn");
const cwAutoInput = document.getElementById("cw-auto");
const cwWpmInput = document.getElementById("cw-wpm");
const cwToneInput = document.getElementById("cw-tone");
const cwSignalIndicator = document.getElementById("cw-signal-indicator");
const cwToneCanvas = document.getElementById("cw-tone-waterfall");
const cwTonePickerEl = document.querySelector(".cw-tone-picker");
const cwToneRangeEl = document.getElementById("cw-tone-range");
const CW_MAX_LINES = 200;
const CW_TONE_MIN_HZ = 100;
const CW_TONE_MAX_HZ = 10_000;
const CW_WPM_MIN = 5;
const CW_WPM_MAX = 40;
let cwLastAppendTime = 0;
let cwTonePickerRaf = null;
let cwPaused = false;
let cwBufferedWhilePaused = 0;

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
  const tunedHz = Number.isFinite(window.lastFreqHz) ? Number(window.lastFreqHz) : NaN;
  const bandwidthHz = Number.isFinite(window.currentBandwidthHz) ? Number(window.currentBandwidthHz) : NaN;
  if (!Number.isFinite(tunedHz) || !Number.isFinite(bandwidthHz) || bandwidthHz <= 0) {
    return null;
  }
  const mode = String(document.getElementById("mode")?.value || "").toUpperCase();
  const lowerSideband = mode === "CWR";
  const upperSideband = mode === "CW";
  if (!lowerSideband && !upperSideband) return null;

  const toneMinHz = CW_TONE_MIN_HZ;
  const toneMaxHz = CW_TONE_MAX_HZ;
  if (toneMaxHz < toneMinHz) {
    return null;
  }
  return {
    tunedHz,
    bandwidthHz,
    toneMinHz,
    toneMaxHz,
    toneSpanHz: Math.max(1, toneMaxHz - toneMinHz),
    lowerSideband,
    mode,
  };
}

function cwToneToRfHz(range, toneHz) {
  if (!range) return NaN;
  return range.lowerSideband
    ? range.tunedHz - toneHz
    : range.tunedHz + toneHz;
}

function toneClampForRange(tone, range) {
  const clamped = clampCwTone(tone);
  if (!range) return clamped;
  return Math.max(range.toneMinHz, Math.min(range.toneMaxHz, clamped));
}

function ensureCwToneCanvasResolution() {
  if (!cwToneCanvas) return false;
  const rect = cwToneCanvas.getBoundingClientRect();
  const cssWidth = Math.round(rect.width);
  const cssHeight = Math.round(rect.height);
  if (cssWidth < 8 || cssHeight < 8) {
    return false;
  }
  const dpr = window.devicePixelRatio || 1;
  const nextWidth = Math.round(cssWidth * dpr);
  const nextHeight = Math.round(cssHeight * dpr);
  if (cwToneCanvas.width !== nextWidth || cwToneCanvas.height !== nextHeight) {
    cwToneCanvas.width = nextWidth;
    cwToneCanvas.height = nextHeight;
    return true;
  }
  return false;
}

function drawCwTonePicker() {
  if (!cwToneCanvas) return;
  ensureCwToneCanvasResolution();
  if (cwToneCanvas.width < 8 || cwToneCanvas.height < 8) return;
  const ctx = cwToneCanvas.getContext("2d");
  if (!ctx) return;

  const width = cwToneCanvas.width;
  const height = cwToneCanvas.height;
  ctx.clearRect(0, 0, width, height);

  const range = currentCwToneRange();
  if (!window.lastSpectrumData || !Array.isArray(window.lastSpectrumData.bins) || !window.lastSpectrumData.bins.length || !range) {
    if (cwToneRangeEl) {
      const mode = String(document.getElementById("mode")?.value || "").toUpperCase();
      if (mode !== "CW" && mode !== "CWR") {
        cwToneRangeEl.textContent = "CW/CWR mode required";
      } else if (!window.lastSpectrumData || !Array.isArray(window.lastSpectrumData.bins) || !window.lastSpectrumData.bins.length) {
        cwToneRangeEl.textContent = "Waiting for spectrum";
      }
    }
    ctx.fillStyle = "rgba(130, 150, 165, 0.22)";
    ctx.fillRect(0, 0, width, height);
    return;
  }

  if (cwToneRangeEl) {
    const side = range.lowerSideband ? "Lower side" : "Upper side";
    cwToneRangeEl.textContent = `Audio ${range.toneMinHz}-${range.toneMaxHz} Hz · ${side}`;
  }

  const bins = window.lastSpectrumData.bins;
  const sampleRate = Number(window.lastSpectrumData.sample_rate);
  const centerHz = Number(window.lastSpectrumData.center_hz);
  const maxIdx = Math.max(1, bins.length - 1);
  const fullLoHz = centerHz - sampleRate / 2;
  const tones = new Array(width).fill(-140);
  for (let x = 0; x < width; x += 1) {
    const frac = width <= 1 ? 0 : x / (width - 1);
    const toneHz = range.toneMinHz + frac * range.toneSpanHz;
    const rfHz = cwToneToRfHz(range, toneHz);
    const idx = Math.max(0, Math.min(maxIdx, Math.round((((rfHz - fullLoHz) / sampleRate) * maxIdx))));
    const power = Number.isFinite(Number(bins[idx])) ? Number(bins[idx]) : -140;
    tones[x] = power;
  }

  const smoothed = new Array(width).fill(-140);
  const smoothRadius = Math.max(1, Math.round(width / 180));
  for (let x = 0; x < width; x += 1) {
    let sum = 0;
    let count = 0;
    for (let i = x - smoothRadius; i <= x + smoothRadius; i += 1) {
      if (i < 0 || i >= width) continue;
      sum += tones[i];
      count += 1;
    }
    smoothed[x] = count > 0 ? sum / count : tones[x];
  }

  const sorted = smoothed.slice().sort((a, b) => a - b);
  const q20 = sorted[Math.floor((sorted.length - 1) * 0.2)] ?? -120;
  const q95 = sorted[Math.floor((sorted.length - 1) * 0.95)] ?? -70;
  const floorDb = Math.min(q20 - 2, q95 - 10);
  const ceilDb = Math.max(floorDb + 18, q95 + 2);
  const dbSpan = Math.max(1, ceilDb - floorDb);
  const yForDb = (db) => {
    const n = Math.max(0, Math.min(1, (db - floorDb) / dbSpan));
    return Math.round((1 - n) * (height - 1));
  };

  const rootStyle = getComputedStyle(document.documentElement);
  const accent = (rootStyle.getPropertyValue("--accent-green") || "").trim() || "#00d17f";
  const axisColor = "rgba(230, 235, 245, 0.15)";
  const textColor = "rgba(230, 235, 245, 0.58)";

  ctx.fillStyle = "rgba(7, 12, 18, 0.94)";
  ctx.fillRect(0, 0, width, height);

  const hGridCount = 4;
  ctx.strokeStyle = axisColor;
  ctx.lineWidth = 1;
  for (let i = 1; i <= hGridCount; i += 1) {
    const y = Math.round((i / (hGridCount + 1)) * (height - 1)) + 0.5;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(width, y);
    ctx.stroke();
  }

  const toneStep = range.toneSpanHz <= 500 ? 50 : range.toneSpanHz <= 1000 ? 100 : 200;
  const firstTick = Math.ceil(range.toneMinHz / toneStep) * toneStep;
  ctx.font = `${Math.max(10, Math.round(height * 0.18))}px ui-monospace, SFMono-Regular, Menlo, monospace`;
  ctx.fillStyle = textColor;
  for (let tone = firstTick; tone <= range.toneMaxHz; tone += toneStep) {
    const frac = (tone - range.toneMinHz) / range.toneSpanHz;
    const x = Math.max(0, Math.min(width - 1, Math.round(frac * (width - 1)))) + 0.5;
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, height);
    ctx.stroke();
    if (tone % (toneStep * 2) === 0) {
      const label = `${Math.round(tone)}`;
      const textWidth = ctx.measureText(label).width;
      ctx.fillText(label, Math.max(1, Math.min(width - textWidth - 1, x + 2)), height - 3);
    }
  }

  ctx.beginPath();
  ctx.moveTo(0, height - 0.5);
  for (let x = 0; x < width; x += 1) {
    ctx.lineTo(x + 0.5, yForDb(smoothed[x]) + 0.5);
  }
  ctx.lineTo(width - 0.5, height - 0.5);
  ctx.closePath();
  ctx.save();
  ctx.globalAlpha = 0.24;
  ctx.fillStyle = accent;
  ctx.fill();
  ctx.restore();

  ctx.beginPath();
  for (let x = 0; x < width; x += 1) {
    const y = yForDb(smoothed[x]) + 0.5;
    if (x === 0) ctx.moveTo(0.5, y);
    else ctx.lineTo(x + 0.5, y);
  }
  ctx.lineWidth = 1.8;
  ctx.strokeStyle = accent;
  ctx.stroke();

  const currentTone = toneClampForRange(cwToneInput ? cwToneInput.value : 700, range);
  const markerFrac = (currentTone - range.toneMinHz) / range.toneSpanHz;
  const markerX = Math.max(0, Math.min(width - 1, Math.round(markerFrac * (width - 1))));
  const markerY = yForDb(smoothed[Math.max(0, Math.min(width - 1, markerX))]);
  ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
  ctx.fillRect(markerX, 0, 1.5, height);
  ctx.beginPath();
  ctx.arc(markerX, markerY, Math.max(2, Math.round(height * 0.055)), 0, Math.PI * 2);
  ctx.fill();

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
    const tone = range.toneMinHz + frac * range.toneSpanHz;
    await setCwTone(tone);
  });
}

window.resetCwHistoryView = function() {
  if (cwOutputEl) cwOutputEl.innerHTML = "";
  cwLastAppendTime = 0;
  cwBufferedWhilePaused = 0;
  updateCwPauseUi();
  drawCwTonePicker();
};

function updateCwPauseUi() {
  if (!cwPauseBtn) return;
  cwPauseBtn.textContent = cwPaused ? "Resume" : "Pause";
  cwPauseBtn.classList.toggle("active", cwPaused);
}

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
  if (cwStatusEl) cwStatusEl.textContent = cwPaused ? "Paused" : "Receiving";
  if (evt.text && cwOutputEl && !cwPaused) {
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
  if (cwPaused && evt.text) {
    cwBufferedWhilePaused += 1;
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

if (cwPauseBtn) {
  cwPauseBtn.addEventListener("click", () => {
    cwPaused = !cwPaused;
    if (!cwPaused) {
      cwBufferedWhilePaused = 0;
    }
    updateCwPauseUi();
  });
}

window.refreshCwTonePicker = function refreshCwTonePicker() {
  ensureCwToneCanvasResolution();
  drawCwTonePicker();
};
window.addEventListener("resize", () => {
  if (ensureCwToneCanvasResolution()) drawCwTonePicker();
});
applyCwAutoUi(!!cwAutoInput?.checked);
updateCwPauseUi();
ensureCwToneCanvasResolution();
drawCwTonePicker();
