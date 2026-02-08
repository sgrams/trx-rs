// --- CW (Morse) Decoder Plugin ---
const cwToggleBtn = document.getElementById("cw-toggle-btn");
const cwStatusEl = document.getElementById("cw-status");
const cwOutputEl = document.getElementById("cw-output");
const cwWpmInput = document.getElementById("cw-wpm");
const cwToneInput = document.getElementById("cw-tone");
const cwThresholdInput = document.getElementById("cw-threshold");
const cwThresholdVal = document.getElementById("cw-threshold-val");
const cwSignalIndicator = document.getElementById("cw-signal-indicator");
const cwWpmAutoCheck = document.getElementById("cw-wpm-auto");
const cwToneAutoCheck = document.getElementById("cw-tone-auto");
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

// Toggle readonly on WPM input based on Auto checkbox
cwWpmAutoCheck.addEventListener("change", () => {
  cwWpmInput.readOnly = cwWpmAutoCheck.checked;
});

// Toggle readonly on Tone input based on Auto checkbox
cwToneAutoCheck.addEventListener("change", () => {
  cwToneInput.readOnly = cwToneAutoCheck.checked;
});

function createCwDecoder(sampleRate) {
  let wpm = parseInt(cwWpmInput.value, 10) || 15;
  let toneFreq = parseInt(cwToneInput.value, 10) || 700;
  let threshold = (parseInt(cwThresholdInput.value, 10) || 5) / 100;

  // Goertzel parameters for main detector
  const windowMs = 50; // 50ms analysis window
  const windowSize = Math.round(sampleRate * windowMs / 1000);
  let k = Math.round(toneFreq * windowSize / sampleRate);
  let omega = (2 * Math.PI * k) / windowSize;
  let coeff = 2 * Math.cos(omega);

  let sampleBuf = new Float32Array(windowSize);
  let sampleIdx = 0;

  // Tone state tracking
  let toneOn = false;
  let toneOnAt = 0;
  let toneOffAt = 0;
  let currentSymbol = ""; // accumulates dits/dahs for current character
  let decoded = "";
  let lastAppendTime = 0;

  // --- Auto Tone Detection ---
  // Scan 300–1200 Hz in ~25 Hz steps
  const TONE_SCAN_LOW = 300;
  const TONE_SCAN_HIGH = 1200;
  const TONE_SCAN_STEP = 25;
  const toneScanBins = [];
  for (let f = TONE_SCAN_LOW; f <= TONE_SCAN_HIGH; f += TONE_SCAN_STEP) {
    const bk = Math.round(f * windowSize / sampleRate);
    const bOmega = (2 * Math.PI * bk) / windowSize;
    toneScanBins.push({ freq: f, coeff: 2 * Math.cos(bOmega) });
  }
  let toneStableBin = -1;   // index of the bin that's been stable
  let toneStableCount = 0;  // how many consecutive windows it's been the peak
  const TONE_STABLE_NEEDED = 3;

  // --- Auto WPM Detection ---
  const onDurations = [];    // rolling buffer of on-durations (ms)
  const MAX_ON_DURATIONS = 30;
  const MIN_ON_DURATIONS = 8;

  function recomputeGoertzel(newFreq) {
    toneFreq = newFreq;
    k = Math.round(toneFreq * windowSize / sampleRate);
    omega = (2 * Math.PI * k) / windowSize;
    coeff = 2 * Math.cos(omega);
  }

  // Timing: 1 unit = 1200/WPM ms
  function unitMs() { return 1200 / wpm; }

  function goertzelEnergy(buf, c) {
    let s0 = 0, s1 = 0, s2 = 0;
    for (let i = 0; i < buf.length; i++) {
      s0 = c * s1 - s2 + buf[i];
      s2 = s1;
      s1 = s0;
    }
    return (s1 * s1 + s2 * s2 - c * s1 * s2) / (buf.length * buf.length);
  }

  function goertzelDetect(buf) {
    const toneEnergy = goertzelEnergy(buf, coeff);
    let totalEnergy = 0;
    for (let i = 0; i < buf.length; i++) {
      totalEnergy += buf[i] * buf[i];
    }
    const avgEnergy = totalEnergy / buf.length;
    if (avgEnergy < 1e-10) return false;
    return (toneEnergy / avgEnergy) > threshold;
  }

  function autoDetectTone(buf) {
    // Compute broadband energy
    let totalEnergy = 0;
    for (let i = 0; i < buf.length; i++) {
      totalEnergy += buf[i] * buf[i];
    }
    const avgEnergy = totalEnergy / buf.length;
    if (avgEnergy < 1e-10) return;

    // Find the bin with highest energy relative to broadband
    let bestIdx = -1;
    let bestRatio = 0;
    for (let b = 0; b < toneScanBins.length; b++) {
      const e = goertzelEnergy(buf, toneScanBins[b].coeff);
      const ratio = e / avgEnergy;
      if (ratio > bestRatio) {
        bestRatio = ratio;
        bestIdx = b;
      }
    }

    // Require the peak to exceed threshold to be meaningful
    if (bestRatio < threshold || bestIdx < 0) {
      toneStableCount = 0;
      toneStableBin = -1;
      return;
    }

    // Check stability: same bin ±1
    if (toneStableBin >= 0 && Math.abs(bestIdx - toneStableBin) <= 1) {
      toneStableCount++;
    } else {
      toneStableBin = bestIdx;
      toneStableCount = 1;
    }

    if (toneStableCount >= TONE_STABLE_NEEDED) {
      const detectedFreq = toneScanBins[toneStableBin].freq;
      if (Math.abs(detectedFreq - toneFreq) > TONE_SCAN_STEP) {
        recomputeGoertzel(detectedFreq);
        cwToneInput.value = detectedFreq;
      }
    }
  }

  function autoDetectWpm() {
    if (onDurations.length < MIN_ON_DURATIONS) return;

    // Sort durations ascending
    const sorted = onDurations.slice().sort((a, b) => a - b);

    // K-means-style split: find the best boundary between dit and dah clusters
    let bestBoundary = 1;
    let bestScore = Infinity;
    for (let i = 1; i < sorted.length; i++) {
      const cluster1 = sorted.slice(0, i);
      const cluster2 = sorted.slice(i);
      const mean1 = cluster1.reduce((a, b) => a + b, 0) / cluster1.length;
      const mean2 = cluster2.reduce((a, b) => a + b, 0) / cluster2.length;
      let score = 0;
      for (const v of cluster1) score += (v - mean1) * (v - mean1);
      for (const v of cluster2) score += (v - mean2) * (v - mean2);
      if (score < bestScore) {
        bestScore = score;
        bestBoundary = i;
      }
    }

    // The shorter cluster is dits — take the median
    const ditCluster = sorted.slice(0, bestBoundary);
    if (ditCluster.length === 0) return;
    const ditMs = ditCluster[Math.floor(ditCluster.length / 2)];
    if (ditMs < 10) return; // too short, ignore

    let newWpm = Math.round(1200 / ditMs);
    newWpm = Math.max(5, Math.min(40, newWpm));
    if (newWpm !== wpm) {
      wpm = newWpm;
      cwWpmInput.value = wpm;
    }
  }

  function processWindow() {
    // Run auto tone detection if enabled
    if (cwToneAutoCheck.checked) {
      autoDetectTone(sampleBuf);
    }

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

      // Collect on-duration for auto WPM
      if (cwWpmAutoCheck.checked) {
        onDurations.push(onDuration);
        if (onDurations.length > MAX_ON_DURATIONS) {
          onDurations.shift();
        }
        autoDetectWpm();
      }
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
    if (!cwWpmAutoCheck.checked) {
      wpm = parseInt(cwWpmInput.value, 10) || 15;
    }
    if (!cwToneAutoCheck.checked) {
      const newTone = parseInt(cwToneInput.value, 10) || 700;
      if (newTone !== toneFreq) {
        recomputeGoertzel(newTone);
      }
    }
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
        cwToneInput.addEventListener("change", decoderEngine.updateConfig);
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
