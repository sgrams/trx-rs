// --- CW (Morse) Decoder Plugin (server-side decode) ---
const cwStatusEl = document.getElementById("cw-status");
const cwOutputEl = document.getElementById("cw-output");
const cwAutoInput = document.getElementById("cw-auto");
const cwWpmInput = document.getElementById("cw-wpm");
const cwToneInput = document.getElementById("cw-tone");
const cwSignalIndicator = document.getElementById("cw-signal-indicator");
const cwToneCanvas = document.getElementById("cw-tone-waterfall");
const cwToneGl = typeof createTrxWebGlRenderer === "function"
  ? createTrxWebGlRenderer(cwToneCanvas, { alpha: true })
  : null;
const cwTonePickerEl = document.querySelector(".cw-tone-picker");
const cwToneRangeEl = document.getElementById("cw-tone-range");
const cwBarOverlay = document.getElementById("cw-bar-overlay");
const CW_MAX_LINES = 200;
const CW_TONE_MIN_HZ = 100;
const CW_TONE_MAX_HZ = 10_000;
const CW_WPM_MIN = 5;
const CW_WPM_MAX = 40;
const CW_BAR_WINDOW_MS = 15 * 60 * 1000;
const CW_BAR_LINE_GAP_MS = 5000;
let cwLastAppendTime = 0;
let cwTonePickerRaf = null;
let cwBarHistory = [];     // [{tsMs, ts, text, wpm, tone_hz}]
let cwBarCurrentLine = null; // accumulates chars until gap/newline
let cwBarDismissedAtMs = 0;
// Tracks a user-initiated auto toggle that is in-flight (POST not yet
// acknowledged).  While set, server-state updates must not override the
// checkbox so that a concurrent SSE event carrying the *old* cw_auto value
// does not immediately undo the user's choice.
let cwAutoLocalOverride = null;

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

// Called by app.js render() when a server-state snapshot arrives.  Ignores
// the update while cwAutoLocalOverride is set (user change still in-flight).
window.applyCwAutoUiFromServer = function(enabled) {
  if (cwAutoLocalOverride !== null) return;
  applyCwAutoUi(enabled);
};

function cwBarFlushCurrentLine() {
  if (cwBarCurrentLine && cwBarCurrentLine.text.trim()) {
    cwBarHistory.unshift(cwBarCurrentLine);
    if (cwBarHistory.length > 50) cwBarHistory.length = 50;
  }
  cwBarCurrentLine = null;
}

function updateCwBar() {
  if (!cwBarOverlay) return;
  const mode = (document.getElementById("mode")?.value || "").toUpperCase();
  const isCw = mode === "CW" || mode === "CWR";
  const cutoffMs = Date.now() - CW_BAR_WINDOW_MS;
  const recent = cwBarHistory.filter((l) => l.tsMs >= cutoffMs);
  // Prepend the in-progress line so characters appear immediately
  const liveLines = cwBarCurrentLine && cwBarCurrentLine.text ? [cwBarCurrentLine, ...recent] : recent;
  const newestTsMs = liveLines.reduce((latest, line) => Math.max(latest, Number(line.tsMs) || 0), 0);
  if (!isCw || liveLines.length === 0 || newestTsMs <= cwBarDismissedAtMs) {
    cwBarOverlay.style.display = "none";
    cwBarOverlay.innerHTML = "";
    return;
  }
  let html =
    '<div class="aprs-bar-header">' +
      '<span class="aprs-bar-title"><span class="aprs-bar-title-word">CW</span><span class="aprs-bar-title-word">Live</span></span>' +
      '<span class="aprs-bar-actions">' +
        '<span class="aprs-bar-window">Last 15 minutes</span>' +
        '<span class="aprs-bar-clear-wrap"><span class="aprs-bar-clear" role="button" tabindex="0"' +
          ' onclick="window.clearCwBar()"' +
          ' onkeydown="if(event.key===\'Enter\'||event.key===\' \'){event.preventDefault();window.clearCwBar();}"' +
          ' aria-label="Clear CW overlay">Clear</span></span>' +
        '<button class="aprs-bar-close" type="button" onclick="window.closeCwBar()" aria-label="Close CW overlay">&times;</button>' +
      '</span>' +
    '</div>';
  for (const line of liveLines.slice(0, 8)) {
    const ts = line.ts ? `<span class="aprs-bar-time">${line.ts}</span>` : "";
    const meta = [
      line.wpm ? `${line.wpm} WPM` : null,
      line.tone_hz ? `${line.tone_hz} Hz` : null,
    ].filter(Boolean).join(" · ");
    html += `<div class="aprs-bar-frame">` +
      `<div class="aprs-bar-frame-main">${ts}${escapeMapHtml(line.text)}` +
      (meta ? ` <span class="aprs-bar-time">${escapeMapHtml(meta)}</span>` : "") +
      `</div></div>`;
  }
  cwBarOverlay.innerHTML = html;
  cwBarOverlay.style.display = "flex";
}
window.updateCwBar = updateCwBar;
window.clearCwBar = function() {
  window.resetCwHistoryView();
};
window.closeCwBar = function() {
  cwBarDismissedAtMs = Date.now();
  if (cwBarOverlay) {
    cwBarOverlay.style.display = "none";
    cwBarOverlay.innerHTML = "";
  }
};

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
  if (!cwToneCanvas || !cwToneGl || !cwToneGl.ready) return false;
  const rect = cwToneCanvas.getBoundingClientRect();
  const cssWidth = Math.round(rect.width);
  const cssHeight = Math.round(rect.height);
  if (cssWidth < 8 || cssHeight < 8) {
    return false;
  }
  const dpr = window.devicePixelRatio || 1;
  return cwToneGl.ensureSize(cssWidth, cssHeight, dpr);
}

