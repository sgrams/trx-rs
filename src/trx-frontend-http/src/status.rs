// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

const PKG_NAME: &str = env!("CARGO_PKG_NAME");
const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn index_html() -> String {
    INDEX_HTML_TEMPLATE
        .replace("{pkg}", PKG_NAME)
        .replace("{ver}", PKG_VERSION)
}

const INDEX_HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>{pkg} v{ver} status</title>
  <style>
    body { font-family: sans-serif; margin: 2rem; }
    .app-meta { display: flex; gap: 0.75rem; align-items: baseline; margin-bottom: 1rem; }
    .pill { padding: 0.1rem 0.6rem; border-radius: 999px; background: #eef3ff; color: #1f3c88; font-weight: 600; font-size: 0.95rem; border: 1px solid #c7d6ff; }
    .muted { color: #667; font-size: 0.95rem; }
    .card { border: 1px solid #ddd; border-radius: 8px; padding: 1rem 1.5rem; max-width: 520px; }
    .label { color: #555; font-size: 0.9rem; }
    .value { font-size: 1.4rem; margin-bottom: 0.5rem; }
    .status { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 1rem; }
    input.status-input, select.status-input { width: 100%; padding: 0.45rem 0.5rem; font-size: 1rem; border: 1px solid #ccc; border-radius: 6px; background: #fff; }
    .controls { margin-top: 1rem; display: flex; gap: 0.75rem; align-items: center; flex-wrap: wrap; }
    button { padding: 0.5rem 0.9rem; border-radius: 6px; border: 1px solid #999; background: #f3f3f3; cursor: pointer; }
    button:disabled { opacity: 0.6; cursor: not-allowed; }
    .hint { color: #666; font-size: 0.85rem; }
    .inline { display: flex; gap: 0.5rem; align-items: center; }
    .section-title { margin-top: 1rem; font-size: 1.05rem; font-weight: 600; }
    small { color: #666; }
  </style>
</head>
<body>
  <div class="card">
    <h2>Transceiver Status</h2>
    <div class="section-title">Live readings</div>
    <div class="status">
      <div>
        <div class="label">Frequency (Hz)</div>
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
            <option>USB</option><option>LSB</option><option>CW</option><option>CWR</option><option>AM</option><option>FM</option><option>WFM</option><option>DIG</option><option>PKT</option>
          </select>
          <button id="mode-apply" type="button">Set</button>
        </div>
      </div>
      <div>
        <div class="label">Transmit</div>
        <div class="inline">
          <input class="status-input" id="tx" type="text" readonly value="--" />
          <button id="ptt-btn" type="button">Toggle PTT</button>
        </div>
      </div>
      <div>
        <div class="label">VFO</div>
        <input class="status-input" id="vfo" type="text" readonly value="--" />
      </div>
      <div>
        <div class="label">Power</div>
        <input class="status-input" id="power" type="text" readonly value="--" />
      </div>
      <div>
        <div class="label">Band</div>
        <input class="status-input" id="band" type="text" readonly value="--" />
      </div>
      <div>
        <div class="label">TX Limit</div>
        <div class="inline">
          <input class="status-input" id="tx-limit" type="number" min="0" max="255" step="1" value="--" />
          <button id="tx-limit-btn" type="button">Set</button>
        </div>
        <small>Units depend on rig (percent/watts).</small>
      </div>
    </div>
    <div class="controls">
      <button id="power-btn" type="button">Toggle Power</button>
      <button id="vfo-btn" type="button">Toggle VFO</button>
      <div class="hint" id="power-hint">Connecting…</div>
    </div>
    <div class="label" id="state">Connecting…</div>
  </div>
<div class="app-meta">
  <div class="pill">{pkg} v{ver}</div>
  <div class="muted">Simple HTTP status UI</div>
</div>
  <script>
    const freqEl = document.getElementById("freq");
    const modeEl = document.getElementById("mode");
    const txEl = document.getElementById("tx");
    const bandEl = document.getElementById("band");
    const powerEl = document.getElementById("power");
    const powerBtn = document.getElementById("power-btn");
    const powerHint = document.getElementById("power-hint");
    const stateEl = document.getElementById("state");
    const vfoEl = document.getElementById("vfo");
    const vfoBtn = document.getElementById("vfo-btn");
    const pttBtn = document.getElementById("ptt-btn");
    const freqBtn = document.getElementById("freq-apply");
    const modeBtn = document.getElementById("mode-apply");
    const txLimitInput = document.getElementById("tx-limit");
    const txLimitBtn = document.getElementById("tx-limit-btn");

    let lastControl;
    let lastTxEn = null;

    function render(update) {
      if (update.status && update.status.freq && typeof update.status.freq.hz === "number") {
        freqEl.value = update.status.freq.hz.toLocaleString();
      }
      if (update.status && update.status.mode) {
        modeEl.value = update.status.mode;
      }
      if (update.status && typeof update.status.tx_en === "boolean") {
        txEl.value = update.status.tx_en ? "ON" : "OFF";
        lastTxEn = update.status.tx_en;
        pttBtn.textContent = update.status.tx_en ? "PTT Off" : "PTT On";
      }
      if (update.status && update.status.vfo && Array.isArray(update.status.vfo.entries)) {
        const entries = update.status.vfo.entries;
        const activeIdx = Number.isInteger(update.status.vfo.active) ? update.status.vfo.active : null;
        const parts = entries.map((entry, idx) => {
          const hz = entry && entry.freq && typeof entry.freq.hz === "number" ? entry.freq.hz : null;
          if (hz === null) return null;
          const mark = activeIdx === idx ? " *" : "";
          return `${entry.name || `VFO ${idx + 1}`}: ${hz.toLocaleString()} Hz${mark}`;
        }).filter(Boolean);
        vfoEl.value = parts.join(" | ") || "--";
      } else {
        vfoEl.value = "--";
      }
      if (typeof update.band === "string") {
        bandEl.value = update.band;
      } else {
        bandEl.value = "--";
      }
      if (typeof update.enabled === "boolean") {
        powerEl.value = update.enabled ? "ON" : "OFF";
        powerBtn.disabled = false;
        powerBtn.textContent = update.enabled ? "Power Off" : "Power On";
        powerHint.textContent = "Ready";
      } else {
        powerEl.value = "--";
        powerBtn.disabled = true;
        powerBtn.textContent = "Toggle Power";
        powerHint.textContent = "State unknown";
      }
      lastControl = update.enabled;

      if (update.status && update.status.tx && typeof update.status.tx.limit === "number") {
        txLimitInput.value = update.status.tx.limit;
      }
    }

    function connect() {
      const es = new EventSource("/events");
      es.onopen = () => stateEl.textContent = "Live updates";
      es.onmessage = (evt) => {
        try {
          const data = JSON.parse(evt.data);
          render(data);
        } catch (e) {
          console.error("Bad event data", e);
        }
      };
      es.onerror = () => {
        stateEl.textContent = "Disconnected, retrying…";
        es.close();
        setTimeout(connect, 1000);
      };
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
      const raw = freqEl.value.replace(/[^0-9]/g, "");
      if (!raw) {
        powerHint.textContent = "Freq missing";
        return;
      }
      freqBtn.disabled = true;
      powerHint.textContent = "Setting frequency…";
      try {
        await postPath(`/set_freq?hz=${raw}`);
        powerHint.textContent = "Freq set";
      } catch (err) {
        powerHint.textContent = "Set freq failed";
        console.error(err);
        setTimeout(() => powerHint.textContent = "Ready", 2000);
      } finally {
        freqBtn.disabled = false;
      }
    });

    modeBtn.addEventListener("click", async () => {
      const mode = modeEl.value || "";
      if (!mode) {
        powerHint.textContent = "Mode missing";
        return;
      }
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

    connect();
  </script>
</body>
</html>
"#;
