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
    // If the active channel was evicted, fall back to channel 0.
    const ids = new Set(vchanChannels.map(c => c.id));
    if (vchanActiveId && !ids.has(vchanActiveId)) {
      vchanActiveId = vchanChannels.length > 0 ? vchanChannels[0].id : null;
    }
    vchanRender();
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
  } catch (e) {
    console.error("vchan: subscribe error", e);
  }
}

// Called by app.js from applyCapabilities().
// Shows the channel picker only for SDR rigs.
function vchanApplyCapabilities(caps) {
  const row = document.getElementById("vchan-row");
  if (!row) return;
  row.style.display = (caps && caps.filter_controls) ? "" : "none";
}

// ---------------------------------------------------------------------------
// Freq / mode interception
// ---------------------------------------------------------------------------

// Returns true when the active channel is a non-primary (virtual) channel.
function vchanIsOnVirtual() {
  if (!vchanActiveId || vchanChannels.length === 0) return false;
  return vchanActiveId !== vchanChannels[0].id;
}

async function vchanSetChannelFreq(freqHz) {
  if (!vchanRigId || !vchanActiveId) return;
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
