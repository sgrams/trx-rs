// --- CW (Morse) Decoder Plugin ---
const cwToggleBtn = document.getElementById("cw-toggle-btn");
const cwStatusEl = document.getElementById("cw-status");
const cwOutputEl = document.getElementById("cw-output");
const cwWpmInput = document.getElementById("cw-wpm");
const cwToneInput = document.getElementById("cw-tone");
const cwThresholdInput = document.getElementById("cw-threshold");
const cwThresholdVal = document.getElementById("cw-threshold-val");
const cwSignalIndicator = document.getElementById("cw-signal-indicator");
const CW_MAX_LINES = 200;

let cwActive = false;
let cwWs = null;
let cwAudioCtx = null;
let cwDecoder = null;

// ITU Morse code lookup
const MORSE_TABLE = {
  ".-": "A", "-...": "B", "-.-.": "C", "-..": "D", ".": "E",
  "..-.": "F", "--.": "G", "....": "H", "..": "I", ".---": "J",
  "-.-": "K", ".-..": "L", "--": "M", "-.": "N", "---": "O",
  ".--.": "P", "--.-": "Q", ".-.": "R", "...": "S", "-": "T",
  "..-": "U", "...-": "V", ".--": "W", "-..-": "X", "-.--": "Y",
  "--..": "Z",
  "-----": "0", ".----": "1", "..---": "2", "...--": "3", "....-": "4",
  ".....": "5", "-....": "6", "--...": "7", "---..": "8", "----.": "9",
  ".-.-.-": ".", "--..--": ",", "..--..": "?", ".----.": "'",
  "-.-.--": "!", "-..-.": "/", "-.--.": "(", "-.--.-": ")",
  ".-...": "&", "---...": ":", "-.-.-.": ";", "-...-": "=",
  ".-.-.": "+", "-....-": "-", "..--.-": "_", ".-..-.": "\"",
  "...-..-": "$", ".--.-.": "@",
};

// Update threshold display
cwThresholdInput.addEventListener("input", () => {
  cwThresholdVal.textContent = (cwThresholdInput.value / 100).toFixed(2);
});

