// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

// --- Virtual Channels Plugin ---
//
// Handles the `session` and `channels` SSE events emitted by /events and
// provides the channel picker UI (SDR-only, shown when filter_controls is set).

let vchanSessionId = null;
let vchanRigId = null;
let vchanChannels = [];
let vchanActiveId = null;

function vchanFmtFreq(hz) {
  if (!Number.isFinite(hz) || hz <= 0) return "--";
  if (hz >= 1e9) return (hz / 1e9).toFixed(4).replace(/\.?0+$/, "") + "\u202fGHz";
  if (hz >= 1e6) return (hz / 1e6).toFixed(4).replace(/\.?0+$/, "") + "\u202fMHz";
  if (hz >= 1e3) return (hz / 1e3).toFixed(1).replace(/\.?0+$/, "") + "\u202fkHz";
  return hz + "\u202fHz";
}

// Called by app.js when the SSE `session` event arrives.
function vchanHandleSession(data) {
  try {
    const d = JSON.parse(data);
    vchanSessionId = d.session_id || null;
  } catch (e) {
    console.warn("vchan: bad session event", e);
  }
}

// Called by app.js when the SSE `channels` event arrives.
function vchanHandleChannels(data) {
  try {
    const d = JSON.parse(data);
    vchanRigId = d.rig_id || null;
    vchanChannels = d.channels || [];
    // If the active channel was evicted, fall back to channel 0 and reconnect audio.
    const ids = new Set(vchanChannels.map(c => c.id));
    if (vchanActiveId && !ids.has(vchanActiveId)) {
      vchanActiveId = vchanChannels.length > 0 ? vchanChannels[0].id : null;
      vchanReconnectAudio();
    }
    vchanRender();
  } catch (e) {
    console.warn("vchan: bad channels event", e);
  }
}

function vchanRenderLayers() {
  const container = document.getElementById("vchan-freq-layers");
  if (!container) return;
  container.innerHTML = "";

  if (vchanChannels.length === 0) {
    container.style.height = "0";
    return;
  }

  // Sort by frequency ascending so higher-frequency channels get higher z-index.
  const sorted = [...vchanChannels].sort((a, b) => a.freq_hz - b.freq_hz);

  const LAYER_H_PX = 32;
  const STEP_PX = 11; // vertical offset between layers so each peeks below the next
  const totalH = LAYER_H_PX + (sorted.length - 1) * STEP_PX;
  container.style.height = totalH + "px";

  sorted.forEach((ch, i) => {
    const layer = document.createElement("div");
    layer.className = "vchan-freq-layer";
    if (ch.id === vchanActiveId) layer.classList.add("active");

    layer.style.top = (i * STEP_PX) + "px";
    // Higher frequency → higher index → higher z-index (sits on top by default).
    const defaultZ = i + 1;
    layer.style.zIndex = defaultZ;

    layer.textContent = `${ch.index}: ${vchanFmtFreq(ch.freq_hz)} ${ch.mode}`;
    layer.title = `Ch ${ch.index}: ${vchanFmtFreq(ch.freq_hz)} ${ch.mode} · ${ch.subscribers} subscriber${ch.subscribers !== 1 ? "s" : ""}`;

    // Bring hovered layer to the front; restore on leave.
    const maxZ = sorted.length + 10;
    layer.addEventListener("mouseenter", () => { layer.style.zIndex = maxZ; });
    layer.addEventListener("mouseleave", () => { layer.style.zIndex = defaultZ; });

    layer.addEventListener("click", () => {
      if (ch.id !== vchanActiveId) vchanSubscribe(ch.id);
    });

    container.appendChild(layer);
  });
}

