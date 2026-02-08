// --- CW (Morse) Decoder Plugin (server-side decode) ---
const cwStatusEl = document.getElementById("cw-status");
const cwOutputEl = document.getElementById("cw-output");
const cwWpmInput = document.getElementById("cw-wpm");
const cwToneInput = document.getElementById("cw-tone");
const cwSignalIndicator = document.getElementById("cw-signal-indicator");
const CW_MAX_LINES = 200;

document.getElementById("cw-decode-toggle-btn").addEventListener("click", async () => {
  try { await postPath("/toggle_cw_decode"); } catch (e) { console.error("CW toggle failed", e); }
});

document.getElementById("cw-clear-btn").addEventListener("click", async () => {
  cwOutputEl.innerHTML = "";
  try { await postPath("/clear_cw_decode"); } catch (e) { console.error("CW clear failed", e); }
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
  cwWpmInput.value = evt.wpm;
  cwToneInput.value = evt.tone_hz;
};
