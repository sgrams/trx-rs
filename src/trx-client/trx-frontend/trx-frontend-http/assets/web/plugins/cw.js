// --- CW (Morse) Decoder Plugin (server-side decode) ---
const cwStatusEl = document.getElementById("cw-status");
const cwOutputEl = document.getElementById("cw-output");
const cwAutoInput = document.getElementById("cw-auto");
const cwWpmInput = document.getElementById("cw-wpm");
const cwToneInput = document.getElementById("cw-tone");
const cwSignalIndicator = document.getElementById("cw-signal-indicator");
const CW_MAX_LINES = 200;

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
}

if (cwAutoInput) {
  cwAutoInput.addEventListener("change", async () => {
    const enabled = cwAutoInput.checked;
    applyCwAutoUi(enabled);
    try { await postPath(`/set_cw_auto?enabled=${enabled ? 1 : 0}`); }
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
    if (cwAutoInput && cwAutoInput.checked) return;
    const tone = Math.max(300, Math.min(1200, Number(cwToneInput.value)));
    cwToneInput.value = tone;
    try { await postPath(`/set_cw_tone?tone_hz=${encodeURIComponent(tone)}`); }
    catch (e) { console.error("CW tone set failed", e); }
  });
}

window.resetCwHistoryView = function() {
  cwOutputEl.innerHTML = "";
  cwLastAppendTime = 0;
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
    cwToneInput.value = evt.tone_hz;
  }
};