function vchanRender() {
  const picker = document.getElementById("vchan-picker");
  if (!picker) return;
  picker.innerHTML = "";

  vchanChannels.forEach(ch => {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.title = `Ch ${ch.index}: ${vchanFmtFreq(ch.freq_hz)} ${ch.mode} · ${ch.subscribers} subscriber${ch.subscribers !== 1 ? "s" : ""}`;
    if (ch.id === vchanActiveId) btn.classList.add("active");

    const label = document.createElement("span");
    label.className = "vchan-label";
    label.textContent = `${ch.index}: ${vchanFmtFreq(ch.freq_hz)} ${ch.mode}`;
    btn.appendChild(label);

    if (!ch.permanent) {
      const del = document.createElement("span");
      del.className = "vchan-del";
      del.textContent = "\u00d7";
      del.title = "Delete channel";
      del.addEventListener("click", e => {
        e.stopPropagation();
        vchanDelete(ch.id);
      });
      btn.appendChild(del);
    }

    btn.addEventListener("click", () => {
      if (ch.id !== vchanActiveId) vchanSubscribe(ch.id);
    });

    picker.appendChild(btn);
  });

  // "+" button — allocate a new channel at the current VFO frequency.
  const addBtn = document.createElement("button");
  addBtn.type = "button";
  addBtn.className = "vchan-add";
  addBtn.textContent = "+";
  addBtn.title = "Allocate new virtual channel at current frequency";
  addBtn.addEventListener("click", vchanAllocate);
  picker.appendChild(addBtn);

  vchanRenderLayers();
  vchanSyncAccentUI();
}

