// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

// Background Decoding Scheduler UI

(function () {
  "use strict";

  // -------------------------------------------------------------------------
  // State
  // -------------------------------------------------------------------------
  let schedulerRole = null;       // "control" | "rx" | null
  let currentRigId = null;
  let currentConfig = null;
  let currentSchedulerStatus = null;
  let bookmarkList = [];          // [{id, name, freq_hz, mode}, ...]
  let statusInterval = null;
  let interleaveTicker = null;
  let schedulerStepPending = false;
  let schEntryEditIdx = null;     // null = adding, number = editing that index
  let schedulerDirty = false;     // true when unsaved changes exist
  // Satellite entry editing state moved to sat-scheduler.js

  // -------------------------------------------------------------------------
  // Init
  // -------------------------------------------------------------------------
  function initScheduler(rigId, role) {
    schedulerRole = role;
    currentRigId = rigId || null;
    if (currentRigId) loadScheduler();
    startStatusPolling();
    startInterleaveTicker();
  }

  function destroyScheduler() {
    if (statusInterval) {
      clearInterval(statusInterval);
      statusInterval = null;
    }
    if (interleaveTicker) {
      clearInterval(interleaveTicker);
      interleaveTicker = null;
    }
  }

  // -------------------------------------------------------------------------
  // Active rig (mirrors top-bar rig picker in app.js)
  // -------------------------------------------------------------------------
  function setSchedulerRig(rigId) {
    const nextRigId = rigId || null;
    if (nextRigId === currentRigId) return;
    currentRigId = nextRigId;
    renderSchedulerInterleaveStatus();
    if (!currentRigId) return;
    loadScheduler();
    pollStatus();
  }

  // -------------------------------------------------------------------------
  // API helpers
  // -------------------------------------------------------------------------
  function apiGetScheduler(rigId) {
    return fetch("/scheduler/" + encodeURIComponent(rigId)).then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function apiPutScheduler(rigId, config) {
    return fetch("/scheduler/" + encodeURIComponent(rigId), {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(config),
    }).then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function apiDeleteScheduler(rigId) {
    return fetch("/scheduler/" + encodeURIComponent(rigId), {
      method: "DELETE",
    }).then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function apiGetStatus(rigId) {
    return fetch("/scheduler/" + encodeURIComponent(rigId) + "/status").then(
      function (r) {
        if (!r.ok) throw new Error("HTTP " + r.status);
        return r.json();
      }
    );
  }

  function apiActivateSchedulerEntry(rigId, entryId) {
    return fetch("/scheduler/" + encodeURIComponent(rigId) + "/activate", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ entry_id: entryId }),
    }).then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function apiGetBookmarks() {
    // Fetch merged general + rig-specific bookmarks in a single request.
    var url = currentRigId
      ? "/bookmarks?scope=" + encodeURIComponent(currentRigId)
      : "/bookmarks";
    return fetch(url).then(function (r) { return r.ok ? r.json() : []; });
  }

  // -------------------------------------------------------------------------
  // Load config + bookmarks
  // -------------------------------------------------------------------------
  function loadScheduler() {
    const rig = currentRigId;
    if (!rig) return;

    Promise.all([apiGetScheduler(rig), apiGetBookmarks()])
      .then(function ([config, bms]) {
        currentConfig = config;
        bookmarkList = Array.isArray(bms) ? bms : [];
        populateTsBookmarkSelect();
        renderScheduler();
        clearSchedulerDirty();
        renderSchedulerInterleaveStatus();
      })
      .catch(function (e) {
        console.error("scheduler load failed", e);
        renderSchedulerInterleaveStatus();
      });
  }

  // -------------------------------------------------------------------------
  // Status polling
  // -------------------------------------------------------------------------
  function startStatusPolling() {
    if (statusInterval) clearInterval(statusInterval);
    statusInterval = setInterval(pollStatus, 15000);
    pollStatus();
  }

  function startInterleaveTicker() {
    if (interleaveTicker) clearInterval(interleaveTicker);
    interleaveTicker = setInterval(renderSchedulerInterleaveStatus, 1000);
    renderSchedulerInterleaveStatus();
  }

  function schedulerUtcSeconds() {
    return Math.floor(Date.now() / 1000);
  }

  function schedulerUtcMinuteInfo() {
    const secs = schedulerUtcSeconds();
    const secsIntoDay = ((secs % 86400) + 86400) % 86400;
    return {
      minuteOfDay: Math.floor(secsIntoDay / 60),
      secondOfMinute: secsIntoDay % 60,
    };
  }

  function schedulerEntryIsActive(entry, nowMin) {
    const start = Number(entry && entry.start_min);
    const end = Number(entry && entry.end_min);
    if (!Number.isFinite(start) || !Number.isFinite(end)) return false;
    if (start === end) return true;
    if (start < end) return nowMin >= start && nowMin < end;
    return nowMin >= start || nowMin < end;
  }

  function schedulerEntryCurrentWindowStart(entry, nowMin) {
    const start = Number(entry && entry.start_min);
    const end = Number(entry && entry.end_min);
    if (!Number.isFinite(start) || !Number.isFinite(end)) return Number.NEGATIVE_INFINITY;
    if (start === end) return 0;
    if (start < end) return start;
    return nowMin >= start ? start : (start - 1440);
  }

  function schedulerEntryDisplayName(entry) {
    if (!entry) return "Scheduler entry";
    if (entry.label) return String(entry.label);
    const bookmarkName = bmName(entry.bookmark_id);
    return bookmarkName || "Scheduler entry";
  }

  function schedulerInterleaveState(config) {
    if (!config || config.mode !== "time_span") {
      return { activeEntries: [], currentIndex: -1, remainingSec: 0, cycleMin: 0 };
    }
    const entries = Array.isArray(config.entries) ? config.entries : [];
    const minuteInfo = schedulerUtcMinuteInfo();
    const nowMin = minuteInfo.minuteOfDay;
    const active = entries.filter(function (entry) {
      return schedulerEntryIsActive(entry, nowMin);
    });
    if (active.length === 0) {
      return { activeEntries: [], currentIndex: -1, remainingSec: 0, cycleMin: 0 };
    }
    if (active.length === 1) {
      return { activeEntries: active, currentIndex: 0, remainingSec: 0, cycleMin: 0 };
    }
    const defaultInterleave = Number(config.interleave_min);
    const durations = active.map(function (entry) {
      const own = Number(entry && entry.interleave_min);
      if (Number.isFinite(own) && own > 0) return Math.floor(own);
      if (Number.isFinite(defaultInterleave) && defaultInterleave > 0) return Math.floor(defaultInterleave);
      return 0;
    });
    const cycleMin = durations.reduce(function (sum, value) { return sum + value; }, 0);
    if (!(cycleMin > 0)) {
      return { activeEntries: active, currentIndex: 0, remainingSec: 0, cycleMin: 0 };
    }
    const statusEntryId = currentSchedulerStatus && currentSchedulerStatus.last_entry_id
      ? String(currentSchedulerStatus.last_entry_id)
      : "";
    const statusIndex = statusEntryId
      ? active.findIndex(function (entry) { return String(entry && entry.id || "") === statusEntryId; })
      : -1;
    const statusAppliedUtc = currentSchedulerStatus && Number.isFinite(Number(currentSchedulerStatus.last_applied_utc))
      ? Number(currentSchedulerStatus.last_applied_utc)
      : null;
    if (statusIndex >= 0 && statusAppliedUtc != null) {
      const manualDurationMin = durations[statusIndex];
      const elapsedSec = Math.max(0, schedulerUtcSeconds() - statusAppliedUtc);
      const remainingSec = (manualDurationMin > 0)
        ? Math.max(1, (manualDurationMin * 60) - elapsedSec)
        : 0;
      if (remainingSec > 0) {
        return {
          activeEntries: active,
          currentIndex: statusIndex,
          remainingSec: remainingSec,
          cycleMin: cycleMin,
        };
      }
    }
    const overlapStart = active.reduce(function (maxStart, entry) {
      return Math.max(maxStart, schedulerEntryCurrentWindowStart(entry, nowMin));
    }, Number.NEGATIVE_INFINITY);
    if (!Number.isFinite(overlapStart)) {
      return { activeEntries: active, currentIndex: 0, remainingSec: 0, cycleMin: 0 };
    }
    const nowMinPrecise = minuteInfo.minuteOfDay + (minuteInfo.secondOfMinute / 60);
    const posMin = ((nowMinPrecise - overlapStart) % cycleMin + cycleMin) % cycleMin;
    let cumulative = 0;
    let slotStart = 0;
    let currentIndex = 0;
    let currentDuration = 0;
    for (let i = 0; i < durations.length; i += 1) {
      const nextCumulative = cumulative + durations[i];
      if (posMin < nextCumulative) {
        slotStart = cumulative;
        cumulative = nextCumulative;
        currentIndex = i;
        currentDuration = durations[i];
        break;
      }
      cumulative = nextCumulative;
    }
    if (!(currentDuration > 0)) {
      return { activeEntries: active, currentIndex: 0, remainingSec: 0, cycleMin: 0 };
    }
    const elapsedSlotSec = Math.max(0, Math.floor((posMin - slotStart) * 60));
    const remainingSec = Math.max(1, (currentDuration * 60) - elapsedSlotSec);
    return {
      activeEntries: active,
      currentIndex: currentIndex,
      remainingSec: remainingSec,
      cycleMin: cycleMin,
    };
  }

  function renderSchedulerInterleaveStatus() {
    const wrap = document.getElementById("scheduler-cycle-status");
    if (!wrap) return;

    const state = schedulerInterleaveState(currentConfig);
    const isActive = state.activeEntries.length > 1 && state.cycleMin > 0;

    wrap.style.display = isActive ? "" : "none";

    if (isActive) {
      var activeName = schedulerEntryDisplayName(state.activeEntries[state.currentIndex]);
      var totalSlotSec = state.cycleMin > 0
        ? (state.cycleMin * 60) / state.activeEntries.length
        : 0;
      var elapsedPct = totalSlotSec > 0
        ? Math.min(100, Math.max(0, ((totalSlotSec - state.remainingSec) / totalSlotSec) * 100))
        : 0;

      var ringFill = document.getElementById("interleave-ring-fill");
      if (ringFill) ringFill.setAttribute("stroke-dashoffset", String(100 - elapsedPct));

      var nameEl = document.getElementById("interleave-active-name");
      if (nameEl) nameEl.textContent = activeName;

      var countdownEl = document.getElementById("interleave-countdown");
      if (countdownEl) countdownEl.textContent = "next in " + state.remainingSec + "s · " + state.cycleMin + "m cycle";
    }

    // Also update the timeline needle if visible
    renderTimelineNeedle();
    renderSchedulerStepControls();
  }

  function renderSchedulerStepControls() {
    const prevBtn = document.getElementById("scheduler-prev-btn");
    const nextBtn = document.getElementById("scheduler-next-btn");
    if (!prevBtn || !nextBtn) return;
    const state = schedulerInterleaveState(currentConfig);
    const enabled =
      schedulerRole === "control" &&
      !!currentRigId &&
      !schedulerStepPending &&
      state.activeEntries.length > 1;
    prevBtn.disabled = !enabled;
    nextBtn.disabled = !enabled;
    const hint = enabled
      ? "Select a different active scheduler entry"
      : "Available only when multiple scheduler entries are active";
    prevBtn.title = hint;
    nextBtn.title = hint;
  }

  function pollStatus() {
    const rig = currentRigId;
    if (!rig) return;
    apiGetStatus(rig)
      .then(function (st) {
        currentSchedulerStatus = st || null;
        renderStatus(st);
        renderSchedulerInterleaveStatus();
        renderActivityLog();
        renderSatPassStatus();
      })
      .catch(function () {});
  }

  function renderStatus(st) {
    const el = document.getElementById("scheduler-status-card");
    if (!el) return;
    if (!st || (!st.active && !st.last_bookmark_id)) {
      el.textContent = "No activity yet.";
      return;
    }
    const statusEntryId = st.last_entry_id ? String(st.last_entry_id) : "";
    const entry = statusEntryId && currentConfig && Array.isArray(currentConfig.entries)
      ? currentConfig.entries.find(function (item) { return String(item && item.id || "") === statusEntryId; })
      : null;
    const name = entry
      ? schedulerEntryDisplayName(entry)
      : (st.last_bookmark_name || st.last_bookmark_id || "—");
    let ts = "";
    if (st.last_applied_utc) {
      const d = new Date(st.last_applied_utc * 1000);
      ts = " at " + d.toUTCString();
    }
    const satLabel = st.active_satellite
      ? " [SAT: " + st.active_satellite + "]"
      : "";
    var details = "";
    if (st.freq_hz) {
      details += formatFreq(st.freq_hz);
      if (st.mode) details += " \u00B7 " + st.mode;
      if (st.active_decoders && st.active_decoders.length > 0) {
        details += " \u00B7 " + st.active_decoders.join(", ") + " active";
      }
    }
    if (details) {
      el.innerHTML = "Last applied: " + escHtml(name) + satLabel + ts +
        '<br><span class="sch-status-detail">' + escHtml(details) + '</span>';
    } else {
      el.textContent = "Last applied: " + name + satLabel + ts;
    }
  }

  // -------------------------------------------------------------------------
  // Activity log
  // -------------------------------------------------------------------------
  function apiGetSchedulerLog(rigId) {
    return fetch("/scheduler/" + encodeURIComponent(rigId) + "/log").then(function (r) {
      return r.ok ? r.json() : [];
    });
  }

  function renderActivityLog() {
    var wrap = document.getElementById("scheduler-activity-log-wrap");
    var container = document.getElementById("scheduler-activity-log");
    if (!wrap || !container || !currentRigId) return;

    apiGetSchedulerLog(currentRigId).then(function (entries) {
      if (!entries || entries.length === 0) {
        wrap.style.display = "none";
        return;
      }
      wrap.style.display = "";
      var html = entries.slice().reverse().map(function (e) {
        var d = new Date(e.utc * 1000);
        var ts = d.toUTCString();
        var action = e.action || "unknown";
        var label = e.entry_label || "";
        var bm = e.bookmark_name || "";
        return '<div class="sch-log-entry">' +
          '<span class="sch-log-time">' + escHtml(ts) + '</span> ' +
          '<span class="sch-log-action">' + escHtml(action) + '</span> ' +
          (bm ? '<span class="sch-log-bm">' + escHtml(bm) + '</span>' : '') +
          (label ? ' <span class="sch-log-label">(' + escHtml(label) + ')</span>' : '') +
          '</div>';
      }).join("");
      container.innerHTML = html;
    }).catch(function () {});
  }

  // -------------------------------------------------------------------------
  // Render the full scheduler panel
  // -------------------------------------------------------------------------
  function renderScheduler() {
    const panel = document.getElementById("scheduler-panel");
    if (!panel) return;

    const mode = (currentConfig && currentConfig.mode) || "disabled";
    const isControl = schedulerRole === "control";

    // Mode selector
    setSelected("scheduler-mode-select", mode);

    // Show/hide main-view scheduler controls (visible when base mode active OR satellites enabled)
    const satEnabled = currentConfig && currentConfig.satellites && currentConfig.satellites.enabled;
    const controlRow = document.querySelector(".scheduler-control-row");
    if (controlRow) controlRow.style.display = (mode !== "disabled" || satEnabled) ? "" : "none";

    // Show/hide sections
    const glSection = document.getElementById("scheduler-grayline-section");
    const tsSection = document.getElementById("scheduler-timespan-section");
    if (glSection) glSection.style.display = mode === "grayline" ? "" : "none";
    if (tsSection) tsSection.style.display = mode === "time_span" ? "" : "none";

    // Satellite overlay (always visible — independent of mode)
    renderSatelliteSection();

    // Grayline inputs
    if (mode === "grayline" && currentConfig && currentConfig.grayline) {
      const gl = currentConfig.grayline;
      // Prefer saved value; fall back to server coordinates from app.js globals.
      const lat = gl.lat != null ? gl.lat : (typeof serverLat !== "undefined" ? serverLat : "");
      const lon = gl.lon != null ? gl.lon : (typeof serverLon !== "undefined" ? serverLon : "");
      setInputValue("scheduler-gl-lat", lat != null ? lat : "");
      setInputValue("scheduler-gl-lon", lon != null ? lon : "");
      var gridEl = document.getElementById("scheduler-gl-grid");
      if (gridEl && lat !== "" && lon !== "") {
        gridEl.value = latLonToGrid(lat, lon);
      }
      setInputValue("scheduler-gl-window", gl.transition_window_min != null ? gl.transition_window_min : 20);
      renderBookmarkSelect("scheduler-gl-dawn", gl.dawn_bookmark_id);
      renderBookmarkSelect("scheduler-gl-day", gl.day_bookmark_id);
      renderBookmarkSelect("scheduler-gl-dusk", gl.dusk_bookmark_id);
      renderBookmarkSelect("scheduler-gl-night", gl.night_bookmark_id);
    } else if (mode === "grayline") {
      // No saved grayline config yet — pre-fill coords from server if available.
      const lat = typeof serverLat !== "undefined" ? serverLat : "";
      const lon = typeof serverLon !== "undefined" ? serverLon : "";
      setInputValue("scheduler-gl-lat", lat != null ? lat : "");
      setInputValue("scheduler-gl-lon", lon != null ? lon : "");
      var gridEl2 = document.getElementById("scheduler-gl-grid");
      if (gridEl2 && lat !== "" && lon !== "") {
        gridEl2.value = latLonToGrid(lat, lon);
      }
      setInputValue("scheduler-gl-window", 20);
      renderBookmarkSelect("scheduler-gl-dawn", null);
      renderBookmarkSelect("scheduler-gl-day", null);
      renderBookmarkSelect("scheduler-gl-dusk", null);
      renderBookmarkSelect("scheduler-gl-night", null);
    } else {
      renderBookmarkSelect("scheduler-gl-dawn", null);
      renderBookmarkSelect("scheduler-gl-day", null);
      renderBookmarkSelect("scheduler-gl-dusk", null);
      renderBookmarkSelect("scheduler-gl-night", null);
    }

    // Interleave input
    const ilEl = document.getElementById("scheduler-ts-interleave");
    if (ilEl) {
      const il = currentConfig && currentConfig.interleave_min;
      ilEl.value = il ? il : "";
    }

    // TimeSpan entries
    renderTimespanEntries();

    // Enable/disable controls
    const formEls = panel.querySelectorAll("input, select, button.sch-write");
    formEls.forEach(function (el) {
      el.disabled = !isControl;
    });
    const saveBtn = document.getElementById("scheduler-save-btn");
    if (saveBtn) {
      saveBtn.style.display = isControl ? "" : "none";
    }
    const resetBtn = document.getElementById("scheduler-reset-btn");
    if (resetBtn) {
      resetBtn.style.display = isControl ? "" : "none";
    }
  }

  function setSelected(id, value) {
    const el = document.getElementById(id);
    if (el) el.value = value;
  }

  function setInputValue(id, value) {
    const el = document.getElementById(id);
    if (el) el.value = value;
  }

  function renderBookmarkSelect(id, selectedId) {
    const sel = document.getElementById(id);
    if (!sel) return;
    sel.innerHTML = '<option value="">— none —</option>';
    bookmarkList.forEach(function (bm) {
      const opt = document.createElement("option");
      opt.value = bm.id;
      opt.textContent = bm.name + " (" + formatFreq(bm.freq_hz) + " " + bm.mode + ")";
      if (bm.id === selectedId) opt.selected = true;
      sel.appendChild(opt);
    });
  }

  function formatFreq(hz) {
    if (hz >= 1e6) return (hz / 1e6).toFixed(3) + " MHz";
    if (hz >= 1e3) return (hz / 1e3).toFixed(1) + " kHz";
    return hz + " Hz";
  }

  // -------------------------------------------------------------------------
  // Entry form (inline card below Add Entry button)
  // -------------------------------------------------------------------------
  function schOpenEntryForm(entry, idx) {
    schEntryEditIdx = (idx != null) ? idx : null;

    const titleEl = document.getElementById("sch-entry-form-title");
    if (titleEl) titleEl.textContent = entry ? "Edit Entry" : "Add Entry";

    const startEl = document.getElementById("scheduler-ts-start");
    const endEl = document.getElementById("scheduler-ts-end");
    const bmEl = document.getElementById("scheduler-ts-bookmark");
    const labelEl = document.getElementById("scheduler-ts-label");
    const ilEl = document.getElementById("scheduler-ts-entry-interleave");
    const centerHzEl = document.getElementById("scheduler-ts-center-hz");

    if (startEl) startEl.value = entry ? minToHHMM(entry.start_min) : "";
    if (endEl) endEl.value = entry ? minToHHMM(entry.end_min) : "";
    if (bmEl) bmEl.value = entry ? (entry.bookmark_id || "") : "";
    if (labelEl) labelEl.value = entry ? (entry.label || "") : "";
    if (ilEl) ilEl.value = entry && entry.interleave_min ? entry.interleave_min : "";
    if (centerHzEl) centerHzEl.value = entry && entry.center_hz ? entry.center_hz : "";

    const recordEl = document.getElementById("scheduler-ts-entry-record");
    if (recordEl) recordEl.checked = !!(entry && entry.record);

    pendingExtraBmIds = entry && Array.isArray(entry.bookmark_ids) ? entry.bookmark_ids.slice() : [];
    renderExtraBmList();

    const wrap = document.getElementById("sch-entry-form-wrap");
    if (wrap) {
      wrap.style.display = "block";
      if (startEl) startEl.focus();
    }
  }

  function schCloseEntryForm() {
    const wrap = document.getElementById("sch-entry-form-wrap");
    if (wrap) wrap.style.display = "none";
    schEntryEditIdx = null;
    pendingExtraBmIds = [];
  }

  function schEntryFormSubmit(e) {
    e.preventDefault();

    const startEl = document.getElementById("scheduler-ts-start");
    const endEl = document.getElementById("scheduler-ts-end");
    const bmEl = document.getElementById("scheduler-ts-bookmark");
    const labelEl = document.getElementById("scheduler-ts-label");
    const ilEl = document.getElementById("scheduler-ts-entry-interleave");
    const centerHzEl = document.getElementById("scheduler-ts-center-hz");
    if (!startEl || !endEl || !bmEl) return;

    const bmId = bmEl.value;
    if (!bmId) {
      alert("Please select a primary bookmark.");
      return;
    }

    const startMin = hhmmToMin(startEl.value);
    const endMin = hhmmToMin(endEl.value);
    const label = labelEl ? labelEl.value.trim() : "";
    const ilVal = ilEl ? parseInt(ilEl.value, 10) : NaN;
    const entryInterleave = !isNaN(ilVal) && ilVal > 0 ? ilVal : null;
    const centerHzRaw = centerHzEl ? parseInt(centerHzEl.value, 10) : NaN;
    const centerHz = !isNaN(centerHzRaw) && centerHzRaw > 0 ? centerHzRaw : null;
    const extraBmIds = pendingExtraBmIds.slice();

    if (!currentConfig) {
      currentConfig = { remote: currentRigId, mode: "time_span", entries: [] };
    }
    if (!currentConfig.entries) currentConfig.entries = [];

    const recordCb = document.getElementById("scheduler-ts-entry-record");
    const entryRecord = recordCb ? recordCb.checked : false;

    const entryData = {
      start_min: startMin,
      end_min: endMin,
      bookmark_id: bmId,
      label: label || null,
      interleave_min: entryInterleave,
      center_hz: centerHz,
      bookmark_ids: extraBmIds,
      record: entryRecord,
    };

    if (schEntryEditIdx !== null) {
      const existing = currentConfig.entries[schEntryEditIdx];
      entryData.id = existing ? existing.id : ("ts_" + Date.now().toString(36));
      currentConfig.entries[schEntryEditIdx] = entryData;
    } else {
      entryData.id = "ts_" + Date.now().toString(36);
      currentConfig.entries.push(entryData);
    }

    schCloseEntryForm();
    renderTimespanEntries();
    markSchedulerDirty();
  }

  // -------------------------------------------------------------------------
  // 24h Timeline Bar
  // -------------------------------------------------------------------------
  var TIMELINE_COLORS = ["#38bdf8", "#f59e0b", "#a78bfa", "#34d399", "#fb7185", "#60a5fa"];

  function renderTimeline() {
    var container = document.getElementById("scheduler-ts-timeline");
    if (!container) return;
    var entries = currentConfig && Array.isArray(currentConfig.entries) ? currentConfig.entries : [];
    if (entries.length === 0) {
      container.innerHTML = "";
      return;
    }

    var W = 1000;
    var H = 80;
    var BAR_Y = 6;
    var BAR_H = 30;
    var TICK_Y = BAR_Y + BAR_H + 2;

    var svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ' + W + ' ' + H + '" preserveAspectRatio="none">';

    // Background bar
    svg += '<rect x="0" y="' + BAR_Y + '" width="' + W + '" height="' + BAR_H + '" rx="3" fill="var(--btn-bg)" />';

    // Entry segments
    entries.forEach(function (entry, idx) {
      var start = Number(entry.start_min);
      var end = Number(entry.end_min);
      if (!Number.isFinite(start) || !Number.isFinite(end)) return;
      var color = TIMELINE_COLORS[idx % TIMELINE_COLORS.length];

      if (start === end) {
        // All-day entry
        svg += '<rect class="sch-timeline-seg" x="0" y="' + BAR_Y + '" width="' + W + '" height="' + BAR_H +
          '" rx="3" fill="' + color + '" data-idx="' + idx + '" />';
      } else if (start < end) {
        var x = (start / 1440) * W;
        var w = ((end - start) / 1440) * W;
        svg += '<rect class="sch-timeline-seg" x="' + x.toFixed(1) + '" y="' + BAR_Y + '" width="' + w.toFixed(1) +
          '" height="' + BAR_H + '" fill="' + color + '" data-idx="' + idx + '" />';
      } else {
        // Wrap-around: two segments
        var x1 = (start / 1440) * W;
        var w1 = W - x1;
        svg += '<rect class="sch-timeline-seg" x="' + x1.toFixed(1) + '" y="' + BAR_Y + '" width="' + w1.toFixed(1) +
          '" height="' + BAR_H + '" fill="' + color + '" data-idx="' + idx + '" />';
        var w2 = (end / 1440) * W;
        svg += '<rect class="sch-timeline-seg" x="0" y="' + BAR_Y + '" width="' + w2.toFixed(1) +
          '" height="' + BAR_H + '" fill="' + color + '" data-idx="' + idx + '" />';
      }
    });

    // Interleave stripes for overlapping entries
    var interleaveMin = currentConfig && currentConfig.interleave_min ? Number(currentConfig.interleave_min) : 0;
    if (interleaveMin > 0 && entries.length > 1) {
      // Find overlap regions where 2+ entries are active
      for (var m = 0; m < 1440; m += interleaveMin) {
        var overlapping = [];
        entries.forEach(function (entry, idx) {
          if (schedulerEntryIsActive(entry, m)) {
            overlapping.push(idx);
          }
        });
        if (overlapping.length > 1) {
          var stripeX = (m / 1440) * W;
          var stripeW = Math.max(1, (interleaveMin / 1440) * W);
          // Determine which entry "owns" this stripe via cycle position
          var cyclePos = m % (interleaveMin * overlapping.length);
          var ownerSlot = Math.floor(cyclePos / interleaveMin);
          var ownerIdx = overlapping[ownerSlot % overlapping.length];
          var stripeColor = TIMELINE_COLORS[ownerIdx % TIMELINE_COLORS.length];
          svg += '<rect x="' + stripeX.toFixed(1) + '" y="' + (BAR_Y + BAR_H - 5) + '" width="' + stripeW.toFixed(1) +
            '" height="5" fill="' + stripeColor + '" opacity="0.9" />';
        }
      }
    }

    // Tick marks every 3 hours
    for (var h = 0; h <= 24; h += 3) {
      var tx = (h / 24) * W;
      svg += '<line x1="' + tx.toFixed(1) + '" y1="' + TICK_Y + '" x2="' + tx.toFixed(1) + '" y2="' + (TICK_Y + 5) +
        '" stroke="var(--border-light)" stroke-width="1" />';
      if (h < 24) {
        svg += '<text class="sch-timeline-tick-label" x="' + (tx + 3).toFixed(1) + '" y="' + (TICK_Y + 16) +
          '">' + String(h).padStart(2, "0") + '</text>';
      }
    }

    // Local time ticks
    var LOCAL_TICK_Y = TICK_Y + 18;
    for (var h = 0; h < 24; h += 3) {
      var localMin = h * 60;
      var utcOffset = new Date().getTimezoneOffset(); // offset in minutes (negative for east of UTC)
      var utcMin = (localMin + utcOffset + 1440) % 1440;
      var tx = (utcMin / 1440) * W;
      svg += '<text class="sch-timeline-tick-label sch-timeline-local-tick" x="' + (tx + 3).toFixed(1) + '" y="' + (LOCAL_TICK_Y + 10) +
        '">' + String(h).padStart(2, "0") + 'L</text>';
    }

    // Current time needle
    svg += '<g id="sch-timeline-needle-g">' + timelineNeedleSvg() + '</g>';

    svg += '</svg>';
    container.innerHTML = svg;

    // Wire click events on segments
    container.querySelectorAll(".sch-timeline-seg").forEach(function (seg) {
      seg.addEventListener("click", function () {
        var i = parseInt(seg.getAttribute("data-idx"), 10);
        var entry = currentConfig && currentConfig.entries ? currentConfig.entries[i] : null;
        if (entry) schOpenEntryForm(entry, i);
      });
    });

    // Click-to-add on empty timeline region
    var svgEl = container.querySelector('svg');
    if (svgEl) {
      svgEl.addEventListener('click', function (e) {
        // Only trigger if clicking on the background bar, not on a segment
        if (e.target.classList.contains('sch-timeline-seg')) return;
        var rect = svgEl.getBoundingClientRect();
        var xPct = (e.clientX - rect.left) / rect.width;
        var clickMin = Math.floor(xPct * 1440);
        var startHour = Math.floor(clickMin / 60);
        var startMin = startHour * 60;
        var endMin = ((startHour + 1) % 24) * 60;

        // Pre-fill the entry form with the clicked hour
        schOpenEntryForm(null, null);
        var startEl = document.getElementById('scheduler-ts-start');
        var endEl = document.getElementById('scheduler-ts-end');
        if (startEl) startEl.value = minToHHMM(startMin);
        if (endEl) endEl.value = minToHHMM(endMin);
      });
      svgEl.style.cursor = 'crosshair';
    }
  }

  function timelineNeedleSvg() {
    var info = schedulerUtcMinuteInfo();
    var nowMin = info.minuteOfDay + (info.secondOfMinute / 60);
    var x = (nowMin / 1440) * 1000;
    return '<line class="sch-timeline-needle" x1="' + x.toFixed(1) + '" y1="2" x2="' + x.toFixed(1) + '" y2="38" />' +
      '<polygon class="sch-timeline-needle-head" points="' +
      (x - 3).toFixed(1) + ',2 ' + (x + 3).toFixed(1) + ',2 ' + x.toFixed(1) + ',6" />';
  }

  function renderTimelineNeedle() {
    var g = document.getElementById("sch-timeline-needle-g");
    if (g) g.innerHTML = timelineNeedleSvg();
  }

  // -------------------------------------------------------------------------
  // Inline row editing
  // -------------------------------------------------------------------------
  function schInlineEdit(tr, entry, idx) {
    var bmOptions = bookmarkList.map(function (bm) {
      var sel = bm.id === entry.bookmark_id ? ' selected' : '';
      return '<option value="' + escHtml(bm.id) + '"' + sel + '>' + escHtml(bm.name) + '</option>';
    }).join('');

    tr.innerHTML =
      '<td class="sch-drag-handle" draggable="true" title="Drag to reorder">\u2807</td>' +
      '<td><input type="time" class="status-input sch-inline-input" value="' + minToHHMM(entry.start_min) + '" data-field="start" /></td>' +
      '<td><input type="time" class="status-input sch-inline-input" value="' + minToHHMM(entry.end_min) + '" data-field="end" /></td>' +
      '<td>' + (entry.center_hz ? formatFreq(entry.center_hz) : '\u2014') + '</td>' +
      '<td><select class="status-input sch-inline-input" data-field="bookmark">' + bmOptions + '</select></td>' +
      '<td>' + (Array.isArray(entry.bookmark_ids) && entry.bookmark_ids.length ? entry.bookmark_ids.map(function(id) { return escHtml(bmName(id)); }).join(', ') : '\u2014') + '</td>' +
      '<td><input type="text" class="status-input sch-inline-input" value="' + escHtml(entry.label || '') + '" data-field="label" /></td>' +
      '<td><input type="number" class="status-input sch-inline-input" value="' + (entry.interleave_min || '') + '" min="1" max="60" placeholder="\u2014" data-field="interleave" style="width:4rem;" /></td>' +
      '<td><input type="checkbox" ' + (entry.record ? 'checked' : '') + ' data-field="record" /></td>' +
      '<td><button class="sch-write sch-inline-save" type="button">Save</button><button class="sch-write sch-inline-cancel" type="button">Cancel</button></td>';

    tr.classList.add('sch-inline-editing');

    tr.querySelector('.sch-inline-save').addEventListener('click', function () {
      var startEl = tr.querySelector('[data-field="start"]');
      var endEl = tr.querySelector('[data-field="end"]');
      var bmEl = tr.querySelector('[data-field="bookmark"]');
      var labelEl = tr.querySelector('[data-field="label"]');
      var ilEl = tr.querySelector('[data-field="interleave"]');
      var recEl = tr.querySelector('[data-field="record"]');

      if (bmEl && !bmEl.value) { alert('Please select a bookmark.'); return; }

      entry.start_min = hhmmToMin(startEl.value);
      entry.end_min = hhmmToMin(endEl.value);
      entry.bookmark_id = bmEl.value;
      entry.label = labelEl.value.trim() || null;
      var ilVal = parseInt(ilEl.value, 10);
      entry.interleave_min = (!isNaN(ilVal) && ilVal > 0) ? ilVal : null;
      entry.record = recEl.checked;

      currentConfig.entries[idx] = entry;
      renderTimespanEntries();
      markSchedulerDirty();
    });

    tr.querySelector('.sch-inline-cancel').addEventListener('click', function () {
      renderTimespanEntries();
    });
  }

  // -------------------------------------------------------------------------
  // TimeSpan entries table
  // -------------------------------------------------------------------------
  function renderTimespanEntries() {
    const tbody = document.getElementById("scheduler-ts-tbody");
    if (!tbody) return;
    tbody.innerHTML = "";
    const entries =
      currentConfig && Array.isArray(currentConfig.entries)
        ? currentConfig.entries
        : [];
    entries.forEach(function (entry, idx) {
      const tr = document.createElement("tr");
      if (currentSchedulerStatus && currentSchedulerStatus.last_entry_id &&
          entry.id && String(entry.id) === String(currentSchedulerStatus.last_entry_id)) {
        tr.classList.add("sch-active");
      }
      const il = entry.interleave_min ? String(entry.interleave_min) + " min" : "—";
      const allDay = entry.start_min === entry.end_min;
      const centerCell = entry.center_hz ? formatFreq(entry.center_hz) : "—";
      const extraIds = Array.isArray(entry.bookmark_ids) ? entry.bookmark_ids : [];
      const extraCell = extraIds.length
        ? extraIds.map(function (id) { return escHtml(bmName(id)); }).join(", ")
        : "—";
      tr.innerHTML =
        '<td class="sch-drag-handle" draggable="true" title="Drag to reorder">\u2807</td>' +
        '<td>' + (allDay ? "All day" : minToHHMM(entry.start_min) + ' <span class="sch-local-time">(' + minToLocal(entry.start_min) + ')</span>') + '</td>' +
        '<td>' + (allDay ? "\u2014" : minToHHMM(entry.end_min) + ' <span class="sch-local-time">(' + minToLocal(entry.end_min) + ')</span>') + '</td>' +
        '<td>' + centerCell + '</td>' +
        '<td>' + escHtml(bmName(entry.bookmark_id)) + '</td>' +
        '<td>' + extraCell + '</td>' +
        '<td>' + escHtml(entry.label || "") + '</td>' +
        '<td>' + il + '</td>' +
        '<td>' + (entry.record ? 'Yes' : '') + '</td>' +
        '<td>' +
          '<button class="sch-write sch-edit-btn" data-idx="' + idx + '" type="button">Edit</button>' +
          '<button class="sch-write sch-remove-btn" data-idx="' + idx + '" type="button">Remove</button>' +
        '</td>';
      tbody.appendChild(tr);
    });
    tbody.querySelectorAll(".sch-edit-btn").forEach(function (btn) {
      btn.addEventListener("click", function () {
        const i = parseInt(btn.dataset.idx, 10);
        const entry = currentConfig && currentConfig.entries ? currentConfig.entries[i] : null;
        if (entry) schInlineEdit(btn.closest('tr'), entry, i);
      });
    });
    tbody.querySelectorAll(".sch-remove-btn").forEach(function (btn) {
      btn.addEventListener("click", function () {
        removeEntry(parseInt(btn.dataset.idx, 10));
      });
    });

    // Drag-to-reorder
    (function () {
      var handles = tbody.querySelectorAll('.sch-drag-handle');
      var dragIdx = null;

      handles.forEach(function (handle, idx) {
        var row = handle.parentElement;

        handle.addEventListener('dragstart', function (e) {
          dragIdx = idx;
          row.classList.add('sch-dragging');
          e.dataTransfer.effectAllowed = 'move';
          e.dataTransfer.setData('text/plain', String(idx));
        });

        row.addEventListener('dragover', function (e) {
          e.preventDefault();
          e.dataTransfer.dropEffect = 'move';
          row.classList.add('sch-drag-over');
        });

        row.addEventListener('dragleave', function () {
          row.classList.remove('sch-drag-over');
        });

        row.addEventListener('drop', function (e) {
          e.preventDefault();
          row.classList.remove('sch-drag-over');
          if (dragIdx === null || dragIdx === idx) return;
          var entries = currentConfig.entries;
          var moved = entries.splice(dragIdx, 1)[0];
          entries.splice(idx, 0, moved);
          renderTimespanEntries();
          markSchedulerDirty();
        });

        handle.addEventListener('dragend', function () {
          row.classList.remove('sch-dragging');
          dragIdx = null;
        });
      });
    })();

    renderTimeline();
  }

  function bmName(id) {
    const bm = bookmarkList.find(function (b) { return b.id === id; });
    return bm ? bm.name : String(id || "");
  }

  function minToLocal(min) {
    // Convert UTC minutes-since-midnight to local time string
    var now = new Date();
    var utcMidnight = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()));
    var utcMs = utcMidnight.getTime() + min * 60000;
    var local = new Date(utcMs);
    return String(local.getHours()).padStart(2, "0") + ":" + String(local.getMinutes()).padStart(2, "0");
  }

  function minToHHMM(min) {
    const h = Math.floor(min / 60) % 24;
    const m = min % 60;
    return String(h).padStart(2, "0") + ":" + String(m).padStart(2, "0");
  }

  function hhmmToMin(str) {
    const parts = str.split(":");
    return parseInt(parts[0] || "0", 10) * 60 + parseInt(parts[1] || "0", 10);
  }

  function gridToLatLon(grid) {
    grid = String(grid).toUpperCase().trim();
    if (grid.length < 4) return null;
    var lonField = grid.charCodeAt(0) - 65;
    var latField = grid.charCodeAt(1) - 65;
    var lonSquare = parseInt(grid.charAt(2), 10);
    var latSquare = parseInt(grid.charAt(3), 10);
    if (isNaN(lonSquare) || isNaN(latSquare) || lonField < 0 || lonField > 17 || latField < 0 || latField > 17) return null;
    var lon = lonField * 20 + lonSquare * 2 - 180;
    var lat = latField * 10 + latSquare * 1 - 90;
    if (grid.length >= 6) {
      var lonSub = grid.charCodeAt(4) - 65;
      var latSub = grid.charCodeAt(5) - 65;
      if (lonSub >= 0 && lonSub < 24 && latSub >= 0 && latSub < 24) {
        lon += lonSub * (2 / 24) + (1 / 24);
        lat += latSub * (1 / 24) + (0.5 / 24);
      }
    } else {
      lon += 1; // center of square
      lat += 0.5;
    }
    return { lat: lat, lon: lon };
  }

  function latLonToGrid(lat, lon) {
    lon = parseFloat(lon) + 180;
    lat = parseFloat(lat) + 90;
    if (isNaN(lon) || isNaN(lat)) return "";
    var lonField = String.fromCharCode(65 + Math.floor(lon / 20));
    var latField = String.fromCharCode(65 + Math.floor(lat / 10));
    var lonSquare = Math.floor((lon % 20) / 2);
    var latSquare = Math.floor(lat % 10);
    var lonSub = String.fromCharCode(97 + Math.floor(((lon % 2) / 2) * 24));
    var latSub = String.fromCharCode(97 + Math.floor((lat % 1) * 24));
    return lonField + latField + lonSquare + latSquare + lonSub + latSub;
  }

  function escHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function schedulerSelectRelativeEntry(delta) {
    const state = schedulerInterleaveState(currentConfig);
    if (!currentRigId || schedulerStepPending || state.activeEntries.length <= 1) return;
    const count = state.activeEntries.length;
    const currentIndex = state.currentIndex >= 0 ? state.currentIndex : 0;
    const targetIndex = (currentIndex + delta + count) % count;
    const target = state.activeEntries[targetIndex];
    if (!target || !target.id) return;

    schedulerStepPending = true;
    renderSchedulerStepControls();

    Promise.resolve(typeof vchanTakeSchedulerControl === "function" ? vchanTakeSchedulerControl() : null)
      .then(function () {
        return apiActivateSchedulerEntry(currentRigId, target.id);
      })
      .then(function (status) {
        currentSchedulerStatus = status || null;
        return Promise.resolve(
          typeof vchanToggleSchedulerRelease === "function"
            ? vchanToggleSchedulerRelease()
            : null
        ).then(function () {
          renderStatus(status);
          renderSchedulerInterleaveStatus();
          showSchedulerToast("Selected " + schedulerEntryDisplayName(target) + ".");
          pollStatus();
        });
      })
      .catch(function (e) {
        console.error("scheduler entry selection failed", e);
        showSchedulerToast("Scheduler entry selection failed: " + e.message, true);
      })
      .finally(function () {
        schedulerStepPending = false;
        renderSchedulerStepControls();
      });
  }

  function removeEntry(idx) {
    if (!currentConfig || !currentConfig.entries) return;
    currentConfig.entries.splice(idx, 1);
    renderTimespanEntries();
    markSchedulerDirty();
  }

  // -------------------------------------------------------------------------
  // Bookmark existence check
  // -------------------------------------------------------------------------
  function bookmarkExists(id) {
    if (!id) return true; // null/empty is allowed
    return bookmarkList.some(function (bm) { return bm.id === id; });
  }

  // -------------------------------------------------------------------------
  // Save
  // -------------------------------------------------------------------------
  function saveScheduler() {
    const rig = currentRigId;
    if (!rig) return;

    const modeEl = document.getElementById("scheduler-mode-select");
    const mode = modeEl ? modeEl.value : "disabled";

    const config = {
      remote: rig,
      mode,
      grayline: null,
      entries: [],
    };

    if (mode === "grayline") {
      const lat = parseFloat(document.getElementById("scheduler-gl-lat").value);
      const lon = parseFloat(document.getElementById("scheduler-gl-lon").value);
      const win = parseInt(document.getElementById("scheduler-gl-window").value, 10);
      config.grayline = {
        lat: isNaN(lat) ? 0 : lat,
        lon: isNaN(lon) ? 0 : lon,
        transition_window_min: isNaN(win) ? 20 : win,
        dawn_bookmark_id: selectVal("scheduler-gl-dawn") || null,
        day_bookmark_id: selectVal("scheduler-gl-day") || null,
        dusk_bookmark_id: selectVal("scheduler-gl-dusk") || null,
        night_bookmark_id: selectVal("scheduler-gl-night") || null,
      };
    } else if (mode === "time_span") {
      config.entries =
        currentConfig && currentConfig.entries ? currentConfig.entries : [];
      const ilVal = parseInt(document.getElementById("scheduler-ts-interleave").value, 10);
      config.interleave_min = isNaN(ilVal) || ilVal <= 0 ? null : ilVal;
    }

    // Satellite overlay — saved regardless of base mode.
    config.satellites = collectSatelliteConfig();

    // Validate bookmark existence before saving
    var missingBmErrors = [];
    if (mode === "grayline" && config.grayline) {
      var gl = config.grayline;
      var glFields = [
        ["dawn_bookmark_id", "Grayline dawn"],
        ["day_bookmark_id", "Grayline day"],
        ["dusk_bookmark_id", "Grayline dusk"],
        ["night_bookmark_id", "Grayline night"],
      ];
      glFields.forEach(function (pair) {
        if (!bookmarkExists(gl[pair[0]])) missingBmErrors.push(pair[1] + " (bookmark " + gl[pair[0]] + ")");
      });
    }
    if (mode === "time_span" && Array.isArray(config.entries)) {
      config.entries.forEach(function (entry, idx) {
        var label = entry.label || "Entry #" + (idx + 1);
        if (!bookmarkExists(entry.bookmark_id)) {
          missingBmErrors.push(label + " primary bookmark (" + entry.bookmark_id + ")");
        }
        var extras = Array.isArray(entry.bookmark_ids) ? entry.bookmark_ids : [];
        extras.forEach(function (id) {
          if (!bookmarkExists(id)) {
            missingBmErrors.push(label + " extra channel (" + id + ")");
          }
        });
      });
    }
    if (config.satellites && Array.isArray(config.satellites.entries)) {
      config.satellites.entries.forEach(function (sat, idx) {
        var satLabel = sat.name || "Satellite #" + (idx + 1);
        if (!bookmarkExists(sat.bookmark_id)) {
          missingBmErrors.push(satLabel + " bookmark (" + sat.bookmark_id + ")");
        }
      });
    }
    if (missingBmErrors.length > 0) {
      showSchedulerToast("Missing bookmarks: " + missingBmErrors.join("; "), true);
      return;
    }

    const btn = document.getElementById("scheduler-save-btn");
    if (btn) btn.disabled = true;

    apiPutScheduler(rig, config)
      .then(function (saved) {
        currentConfig = saved;
        renderScheduler();
        clearSchedulerDirty();
        showSchedulerToast("Scheduler saved.");
      })
      .catch(function (e) {
        showSchedulerToast("Save failed: " + e.message, true);
      })
      .finally(function () {
        if (btn) btn.disabled = false;
      });
  }

  function selectVal(id) {
    const el = document.getElementById(id);
    return el ? el.value : "";
  }

  function resetScheduler() {
    const rig = currentRigId;
    if (!rig) return;
    if (!confirm("Reset scheduler for this rig to Disabled?")) return;

    apiDeleteScheduler(rig)
      .then(function () {
        currentConfig = {
          remote: rig,
          mode: "disabled",
          grayline: null,
          entries: [],
        };
        renderScheduler();
        clearSchedulerDirty();
        showSchedulerToast("Scheduler reset.");
      })
      .catch(function (e) {
        showSchedulerToast("Reset failed: " + e.message, true);
      });
  }

  // -------------------------------------------------------------------------
  // Dirty-state tracking
  // -------------------------------------------------------------------------
  function markSchedulerDirty() {
    if (schedulerDirty) return;
    schedulerDirty = true;
    var btn = document.getElementById("scheduler-save-btn");
    if (btn) btn.classList.add("sch-dirty");
  }

  function clearSchedulerDirty() {
    schedulerDirty = false;
    var btn = document.getElementById("scheduler-save-btn");
    if (btn) btn.classList.remove("sch-dirty");
  }

  // -------------------------------------------------------------------------
  // Toast helper
  // -------------------------------------------------------------------------
  function showSchedulerToast(msg, isError) {
    const el = document.getElementById("scheduler-toast");
    if (!el) return;
    el.textContent = msg;
    el.style.background = isError ? "var(--color-error, #c00)" : "var(--accent-green)";
    el.style.display = "block";
    setTimeout(function () {
      el.style.display = "none";
    }, 3000);
  }

  // -------------------------------------------------------------------------
  // Wire events (called once DOM is ready)
  // -------------------------------------------------------------------------
  function wireSchedulerEvents() {
    const modeEl = document.getElementById("scheduler-mode-select");
    if (modeEl) {
      modeEl.addEventListener("change", function () {
        if (!currentConfig) currentConfig = { remote: currentRigId, mode: modeEl.value, entries: [] };
        currentConfig.mode = modeEl.value;
        renderScheduler();
      });
    }

    const saveBtn = document.getElementById("scheduler-save-btn");
    if (saveBtn) saveBtn.addEventListener("click", saveScheduler);

    const resetBtn = document.getElementById("scheduler-reset-btn");
    if (resetBtn) resetBtn.addEventListener("click", resetScheduler);

    const addBtn = document.getElementById("scheduler-ts-add-btn");
    if (addBtn) addBtn.addEventListener("click", function () { schOpenEntryForm(null, null); });

    const entryForm = document.getElementById("sch-entry-form");
    if (entryForm) entryForm.addEventListener("submit", schEntryFormSubmit);

    const cancelBtn = document.getElementById("sch-entry-form-cancel");
    if (cancelBtn) cancelBtn.addEventListener("click", schCloseEntryForm);

    const prevBtn = document.getElementById("scheduler-prev-btn");
    if (prevBtn) prevBtn.addEventListener("click", function () {
      schedulerSelectRelativeEntry(-1);
    });

    const nextBtn = document.getElementById("scheduler-next-btn");
    if (nextBtn) nextBtn.addEventListener("click", function () {
      schedulerSelectRelativeEntry(1);
    });

    // Dirty-state: mark dirty on any user input/change within the scheduler panel
    var schPanel = document.getElementById("scheduler-panel");
    if (schPanel && !schPanel._dirtyWired) {
      schPanel._dirtyWired = true;
      schPanel.addEventListener("input", function (e) {
        // Ignore the entry-form inputs (they don't affect saved config until submitted)
        if (e.target.closest("#sch-entry-form") || e.target.closest("#sch-sat-form")) return;
        markSchedulerDirty();
      });
      schPanel.addEventListener("change", function (e) {
        if (e.target.closest("#sch-entry-form") || e.target.closest("#sch-sat-form")) return;
        markSchedulerDirty();
      });
    }

    // Grid square ↔ lat/lon sync
    var gridEl = document.getElementById("scheduler-gl-grid");
    if (gridEl) {
      gridEl.addEventListener("input", function () {
        var ll = gridToLatLon(gridEl.value);
        if (ll) {
          setInputValue("scheduler-gl-lat", ll.lat.toFixed(3));
          setInputValue("scheduler-gl-lon", ll.lon.toFixed(3));
          markSchedulerDirty();
        }
      });
    }
    var latEl = document.getElementById("scheduler-gl-lat");
    var lonEl = document.getElementById("scheduler-gl-lon");
    [latEl, lonEl].forEach(function (el) {
      if (el) {
        el.addEventListener("input", function () {
          var la = parseFloat(document.getElementById("scheduler-gl-lat").value);
          var lo = parseFloat(document.getElementById("scheduler-gl-lon").value);
          var gEl = document.getElementById("scheduler-gl-grid");
          if (gEl && !isNaN(la) && !isNaN(lo)) {
            gEl.value = latLonToGrid(la, lo);
          }
        });
      }
    });

    wireExtraBmAdd();
    wireSatelliteEvents();
  }

  function populateTsBookmarkSelect() {
    const sel = document.getElementById("scheduler-ts-bookmark");
    const extraSel = document.getElementById("scheduler-ts-extra-bm-pick");
    [sel, extraSel].forEach(function (el) {
      if (!el) return;
      const prev = el.value;
      el.innerHTML = '<option value="">— select bookmark —</option>';
      bookmarkList.forEach(function (bm) {
        const opt = document.createElement("option");
        opt.value = bm.id;
        opt.textContent = bm.name + " (" + formatFreq(bm.freq_hz) + " " + bm.mode + ")";
        el.appendChild(opt);
      });
      if (prev) el.value = prev;
    });
  }

  // Pending extra bookmark IDs for the entry being composed in the add form.
  let pendingExtraBmIds = [];

  function renderExtraBmList() {
    var container = document.getElementById("scheduler-ts-extra-bm-list");
    if (!container) return;
    container.innerHTML = "";
    pendingExtraBmIds.forEach(function (id, idx) {
      var bm = bookmarkList.find(function (b) { return b.id === id; });
      var chip = document.createElement("span");
      chip.className = "sch-extra-bm-chip";
      var rmBtn = document.createElement("span");
      rmBtn.className = "sch-extra-bm-chip-rm";
      rmBtn.textContent = "\u00D7";
      rmBtn.title = "Remove";
      rmBtn.addEventListener("click", function () {
        pendingExtraBmIds.splice(idx, 1);
        renderExtraBmList();
      });
      chip.appendChild(rmBtn);
      var label = document.createTextNode(" " + (bm ? bm.name : id));
      chip.appendChild(label);
      container.appendChild(chip);
    });

    // Disable already-added bookmarks in dropdown
    var pick = document.getElementById("scheduler-ts-extra-bm-pick");
    if (pick) {
      Array.from(pick.options).forEach(function (opt) {
        if (opt.value) {
          opt.disabled = pendingExtraBmIds.includes(opt.value);
        }
      });
    }
  }

  function wireExtraBmAdd() {
    const addBtn = document.getElementById("scheduler-ts-extra-bm-add");
    if (!addBtn || addBtn._wired) return;
    addBtn._wired = true;
    addBtn.addEventListener("click", function () {
      const pick = document.getElementById("scheduler-ts-extra-bm-pick");
      if (!pick || !pick.value) return;
      if (!pendingExtraBmIds.includes(pick.value)) {
        pendingExtraBmIds.push(pick.value);
        renderExtraBmList();
      }
      pick.value = "";
    });
  }

  // -------------------------------------------------------------------------
  // Satellite overlay (delegated to sat-scheduler.js)
  // -------------------------------------------------------------------------
  function renderSatelliteSection() {
    if (window.satScheduler) window.satScheduler.renderSection();
  }

  function renderSatPassStatus() {
    if (window.satScheduler) window.satScheduler.renderPassStatus();
  }

  function collectSatelliteConfig() {
    return window.satScheduler
      ? window.satScheduler.collectSatelliteConfig()
      : { enabled: false, pretune_secs: 60, entries: [] };
  }

  function wireSatelliteEvents() {
    // Expose bridge for sat-scheduler.js to access shared state.
    window.schedulerBridge = {
      getConfig:    function () { return currentConfig; },
      getStatus:    function () { return currentSchedulerStatus; },
      getBookmarks: function () { return bookmarkList; },
      markDirty:    function () { markSchedulerDirty(); },
    };
    if (window.satScheduler) window.satScheduler.wireEvents();
  }

  // -------------------------------------------------------------------------
  // Keyboard shortcuts for scheduler control
  // -------------------------------------------------------------------------
  function isInputFocused() {
    var el = document.activeElement;
    if (!el) return false;
    var tag = el.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el.isContentEditable;
  }

  document.addEventListener("keydown", function (e) {
    if (isInputFocused()) return;

    if (e.shiftKey && e.key === "R") {
      e.preventDefault();
      // Toggle release to scheduler
      var releaseBtn = document.getElementById("scheduler-release-btn");
      if (releaseBtn && !releaseBtn.disabled) releaseBtn.click();
    } else if (e.shiftKey && e.key === "N") {
      e.preventDefault();
      schedulerSelectRelativeEntry(1);
    } else if (e.shiftKey && e.key === "P") {
      e.preventDefault();
      schedulerSelectRelativeEntry(-1);
    }
  });

  // -------------------------------------------------------------------------
  // Public API
  // -------------------------------------------------------------------------
  // Persist details open/closed state
  (function () {
    var details = document.querySelector(".sch-ts-details");
    if (!details) return;
    var key = "sch-details-open";
    var saved = localStorage.getItem(key);
    if (saved !== null) details.open = saved === "1";
    details.addEventListener("toggle", function () {
      localStorage.setItem(key, details.open ? "1" : "0");
    });
  })();

  window.initScheduler = initScheduler;
  window.destroyScheduler = destroyScheduler;
  window.wireSchedulerEvents = wireSchedulerEvents;
  window.setSchedulerRig = setSchedulerRig;

  // Auto-initialize if the app has already booted (lazy-load case).
  // When loaded eagerly, initSettingsUI() in app.js calls initScheduler();
  // when loaded lazily (e.g. settings tab click after boot), the app has
  // already passed that point, so we must self-initialize here.
  if (typeof authRole !== "undefined" && authRole !== null) {
    initScheduler(typeof lastActiveRigId !== "undefined" ? lastActiveRigId : null, authRole);
    wireSchedulerEvents();
  }
})();
