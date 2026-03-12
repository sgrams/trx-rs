// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
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
  let bookmarkList = [];          // [{id, name, freq_hz, mode}, ...]
  let statusInterval = null;

  // -------------------------------------------------------------------------
  // Init
  // -------------------------------------------------------------------------
  function initScheduler(rigId, role) {
    schedulerRole = role;
    currentRigId = rigId || null;
    if (currentRigId) loadScheduler();
    startStatusPolling();
  }

  function destroyScheduler() {
    if (statusInterval) {
      clearInterval(statusInterval);
      statusInterval = null;
    }
  }

  // -------------------------------------------------------------------------
  // Active rig (mirrors top-bar rig picker in app.js)
  // -------------------------------------------------------------------------
  function setSchedulerRig(rigId) {
    const nextRigId = rigId || null;
    if (nextRigId === currentRigId) return;
    currentRigId = nextRigId;
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

  function apiGetBookmarks() {
    return fetch("/bookmarks").then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
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
      })
      .catch(function (e) {
        console.error("scheduler load failed", e);
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

  function pollStatus() {
    const rig = currentRigId;
    if (!rig) return;
    apiGetStatus(rig)
      .then(function (st) {
        renderStatus(st);
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
    const name = st.last_bookmark_name || st.last_bookmark_id || "—";
    let ts = "";
    if (st.last_applied_utc) {
      const d = new Date(st.last_applied_utc * 1000);
      ts = " at " + d.toUTCString();
    }
    el.textContent = "Last applied: " + name + ts;
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

    // Show/hide sections
    const glSection = document.getElementById("scheduler-grayline-section");
    const tsSection = document.getElementById("scheduler-timespan-section");
    if (glSection) glSection.style.display = mode === "grayline" ? "" : "none";
    if (tsSection) tsSection.style.display = mode === "time_span" ? "" : "none";

    // Grayline inputs
    if (mode === "grayline" && currentConfig && currentConfig.grayline) {
      const gl = currentConfig.grayline;
      // Prefer saved value; fall back to server coordinates from app.js globals.
      const lat = gl.lat != null ? gl.lat : (typeof serverLat !== "undefined" ? serverLat : "");
      const lon = gl.lon != null ? gl.lon : (typeof serverLon !== "undefined" ? serverLon : "");
      setInputValue("scheduler-gl-lat", lat != null ? lat : "");
      setInputValue("scheduler-gl-lon", lon != null ? lon : "");
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
      const il = entry.interleave_min ? String(entry.interleave_min) + " min" : "—";
      const allDay = entry.start_min === entry.end_min;
      const centerCell = entry.center_hz ? formatFreq(entry.center_hz) : "—";
      const extraIds = Array.isArray(entry.bookmark_ids) ? entry.bookmark_ids : [];
      const extraCell = extraIds.length
        ? extraIds.map(function (id) { return escHtml(bmName(id)); }).join(", ")
        : "—";
      tr.innerHTML =
        '<td>' + (allDay ? "All day" : minToHHMM(entry.start_min)) + '</td>' +
        '<td>' + (allDay ? "—" : minToHHMM(entry.end_min)) + '</td>' +
        '<td>' + centerCell + '</td>' +
        '<td>' + bmName(entry.bookmark_id) + '</td>' +
        '<td>' + extraCell + '</td>' +
        '<td>' + escHtml(entry.label || "") + '</td>' +
        '<td>' + il + '</td>' +
        '<td><button class="sch-write sch-remove-btn" data-idx="' + idx + '" type="button">Remove</button></td>';
      tbody.appendChild(tr);
    });
    tbody.querySelectorAll(".sch-remove-btn").forEach(function (btn) {
      btn.addEventListener("click", function () {
        removeEntry(parseInt(btn.dataset.idx, 10));
      });
    });
  }

  function bmName(id) {
    const bm = bookmarkList.find(function (b) { return b.id === id; });
    return bm ? escHtml(bm.name) : escHtml(id);
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

  function escHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function removeEntry(idx) {
    if (!currentConfig || !currentConfig.entries) return;
    currentConfig.entries.splice(idx, 1);
    renderTimespanEntries();
  }

  // -------------------------------------------------------------------------
  // Add entry
  // -------------------------------------------------------------------------
  function addEntry() {
    const startEl = document.getElementById("scheduler-ts-start");
    const endEl = document.getElementById("scheduler-ts-end");
    const bmEl = document.getElementById("scheduler-ts-bookmark");
    const labelEl = document.getElementById("scheduler-ts-label");
    const ilEl = document.getElementById("scheduler-ts-entry-interleave");
    const centerHzEl = document.getElementById("scheduler-ts-center-hz");
    if (!startEl || !endEl || !bmEl) return;

    const startMin = hhmmToMin(startEl.value);
    const endMin = hhmmToMin(endEl.value);
    const bmId = bmEl.value;
    const label = labelEl ? labelEl.value.trim() : "";
    const ilVal = ilEl ? parseInt(ilEl.value, 10) : NaN;
    const entryInterleave = !isNaN(ilVal) && ilVal > 0 ? ilVal : null;
    const centerHzRaw = centerHzEl ? parseInt(centerHzEl.value, 10) : NaN;
    const centerHz = !isNaN(centerHzRaw) && centerHzRaw > 0 ? centerHzRaw : null;
    const extraBmIds = pendingExtraBmIds.slice();

    if (!bmId) {
      alert("Please select a primary bookmark.");
      return;
    }

    if (!currentConfig) {
      currentConfig = { rig_id: currentRigId, mode: "time_span", entries: [] };
    }
    if (!currentConfig.entries) currentConfig.entries = [];

    const id = "ts_" + Date.now().toString(36);
    currentConfig.entries.push({
      id,
      start_min: startMin,
      end_min: endMin,
      bookmark_id: bmId,
      label: label || null,
      interleave_min: entryInterleave,
      center_hz: centerHz,
      bookmark_ids: extraBmIds,
    });

    startEl.value = "";
    endEl.value = "";
    bmEl.value = "";
    if (labelEl) labelEl.value = "";
    if (ilEl) ilEl.value = "";
    if (centerHzEl) centerHzEl.value = "";
    pendingExtraBmIds = [];
    renderExtraBmList();

    renderTimespanEntries();
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
      rig_id: rig,
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

    const btn = document.getElementById("scheduler-save-btn");
    if (btn) btn.disabled = true;

    apiPutScheduler(rig, config)
      .then(function (saved) {
        currentConfig = saved;
        renderScheduler();
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
          rig_id: rig,
          mode: "disabled",
          grayline: null,
          entries: [],
        };
        renderScheduler();
        showSchedulerToast("Scheduler reset.");
      })
      .catch(function (e) {
        showSchedulerToast("Reset failed: " + e.message, true);
      });
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
        if (!currentConfig) currentConfig = { rig_id: currentRigId, mode: modeEl.value, entries: [] };
        currentConfig.mode = modeEl.value;
        renderScheduler();
      });
    }

    const saveBtn = document.getElementById("scheduler-save-btn");
    if (saveBtn) saveBtn.addEventListener("click", saveScheduler);

    const resetBtn = document.getElementById("scheduler-reset-btn");
    if (resetBtn) resetBtn.addEventListener("click", resetScheduler);

    const addBtn = document.getElementById("scheduler-ts-add-btn");
    if (addBtn) addBtn.addEventListener("click", addEntry);

    wireExtraBmAdd();
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
    const container = document.getElementById("scheduler-ts-extra-bm-list");
    if (!container) return;
    container.innerHTML = "";
    pendingExtraBmIds.forEach(function (id, idx) {
      const bm = bookmarkList.find(function (b) { return b.id === id; });
      const tag = document.createElement("span");
      tag.className = "sch-extra-bm-tag";
      tag.textContent = bm ? bm.name : id;
      const rm = document.createElement("span");
      rm.className = "sch-extra-bm-rm";
      rm.textContent = "×";
      rm.title = "Remove";
      rm.addEventListener("click", function () {
        pendingExtraBmIds.splice(idx, 1);
        renderExtraBmList();
      });
      tag.appendChild(rm);
      container.appendChild(tag);
    });
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
  // Public API
  // -------------------------------------------------------------------------
  window.initScheduler = initScheduler;
  window.destroyScheduler = destroyScheduler;
  window.wireSchedulerEvents = wireSchedulerEvents;
  window.setSchedulerRig = setSchedulerRig;
})();