async function vchanAllocate() {
  if (!vchanSessionId || !vchanRigId) return;

  // Use the last known rig frequency and mode as the starting point.
  const freqHz = (typeof lastFreqHz === "number" && lastFreqHz > 0)
    ? lastFreqHz
    : 0;
  const modeEl = document.getElementById("mode");
  const mode = modeEl ? (modeEl.value || "USB") : "USB";

  try {
    const resp = await fetch(`/channels/${encodeURIComponent(vchanRigId)}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ session_id: vchanSessionId, freq_hz: freqHz, mode }),
    });
    if (!resp.ok) {
      const msg = await resp.text().catch(() => String(resp.status));
      console.warn("vchan: allocate failed —", msg);
      return;
    }
    const ch = await resp.json();
    vchanActiveId = ch.id;
    // The SSE `channels` event will trigger vchanRender(); optimistically
    // mark active so the picker feels responsive even before the event arrives.
    vchanRender();
    vchanReconnectAudio();
  } catch (e) {
    console.error("vchan: allocate error", e);
  }
}

async function vchanDelete(channelId) {
  if (!vchanRigId) return;
  try {
    const resp = await fetch(
      `/channels/${encodeURIComponent(vchanRigId)}/${encodeURIComponent(channelId)}`,
      { method: "DELETE" }
    );
    if (!resp.ok) {
      console.warn("vchan: delete failed", resp.status);
    }
    // Channel list updates via SSE `channels` event.
  } catch (e) {
    console.error("vchan: delete error", e);
  }
}

async function vchanSubscribe(channelId) {
  if (!vchanSessionId || !vchanRigId) return;
  try {
    const resp = await fetch(
      `/channels/${encodeURIComponent(vchanRigId)}/${encodeURIComponent(channelId)}/subscribe`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ session_id: vchanSessionId }),
      }
    );
    if (!resp.ok) {
      console.warn("vchan: subscribe failed", resp.status);
      return;
    }
    vchanActiveId = channelId;
    vchanRender();
    vchanSyncModeDisplay();
    vchanReconnectAudio();
  } catch (e) {
    console.error("vchan: subscribe error", e);
  }
}

// Reconnect the audio WebSocket to the appropriate endpoint:
// - virtual channel: /audio?channel_id=<uuid>
// - primary channel: /audio (no param)
// Always updates _audioChannelOverride so that starting audio later
// connects to the correct channel. Only reconnects if RX audio is active.
function vchanReconnectAudio() {
  // Always update the override so startRxAudio picks up the right URL,
  // even when audio isn't currently running.
  const ch = vchanIsOnVirtual() ? vchanActiveChannel() : null;
  if (typeof _audioChannelOverride !== "undefined") {
    _audioChannelOverride = ch ? ch.id : null;
  }
  if (typeof rxActive === "undefined" || !rxActive) return;
  if (typeof stopRxAudio === "function") stopRxAudio();
  // Delay so the server has time to set up the per-channel encoder.
  // The server-side audio_ws handler also polls for up to 2 s, so this
  // just needs to be long enough for the WS upgrade to reach the server.
  setTimeout(() => {
    if (typeof startRxAudio === "function") startRxAudio();
  }, 300);
}

// Called by app.js from applyCapabilities().
// Shows the channel picker only for SDR rigs.
function vchanApplyCapabilities(caps) {
  const row = document.getElementById("vchan-row");
  if (!row) return;
  row.style.display = (caps && caps.filter_controls) ? "" : "none";
}

// ---------------------------------------------------------------------------
// Freq / mode interception + UI accent
// ---------------------------------------------------------------------------

// Returns true when the active channel is a non-primary (virtual) channel.
function vchanIsOnVirtual() {
  if (!vchanActiveId || vchanChannels.length === 0) return false;
  return vchanActiveId !== vchanChannels[0].id;
}

function vchanActiveChannel() {
  return vchanChannels.find(c => c.id === vchanActiveId) || null;
}

// Update the main freq input to show the virtual channel's frequency.
function vchanUpdateFreqDisplay() {
  const ch = vchanActiveChannel();
  if (!ch) return;
  const el = document.getElementById("freq");
  if (!el) return;
  if (typeof formatFreqForStep === "function" && typeof jogUnit !== "undefined") {
    el.value = formatFreqForStep(ch.freq_hz, jogUnit);
  } else {
    el.value = (ch.freq_hz / 1e6).toFixed(6).replace(/\.?0+$/, "");
  }
}

// Sync the mode picker to the active virtual channel's mode.
// Called whenever the active channel changes or the channel list is refreshed.
function vchanSyncModeDisplay() {
  const modeEl = document.getElementById("mode");
  if (!modeEl) return;
  if (vchanIsOnVirtual()) {
    const ch = vchanActiveChannel();
    if (ch && ch.mode) modeEl.value = ch.mode.toUpperCase();
  }
  // When on primary channel, app.js rig-state updates handle the picker.
}

// Sync the BW input to the active virtual channel's bandwidth.
function vchanSyncBwDisplay() {
  if (!vchanIsOnVirtual()) return;
  const ch = vchanActiveChannel();
  if (!ch) return;
  const bwEl = document.getElementById("spectrum-bw-input");
  if (!bwEl) return;
  // bandwidth_hz == 0 means mode-default; derive it from the channel mode.
  let bwHz = ch.bandwidth_hz || 0;
  if (bwHz === 0 && typeof mwDefaultsForMode === "function") {
    bwHz = mwDefaultsForMode(ch.mode)[0] || 0;
  }
  if (bwHz > 0) {
    bwEl.value = (bwHz / 1000).toFixed(3).replace(/\.?0+$/, "");
    if (typeof currentBandwidthHz !== "undefined") {
      currentBandwidthHz = bwHz;
      window.currentBandwidthHz = bwHz;
    } else {
      window.currentBandwidthHz = bwHz;
    }
  }
}

// Add / remove the vchan accent class from the freq and BW inputs.
function vchanSyncAccentUI() {
  const onVirtual = vchanIsOnVirtual();
  const freqEl = document.getElementById("freq");
  const bwEl   = document.getElementById("spectrum-bw-input");
  if (freqEl) freqEl.classList.toggle("vchan-ch-active", onVirtual);
  if (bwEl)   bwEl.classList.toggle("vchan-ch-active", onVirtual);
  if (onVirtual) {
    vchanUpdateFreqDisplay();
    vchanSyncModeDisplay();
    vchanSyncBwDisplay();
  } else if (typeof _origRefreshFreqDisplay === "function") {
    _origRefreshFreqDisplay();
  }
}

// Saved reference to the original refreshFreqDisplay from app.js.
let _origRefreshFreqDisplay = null;

async function vchanSetChannelFreq(freqHz) {
  if (!vchanRigId || !vchanActiveId) return;
  // Validate against current SDR capture window.
  if (typeof lastSpectrumData !== "undefined" && lastSpectrumData &&
      lastSpectrumData.sample_rate > 0) {
    const halfSpan = Number(lastSpectrumData.sample_rate) / 2;
    const center   = Number(lastSpectrumData.center_hz);
    if (Math.abs(freqHz - center) > halfSpan) {
      if (typeof showHint === "function") {
        showHint(
          `Out of SDR bandwidth (center ${(center / 1e6).toFixed(3)} MHz ±${(halfSpan / 1e3).toFixed(0)} kHz)`,
          3000
        );
      }
      return;
    }
  }
  try {
    const resp = await fetch(
      `/channels/${encodeURIComponent(vchanRigId)}/${encodeURIComponent(vchanActiveId)}/freq`,
      {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ freq_hz: Math.round(freqHz) }),
      }
    );
    if (!resp.ok) console.warn("vchan: set freq failed", resp.status);
  } catch (e) {
    console.error("vchan: set freq error", e);
  }
}

async function vchanSetChannelBandwidth(bwHz) {
  if (!vchanRigId || !vchanActiveId) return;
  try {
    const resp = await fetch(
      `/channels/${encodeURIComponent(vchanRigId)}/${encodeURIComponent(vchanActiveId)}/bw`,
      {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ bandwidth_hz: Math.round(bwHz) }),
      }
    );
    if (!resp.ok) console.warn("vchan: set bw failed", resp.status);
  } catch (e) {
    console.error("vchan: set bw error", e);
  }
}

async function vchanSetChannelMode(mode) {
  if (!vchanRigId || !vchanActiveId) return;
  try {
    const resp = await fetch(
      `/channels/${encodeURIComponent(vchanRigId)}/${encodeURIComponent(vchanActiveId)}/mode`,
      {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ mode }),
      }
    );
    if (!resp.ok) console.warn("vchan: set mode failed", resp.status);
  } catch (e) {
    console.error("vchan: set mode error", e);
  }
}

// Called by app.js (applyModeFromPicker) and bookmarks.js (bmApply) before
// sending /set_mode to the server.  Returns true if the change was handled
// by the virtual channel (caller should skip the server request).
window.vchanInterceptMode = async function(mode) {
  if (!vchanIsOnVirtual()) return false;
  await vchanSetChannelMode(mode);
  return true;
};

// Called by app.js bandwidth setters before sending /set_bandwidth to the
// server.  Returns true if the change was handled by the virtual channel.
window.vchanInterceptBandwidth = async function(bwHz) {
  if (!vchanIsOnVirtual()) return false;
  await vchanSetChannelBandwidth(bwHz);
  return true;
};

// Wrap setRigFrequency (defined in app.js, loaded before this file) so that
// frequency changes are redirected to the active virtual channel instead of
// the server when on a non-primary channel.
(function() {
  const _orig = window.setRigFrequency;
  window.setRigFrequency = async function(freqHz) {
    if (vchanIsOnVirtual()) {
      await vchanSetChannelFreq(freqHz);
      return;
    }
    if (typeof _orig === "function") return _orig(freqHz);
  };
})();

// Wrap refreshFreqDisplay so the main freq field stays in sync with the
// active virtual channel's frequency (SSE rig-state updates would otherwise
// constantly overwrite it with channel 0's freq).
(function() {
  _origRefreshFreqDisplay = window.refreshFreqDisplay;
  window.refreshFreqDisplay = function() {
    if (vchanIsOnVirtual()) {
      vchanUpdateFreqDisplay();
      return;
    }
    if (typeof _origRefreshFreqDisplay === "function") _origRefreshFreqDisplay();
  };
})();