function drawCwTonePicker() {
  if (!cwToneCanvas || !cwToneGl || !cwToneGl.ready) return;
  ensureCwToneCanvasResolution();
  if (cwToneCanvas.width < 8 || cwToneCanvas.height < 8) return;
  const width = cwToneCanvas.width;
  const height = cwToneCanvas.height;
  cwToneGl.clear([0, 0, 0, 0]);

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
    cwToneGl.fillRect(0, 0, width, height, [130 / 255, 150 / 255, 165 / 255, 0.22]);
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
  const parseColor = typeof window.trxParseCssColor === "function"
    ? window.trxParseCssColor
    : null;
  const accentRgba = parseColor ? parseColor(accent) : [0, 0.82, 0.5, 1];
  const axisColor = [230 / 255, 235 / 255, 245 / 255, 0.15];

  cwToneGl.fillRect(0, 0, width, height, [7 / 255, 12 / 255, 18 / 255, 0.94]);

  const hGridCount = 4;
  const gridSegments = [];
  for (let i = 1; i <= hGridCount; i += 1) {
    const y = Math.round((i / (hGridCount + 1)) * (height - 1));
    gridSegments.push(0, y, width, y);
  }
  cwToneGl.drawSegments(gridSegments, axisColor, 1);

  const toneStep = range.toneSpanHz <= 500 ? 50 : range.toneSpanHz <= 1000 ? 100 : 200;
  const firstTick = Math.ceil(range.toneMinHz / toneStep) * toneStep;
  const tickSegments = [];
  for (let tone = firstTick; tone <= range.toneMaxHz; tone += toneStep) {
    const frac = (tone - range.toneMinHz) / range.toneSpanHz;
    const x = Math.max(0, Math.min(width - 1, Math.round(frac * (width - 1))));
    tickSegments.push(x, 0, x, height);
  }
  cwToneGl.drawSegments(tickSegments, axisColor, 1);

  const linePoints = [];
  for (let x = 0; x < width; x += 1) {
    linePoints.push(x, yForDb(smoothed[x]));
  }
  cwToneGl.drawFilledArea(linePoints, height, [accentRgba[0], accentRgba[1], accentRgba[2], 0.24]);
  cwToneGl.drawPolyline(linePoints, accentRgba, Math.max(1.2, (window.devicePixelRatio || 1) * 1.2));

  const currentTone = toneClampForRange(cwToneInput ? cwToneInput.value : 700, range);
  const markerFrac = (currentTone - range.toneMinHz) / range.toneSpanHz;
  const markerX = Math.max(0, Math.min(width - 1, Math.round(markerFrac * (width - 1))));
  const markerY = yForDb(smoothed[Math.max(0, Math.min(width - 1, markerX))]);
  cwToneGl.drawSegments([markerX, 0, markerX, height], [1, 1, 1, 0.9], 1.5);
  cwToneGl.drawPoints([markerX, markerY], Math.max(2, Math.round(height * 0.055)), [1, 1, 1, 0.9]);

  if (cwAutoInput?.checked) {
    cwToneGl.fillRect(0, 0, width, height, [0, 0, 0, 0.22]);
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
    cwAutoLocalOverride = enabled;
    applyCwAutoUi(enabled);
    try {
      await postPath(`/set_cw_auto?enabled=${enabled ? "true" : "false"}`);
      drawCwTonePicker();
    } catch (e) {
      console.error("CW auto toggle failed", e);
    } finally {
      cwAutoLocalOverride = null;
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
  cwBarHistory = [];
  cwBarCurrentLine = null;
  updateCwBar();
  drawCwTonePicker();
};

document.getElementById("settings-clear-cw-history")?.addEventListener("click", async () => {
  try {
    await postPath("/clear_cw_decode");
    window.resetCwHistoryView();
  } catch (e) {
    console.error("CW history clear failed", e);
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
  // Bar history accumulation (regardless of pause state)
  if (evt.text) {
    const now = Date.now();
    if (evt.text === "\n") {
      cwBarFlushCurrentLine();
    } else {
      if (!cwBarCurrentLine || now - cwBarCurrentLine.lastMs > CW_BAR_LINE_GAP_MS) {
        cwBarFlushCurrentLine();
        const ts = new Date(now).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
        cwBarCurrentLine = { tsMs: now, ts, text: "", wpm: null, tone_hz: null, lastMs: now };
      }
      cwBarCurrentLine.text += evt.text;
      cwBarCurrentLine.lastMs = now;
      if (Number.isFinite(Number(evt.wpm))) cwBarCurrentLine.wpm = clampCwWpm(evt.wpm);
      if (Number.isFinite(Number(evt.tone_hz))) cwBarCurrentLine.tone_hz = Math.round(Number(evt.tone_hz));
    }
    updateCwBar();
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

window.restoreCwHistory = function(events) {
  if (!Array.isArray(events) || events.length === 0) return;
  if (cwStatusEl) cwStatusEl.textContent = "Receiving";
  for (const evt of events) {
    window.onServerCw(evt);
  }
};

window.refreshCwTonePicker = function refreshCwTonePicker() {
  ensureCwToneCanvasResolution();
  drawCwTonePicker();
};
window.addEventListener("resize", () => {
  if (ensureCwToneCanvasResolution()) drawCwTonePicker();
});
applyCwAutoUi(!!cwAutoInput?.checked);
updateCwBar();
ensureCwToneCanvasResolution();
drawCwTonePicker();