function createCwDecoder(sampleRate) {
  let wpm = parseInt(cwWpmInput.value, 10) || 15;
  let toneFreq = parseInt(cwToneInput.value, 10) || 700;
  let threshold = (parseInt(cwThresholdInput.value, 10) || 5) / 100;

  // Goertzel parameters
  const windowMs = 50; // 50ms analysis window
  const windowSize = Math.round(sampleRate * windowMs / 1000);
  const k = Math.round(toneFreq * windowSize / sampleRate);
  const omega = (2 * Math.PI * k) / windowSize;
  const coeff = 2 * Math.cos(omega);

  let sampleBuf = new Float32Array(windowSize);
  let sampleIdx = 0;

  // Tone state tracking
  let toneOn = false;
  let toneOnAt = 0;
  let toneOffAt = 0;
  let currentSymbol = ""; // accumulates dits/dahs for current character
  let decoded = "";
  let lastAppendTime = 0;

  // Timing: 1 unit = 1200/WPM ms
  function unitMs() { return 1200 / wpm; }

  function goertzelDetect(buf) {
    let s0 = 0, s1 = 0, s2 = 0;
    let totalEnergy = 0;
    for (let i = 0; i < buf.length; i++) {
      s0 = coeff * s1 - s2 + buf[i];
      s2 = s1;
      s1 = s0;
      totalEnergy += buf[i] * buf[i];
    }
    const toneEnergy = (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (buf.length * buf.length);
    const avgEnergy = totalEnergy / buf.length;
    if (avgEnergy < 1e-10) return false;
    return (toneEnergy / avgEnergy) > threshold;
  }

  function processWindow() {
    const detected = goertzelDetect(sampleBuf);
    const now = performance.now();

    // Update signal indicator
    if (detected) {
      cwSignalIndicator.className = "cw-signal-on";
    } else {
      cwSignalIndicator.className = "cw-signal-off";
    }

    if (detected && !toneOn) {
      // Tone just turned on
      toneOn = true;
      const offDuration = now - toneOffAt;
      if (toneOffAt > 0) {
        const u = unitMs();
        if (offDuration > u * 5) {
          // Word gap (7 units, use 5 as threshold)
          if (currentSymbol) {
            const ch = MORSE_TABLE[currentSymbol] || "?";
            appendChar(ch);
            currentSymbol = "";
          }
          appendChar(" ");
        } else if (offDuration > u * 2) {
          // Character gap (3 units, use 2 as threshold)
          if (currentSymbol) {
            const ch = MORSE_TABLE[currentSymbol] || "?";
            appendChar(ch);
            currentSymbol = "";
          }
        }
        // else: inter-element gap, do nothing
      }
      toneOnAt = now;
    } else if (!detected && toneOn) {
      // Tone just turned off
      toneOn = false;
      const onDuration = now - toneOnAt;
      const u = unitMs();
      if (onDuration > u * 2) {
        currentSymbol += "-"; // dah (3 units, use 2 as threshold)
      } else {
        currentSymbol += "."; // dit
      }
      toneOffAt = now;
    }

    // Flush pending character after long silence
    if (!toneOn && currentSymbol && toneOffAt > 0) {
      const silenceDuration = now - toneOffAt;
      if (silenceDuration > unitMs() * 5) {
        const ch = MORSE_TABLE[currentSymbol] || "?";
        appendChar(ch);
        currentSymbol = "";
      }
    }
  }

  function appendChar(ch) {
    decoded += ch;
    // Append to output element
    const now = Date.now();
    if (!cwOutputEl.lastElementChild || now - lastAppendTime > 10000 || ch === "\n") {
      const line = document.createElement("div");
      line.className = "cw-line";
      cwOutputEl.appendChild(line);
    }
    lastAppendTime = now;
    const lastLine = cwOutputEl.lastElementChild;
    if (lastLine) {
      lastLine.textContent += ch;
    }
    // Cap lines
    while (cwOutputEl.children.length > CW_MAX_LINES) {
      cwOutputEl.removeChild(cwOutputEl.firstChild);
    }
    cwOutputEl.scrollTop = cwOutputEl.scrollHeight;
  }

  function processSamples(mono) {
    for (let i = 0; i < mono.length; i++) {
      sampleBuf[sampleIdx++] = mono[i];
      if (sampleIdx >= windowSize) {
        processWindow();
        sampleIdx = 0;
      }
    }
  }

  function updateConfig() {
    wpm = parseInt(cwWpmInput.value, 10) || 15;
    threshold = (parseInt(cwThresholdInput.value, 10) || 5) / 100;
  }

  return { processSamples, updateConfig };
}

function startCw() {
  if (cwActive) { stopCw(); return; }
  if (!hasWebCodecs) {
    cwStatusEl.textContent = "Requires Chrome/Edge";
    return;
  }

  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  cwWs = new WebSocket(`${proto}//${location.host}/audio`);
  cwWs.binaryType = "arraybuffer";
  cwStatusEl.textContent = "Connecting…";

  let decoderEngine = null;

  cwWs.onopen = () => {
    cwStatusEl.textContent = "Waiting for stream info…";
  };

  cwWs.onmessage = (evt) => {
    if (typeof evt.data === "string") {
      try {
        const info = JSON.parse(evt.data);
        const sr = info.sample_rate || 48000;
        const ch = info.channels || 1;
        cwAudioCtx = new AudioContext({ sampleRate: sr });
        decoderEngine = createCwDecoder(sr);

        let cwFrameCount = 0;
        cwDecoder = new AudioDecoder({
          output: (frame) => {
            if (cwFrameCount++ === 0) {
              console.log("[CW-DBG] First PCM frame:", frame.numberOfFrames, "samples,", frame.numberOfChannels, "ch, format:", frame.format, "sr:", frame.sampleRate);
            }
            const buf = new Float32Array(frame.numberOfFrames * frame.numberOfChannels);
            frame.copyTo(buf, { planeIndex: 0 });
            let mono;
            if (frame.numberOfChannels === 1) {
              mono = buf;
            } else {
              mono = new Float32Array(frame.numberOfFrames);
              for (let i = 0; i < frame.numberOfFrames; i++) {
                mono[i] = buf[i * frame.numberOfChannels];
              }
            }
            decoderEngine.processSamples(mono);
            frame.close();
          },
          error: (e) => { console.error("CW AudioDecoder error", e); }
        });
        cwDecoder.configure({
          codec: "opus",
          sampleRate: sr,
          numberOfChannels: ch,
        });

        cwActive = true;
        cwToggleBtn.style.borderColor = "#00d17f";
        cwToggleBtn.style.color = "#00d17f";
        cwToggleBtn.textContent = "Stop CW";
        cwStatusEl.textContent = "Listening…";

        // Allow live config updates
        cwWpmInput.addEventListener("change", decoderEngine.updateConfig);
        cwThresholdInput.addEventListener("input", decoderEngine.updateConfig);
      } catch (e) {
        console.error("CW stream info error", e);
        cwStatusEl.textContent = "Error";
      }
      return;
    }

    // Binary Opus data
    if (!cwDecoder) return;
    try {
      cwDecoder.decode(new EncodedAudioChunk({
        type: "key",
        timestamp: performance.now() * 1000,
        data: new Uint8Array(evt.data),
      }));
    } catch (e) {
      // Ignore individual decode errors
    }
  };

  cwWs.onclose = () => {
    stopCw();
  };

  cwWs.onerror = () => {
    cwStatusEl.textContent = "Connection error";
  };
}

function stopCw() {
  cwActive = false;
  if (cwWs) { cwWs.close(); cwWs = null; }
  if (cwAudioCtx) { cwAudioCtx.close(); cwAudioCtx = null; }
  if (cwDecoder) {
    try { cwDecoder.close(); } catch (e) {}
    cwDecoder = null;
  }
  cwToggleBtn.style.borderColor = "";
  cwToggleBtn.style.color = "";
  cwToggleBtn.textContent = "Start CW";
  cwStatusEl.textContent = "Stopped";
  cwSignalIndicator.className = "cw-signal-off";
}

cwToggleBtn.addEventListener("click", startCw);
