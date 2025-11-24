// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

const PKG_NAME: &str = env!("CARGO_PKG_NAME");
const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn index_html(callsign: Option<&str>) -> String {
    INDEX_HTML_TEMPLATE
        .replace("{pkg}", PKG_NAME)
        .replace("{ver}", PKG_VERSION)
        .replace("{callsign_opt}", callsign.unwrap_or(""))
}

const INDEX_HTML_TEMPLATE: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>{pkg} v{ver} status</title>
  <link rel="icon" href="/favicon.ico" />
  <style>
    body { font-family: sans-serif; margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center; background: #0d1117; color: #e5e7eb; }
    .card { border: 1px solid #1f2a35; border-radius: 12px; padding: 1.25rem 1.75rem; width: min(680px, 90vw); box-shadow: 0 12px 40px rgba(0,0,0,0.35); background: #161b22; }
    .label { color: #9aa4b5; font-size: 0.9rem; margin-bottom: 6px; display: block; }
    .value { font-size: 1.4rem; margin-bottom: 0.5rem; }
    .status { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 1.1rem 1rem; }
    input.status-input, select.status-input { width: 100%; padding: 0.45rem 0.5rem; font-size: 1rem; border: 1px solid #2d3748; border-radius: 6px; background: #0f1720; color: #e5e7eb; }
    .vfo-box { width: 100%; min-height: 2.6rem; padding: 0.45rem 0.5rem; border: 1px solid #2d3748; border-radius: 6px; background: #0f1720; color: #e5e7eb; white-space: pre-line; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace; }
    .controls { margin-top: 1rem; display: flex; gap: 0.75rem; align-items: center; flex-wrap: wrap; }
    button { padding: 0.5rem 0.9rem; border-radius: 6px; border: 1px solid #394455; background: #1f2937; color: #e5e7eb; cursor: pointer; height: 2.4rem; }
    button:disabled { opacity: 0.6; cursor: not-allowed; }
    .hint { color: #9aa4b5; font-size: 0.85rem; }
    .inline { display: flex; gap: 0.5rem; align-items: center; }
    .section-title { margin-top: 0.5rem; font-size: 1.05rem; font-weight: 600; color: #c5cedd; }
    small { color: #9aa4b5; }
    .header { display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 0.25rem; }
    .title { font-size: 1.4rem; font-weight: 700; display: inline-flex; align-items: center; gap: 0.35rem; position: relative; z-index: 2; }
    .logo-bg { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; pointer-events: none; opacity: 0.2; }
    .logo-bg img { max-width: 50%; max-height: 50%; filter: drop-shadow(0 4px 12px rgba(0,0,0,0.35)); }
    .subtitle { color: #9aa4b5; font-size: 0.95rem; }
    .band-tag { display: inline-block; padding: 2px 6px; border-radius: 6px; background: #1f2937; color: #e5e7eb; font-size: 0.82rem; border: 1px solid #2d3748; margin-left: 6px; }
    .signal { display: flex; gap: 0.6rem; align-items: center; }
    .signal-bar { flex: 1 1 auto; height: 12px; border-radius: 999px; background: #1f2937; border: 1px solid #2d3748; overflow: hidden; }
    .signal-bar-fill { height: 100%; width: 0%; background: linear-gradient(90deg, #00d17f, #f0ad4e, #e55353); transition: width 150ms ease; }
    .signal-value { font-size: 0.95rem; color: #c5cedd; min-width: 48px; text-align: right; }
    .meter { display: flex; gap: 0.6rem; align-items: center; }
    .meter-bar { flex: 1 1 auto; height: 12px; border-radius: 999px; background: #1f2937; border: 1px solid #2d3748; overflow: hidden; }
    .meter-fill { height: 100%; width: 0%; background: linear-gradient(90deg, #00d17f, #f0ad4e, #e55353); transition: width 150ms ease; }
    .meter-value { font-size: 0.95rem; color: #c5cedd; min-width: 64px; text-align: right; }
    .footer { margin-top: 0.6rem; display: flex; justify-content: flex-end; }
    .full-row { grid-column: 1 / -1; }
  </style>
</head>
<body>
  <div class="card" id="card" style="position:relative; overflow:hidden;">
    <div class="logo-bg"><img id="logo" src="/logo.png?v=1" alt="trx logo" onerror="console.error('logo load failed'); this.style.display='none'" /></div>
    <div class="header" style="position:relative; z-index:2;">
      <div>
        <div class="title"><span id="rig-title">Rig status</span></div>
        <div class="subtitle">{pkg} v{ver}</div>
      </div>
      <div id="callsign" style="color:#9aa4b5; font-weight:600; display:none;">{callsign_opt}</div>
    </div>
    <div id="loading" style="text-align:center; padding:2rem 0;">
      <div id="loading-title" style="margin-bottom:0.4rem; font-size:1.1rem; font-weight:600;">Initializing (rig)…</div>
      <div id="loading-sub" style="color:#9aa4b5;"></div>
    </div>
    <div id="content" style="display:none;">
    <div class="status">
      <div>
        <div class="label">Frequency<span class="band-tag" id="band-label">--</span></div>
        <div class="inline">
          <input class="status-input" id="freq" type="text" value="--" />
          <button id="freq-apply" type="button">Set</button>
        </div>
      </div>
      <div>
        <div class="label">Mode</div>
        <div class="inline">
          <select class="status-input" id="mode">
            <option value="">--</option>
          </select>
          <button id="mode-apply" type="button">Set</button>
        </div>
      </div>
      <div>
        <div class="label">Transmit / VFO / Power</div>
        <div class="inline" style="gap: 0.6rem; flex-wrap: wrap;">
          <button id="ptt-btn" type="button" style="flex: 1 1 30%;">Toggle PTT</button>
          <button id="vfo-btn" type="button" style="flex: 1 1 30%;">VFO</button>
          <button id="power-btn" type="button" style="flex: 1 1 30%;">Toggle Power</button>
          <button id="lock-btn" type="button" style="flex: 1 1 30%;">Lock</button>
        </div>
      </div>
      <div style="margin-bottom: 0.9rem;">
        <div class="label">VFO</div>
        <div class="vfo-box" id="vfo">--</div>
      </div>
      <div class="full-row">
        <div class="label">Signal</div>
        <div class="signal" style="gap: 1rem;">
          <div class="signal-bar"><div class="signal-bar-fill" id="signal-bar"></div></div>
          <div class="signal-value" id="signal-value">--</div>
        </div>
      </div>
      <div class="full-row" id="tx-meters" style="display:none;">
        <div class="label">TX Meters</div>
        <div class="meter" style="gap: 1rem; margin-bottom: 0.4rem;">
          <div class="meter-bar"><div class="meter-fill" id="pwr-bar"></div></div>
          <div class="meter-value" id="pwr-value">PWR --</div>
        </div>
        <div class="meter" style="gap: 1rem;">
          <div class="meter-bar"><div class="meter-fill" id="swr-bar"></div></div>
          <div class="meter-value" id="swr-value">SWR --</div>
        </div>
      </div>
      <div id="tx-limit-row" style="display:none;">
        <div class="label">TX Limit</div>
        <div class="inline">
          <input class="status-input" id="tx-limit" type="number" min="0" max="255" step="1" value="" placeholder="--" />
          <button id="tx-limit-btn" type="button">Set</button>
        </div>
        <small>Units depend on rig (percent/watts).</small>
      </div>
    </div>
    <div class="footer">
      <div class="hint" id="power-hint">Connecting…</div>
    </div>
    </div>
  </div>
  <script>
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
        if (now - lastEventAt > 8000) {
          es.close();
          connect();
        }
      }, 4000);
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
  </script>
</body>
</html>
"##;
