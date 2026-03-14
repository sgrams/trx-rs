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
let schedulerReleaseState = null;
let schedulerReleasePollTimer = null;

function vchanFmtFreq(hz) {
  if (!Number.isFinite(hz) || hz <= 0) return "--";
  if (hz >= 1e9) return (hz / 1e9).toFixed(4).replace(/\.?0+$/, "") + "\u202fGHz";
  if (hz >= 1e6) return (hz / 1e6).toFixed(4).replace(/\.?0+$/, "") + "\u202fMHz";
  if (hz >= 1e3) return (hz / 1e3).toFixed(1).replace(/\.?0+$/, "") + "\u202fkHz";
  return hz + "\u202fHz";
}

function schedulerReleaseSummaryText(state) {
  if (!state) return "Scheduler is controlling the rig.";
  const connected = Number(state.connected_sessions) || 0;
  const released = Number(state.released_sessions) || 0;
  if (connected === 0) return "Scheduler can control the rig.";
  if (state.all_released) {
    return connected === 1
      ? "Scheduler is controlling the rig."
      : `Scheduler is controlling the rig for all ${connected} users.`;
  }
  if (!state.current_session_released) {
    const othersReleased = Math.max(released, 0);
    return othersReleased > 0
      ? `You are holding control. ${othersReleased} other user${othersReleased === 1 ? "" : "s"} already released it.`
      : "You are holding control. Release it to return control to the scheduler.";
  }
  const blocking = Math.max(connected - released, 0);
  return blocking > 0
    ? `Scheduler is waiting for ${blocking} user${blocking === 1 ? "" : "s"} to stop manual tuning.`
    : "Scheduler can control the rig.";
}

function vchanRenderSchedulerRelease() {
  const btn = document.getElementById("scheduler-release-btn");
  const status = document.getElementById("scheduler-release-status");
  if (!btn || !status) return;
  const currentReleased = !!(schedulerReleaseState && schedulerReleaseState.current_session_released);
  btn.disabled = !vchanSessionId || currentReleased;
  btn.classList.toggle("active", !currentReleased);
  btn.textContent = "Release to Scheduler";
  status.textContent = schedulerReleaseSummaryText(schedulerReleaseState);
}

async function vchanPollSchedulerRelease() {
  if (!vchanSessionId) {
    schedulerReleaseState = null;
    vchanRenderSchedulerRelease();
    return;
  }
  try {
    const resp = await fetch(`/scheduler-control?session_id=${encodeURIComponent(vchanSessionId)}`);
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    schedulerReleaseState = await resp.json();
    vchanRenderSchedulerRelease();
  } catch (e) {
    console.error("scheduler release status failed", e);
  }
}

function vchanStartSchedulerReleasePolling() {
  if (schedulerReleasePollTimer) {
    clearInterval(schedulerReleasePollTimer);
  }
  schedulerReleasePollTimer = setInterval(vchanPollSchedulerRelease, 10000);
}

async function vchanToggleSchedulerRelease() {
  if (!vchanSessionId) return;
  const rigId = vchanRigId || (typeof lastActiveRigId !== "undefined" ? lastActiveRigId : null);
  try {
    const resp = await fetch("/scheduler-control", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ session_id: vchanSessionId, released: true, rig_id: rigId }),
    });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    schedulerReleaseState = await resp.json();
    vchanRenderSchedulerRelease();
  } catch (e) {
    console.error("scheduler release toggle failed", e);
  }
}

async function vchanTakeSchedulerControl() {
  if (!vchanSessionId) return;
  if (schedulerReleaseState && !schedulerReleaseState.current_session_released) return;
  try {
    const resp = await fetch("/scheduler-control", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ session_id: vchanSessionId, released: false }),
    });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    schedulerReleaseState = await resp.json();
    vchanRenderSchedulerRelease();
  } catch (e) {
    console.error("scheduler control takeover failed", e);
  }
}

// Called by app.js when the SSE `session` event arrives.
function vchanHandleSession(data) {
  try {
    const d = JSON.parse(data);
    vchanSessionId = d.session_id || null;
    vchanPollSchedulerRelease();
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
    vchanRenderSchedulerRelease();
    if (typeof renderRdsOverlays === "function") renderRdsOverlays();
  } catch (e) {
    console.warn("vchan: bad channels event", e);
  }
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

  vchanSyncAccentUI();
  if (typeof updateDocumentTitle === "function" && typeof activeChannelRds === "function") {
    updateDocumentTitle(activeChannelRds());
  }
  vchanRenderSchedulerRelease();
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
    await vchanTakeSchedulerControl();
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
  const picker = document.getElementById("vchan-picker");
  if (!picker) return;
  picker.style.display = (caps && caps.filter_controls) ? "" : "none";
  vchanRenderSchedulerRelease();
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
  const modeUpper = (modeEl.value || "").toUpperCase();
  if (typeof lastModeName !== "undefined") {
    if (modeUpper === "WFM" && lastModeName !== "WFM") {
      if (typeof setJogDivisor === "function") setJogDivisor(10);
      if (typeof resetRdsDisplay === "function") resetRdsDisplay();
    } else if (modeUpper !== "WFM" && lastModeName === "WFM") {
      if (typeof resetRdsDisplay === "function") resetRdsDisplay();
    }
    lastModeName = modeUpper;
  }
  if (typeof updateWfmControls === "function") updateWfmControls();
  if (typeof updateSdrSquelchControlVisibility === "function") {
    updateSdrSquelchControlVisibility();
  }
  if (typeof refreshRdsUi === "function") {
    refreshRdsUi();
  } else if (typeof positionRdsPsOverlay === "function") {
    positionRdsPsOverlay();
  }
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
  if (typeof updateDocumentTitle === "function" && typeof activeChannelRds === "function") {
    updateDocumentTitle(activeChannelRds());
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
    await vchanTakeSchedulerControl();
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
    await vchanTakeSchedulerControl();
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
    await vchanTakeSchedulerControl();
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
    await vchanTakeSchedulerControl();
    if (typeof _orig === "function") return _orig(freqHz);
  };
})();

(function initSchedulerReleaseControl() {
  const btn = document.getElementById("scheduler-release-btn");
  if (btn) {
    btn.addEventListener("click", () => {
      vchanToggleSchedulerRelease();
    });
  }
  vchanStartSchedulerReleasePolling();
  vchanRenderSchedulerRelease();
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
