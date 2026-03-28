// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

// Satellite Pass Scheduling UI
// Manages the satellite overlay section within the background decoding scheduler.
// Communicates with scheduler.js via a thin window API for shared state access.

(function () {
  "use strict";

  // ── DOM references (cached once) ──────────────────────────────────
  const dom = {
    enabled:    document.getElementById("scheduler-sat-enabled"),
    pretune:    document.getElementById("scheduler-sat-pretune"),
    body:       document.getElementById("scheduler-sat-body"),
    tbody:      document.getElementById("scheduler-sat-tbody"),
    addBtn:     document.getElementById("scheduler-sat-add-btn"),
    passStatus: document.getElementById("scheduler-sat-pass-status"),
    formWrap:   document.getElementById("sch-sat-form-wrap"),
    formTitle:  document.getElementById("sch-sat-form-title"),
    form:       document.getElementById("sch-sat-form"),
    formCancel: document.getElementById("sch-sat-form-cancel"),
    preset:     document.getElementById("scheduler-sat-preset"),
    name:       document.getElementById("scheduler-sat-name"),
    norad:      document.getElementById("scheduler-sat-norad"),
    bookmark:   document.getElementById("scheduler-sat-bookmark"),
    minEl:      document.getElementById("scheduler-sat-min-el"),
    priority:   document.getElementById("scheduler-sat-priority"),
    centerHz:   document.getElementById("scheduler-sat-center-hz"),
  };

  // ── Local state ───────────────────────────────────────────────────
  let editIdx = null; // null = adding, number = editing

  // ── Scheduler bridge ──────────────────────────────────────────────
  // These accessors call into scheduler.js via window.schedulerBridge,
  // which is set up by scheduler.js after it initializes.
  function getBridge() {
    return window.schedulerBridge || {};
  }

  function getConfig() {
    const b = getBridge();
    return typeof b.getConfig === "function" ? b.getConfig() : null;
  }

  function getStatus() {
    const b = getBridge();
    return typeof b.getStatus === "function" ? b.getStatus() : null;
  }

  function getBookmarks() {
    const b = getBridge();
    return typeof b.getBookmarks === "function" ? b.getBookmarks() : [];
  }

  function bmName(id) {
    const bm = getBookmarks().find(function (b) { return b.id === id; });
    return bm ? bm.name : String(id || "");
  }

  function escHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function formatFreq(hz) {
    if (hz >= 1e6) return (hz / 1e6).toFixed(3) + " MHz";
    if (hz >= 1e3) return (hz / 1e3).toFixed(1) + " kHz";
    return hz + " Hz";
  }

  // ── Satellite config helpers ──────────────────────────────────────
  function getSatelliteEntries() {
    var config = getConfig();
    return (config && config.satellites && Array.isArray(config.satellites.entries))
      ? config.satellites.entries
      : [];
  }

  function ensureSatelliteConfig() {
    var config = getConfig();
    if (!config) return { enabled: false, pretune_secs: 60, entries: [] };
    if (!config.satellites) config.satellites = { enabled: false, pretune_secs: 60, entries: [] };
    if (!config.satellites.entries) config.satellites.entries = [];
    return config.satellites;
  }

  function collectSatelliteConfig() {
    var enabled = dom.enabled ? dom.enabled.checked : false;
    var pretune = dom.pretune ? parseInt(dom.pretune.value, 10) : 60;
    return {
      enabled: enabled,
      pretune_secs: isNaN(pretune) || pretune < 0 ? 60 : pretune,
      entries: getSatelliteEntries(),
    };
  }

  // ── Render: section ───────────────────────────────────────────────
  function renderSection() {
    var config = getConfig();
    var satCfg = (config && config.satellites) || {};
    var enabled = !!satCfg.enabled;

    if (dom.enabled) dom.enabled.checked = enabled;
    if (dom.pretune) dom.pretune.value = satCfg.pretune_secs != null ? satCfg.pretune_secs : 60;
    if (dom.body) dom.body.style.display = enabled ? "" : "none";

    renderEntries();
    renderPassStatus();
  }

  // ── Render: entries table ─────────────────────────────────────────
  function renderEntries() {
    if (!dom.tbody) return;
    var entries = getSatelliteEntries();
    var frag = document.createDocumentFragment();

    entries.forEach(function (entry, idx) {
      var tr = document.createElement("tr");

      var tdSat = document.createElement("td");
      tdSat.textContent = entry.satellite || "";
      tr.appendChild(tdSat);

      var tdNorad = document.createElement("td");
      tdNorad.textContent = entry.norad_id || "";
      tr.appendChild(tdNorad);

      var tdBm = document.createElement("td");
      tdBm.textContent = bmName(entry.bookmark_id);
      tr.appendChild(tdBm);

      var tdEl = document.createElement("td");
      tdEl.textContent = (entry.min_elevation_deg != null ? entry.min_elevation_deg + "\u00B0" : "5\u00B0");
      tr.appendChild(tdEl);

      var tdPrio = document.createElement("td");
      tdPrio.textContent = entry.priority || 0;
      tr.appendChild(tdPrio);

      var tdActions = document.createElement("td");

      var editBtn = document.createElement("button");
      editBtn.className = "sch-write";
      editBtn.type = "button";
      editBtn.textContent = "Edit";
      editBtn.addEventListener("click", function () {
        openForm(entry, idx);
      });
      tdActions.appendChild(editBtn);

      var removeBtn = document.createElement("button");
      removeBtn.className = "sch-write";
      removeBtn.type = "button";
      removeBtn.textContent = "Remove";
      removeBtn.addEventListener("click", function () {
        removeEntry(idx);
      });
      tdActions.appendChild(removeBtn);

      tr.appendChild(tdActions);
      frag.appendChild(tr);
    });

    dom.tbody.replaceChildren(frag);
  }

  // ── Render: pass status ───────────────────────────────────────────
  function renderPassStatus() {
    if (!dom.passStatus) return;
    var entries = getSatelliteEntries();
    if (entries.length === 0) {
      dom.passStatus.innerHTML = "";
      return;
    }
    var status = getStatus();
    if (status && status.active_satellite) {
      dom.passStatus.innerHTML =
        '<span class="sch-sat-active-badge">PASS ACTIVE: ' +
        escHtml(status.active_satellite) +
        '</span>';
    } else {
      dom.passStatus.innerHTML =
        '<span style="color:var(--text-muted);font-size:0.8rem;">No satellite pass active. Predictions available in the SAT tab.</span>';
    }
  }

  // ── Render: bookmark dropdown ─────────────────────────────────────
  function renderBookmarkSelect(selectedId) {
    if (!dom.bookmark) return;
    dom.bookmark.innerHTML = '<option value="">— none —</option>';
    getBookmarks().forEach(function (bm) {
      var opt = document.createElement("option");
      opt.value = bm.id;
      opt.textContent = bm.name + " (" + formatFreq(bm.freq_hz) + " " + bm.mode + ")";
      if (bm.id === selectedId) opt.selected = true;
      dom.bookmark.appendChild(opt);
    });
  }

  // ── Entry management ──────────────────────────────────────────────
  function removeEntry(idx) {
    var sat = ensureSatelliteConfig();
    sat.entries.splice(idx, 1);
    renderEntries();
  }

  // ── Form: open ────────────────────────────────────────────────────
  function openForm(entry, idx) {
    editIdx = (idx != null) ? idx : null;

    if (dom.formTitle) dom.formTitle.textContent = entry ? "Edit Satellite" : "Add Satellite";
    if (dom.preset)   dom.preset.value = "";
    if (dom.name)     dom.name.value = entry ? (entry.satellite || "") : "";
    if (dom.norad)    dom.norad.value = entry ? (entry.norad_id || "") : "";
    if (dom.minEl)    dom.minEl.value = entry && entry.min_elevation_deg != null ? entry.min_elevation_deg : 5;
    if (dom.priority) dom.priority.value = entry && entry.priority != null ? entry.priority : 0;
    if (dom.centerHz) dom.centerHz.value = entry && entry.center_hz ? entry.center_hz : "";

    renderBookmarkSelect(entry ? entry.bookmark_id : null);

    if (dom.formWrap) {
      dom.formWrap.style.display = "flex";
      if (dom.name) dom.name.focus();
    }
  }

  // ── Form: close ───────────────────────────────────────────────────
  function closeForm() {
    if (dom.formWrap) dom.formWrap.style.display = "none";
    editIdx = null;
  }

  // ── Form: submit ──────────────────────────────────────────────────
  function onFormSubmit(e) {
    e.preventDefault();

    var satellite = dom.name ? dom.name.value.trim() : "";
    var noradId = dom.norad ? parseInt(dom.norad.value, 10) : NaN;
    var bmId = dom.bookmark ? dom.bookmark.value : "";

    if (!satellite)                    { alert("Please enter a satellite name."); return; }
    if (isNaN(noradId) || noradId <= 0) { alert("Please enter a valid NORAD catalog number."); return; }
    if (!bmId)                         { alert("Please select a bookmark."); return; }

    var minEl = dom.minEl ? parseFloat(dom.minEl.value) : 5;
    var prio = dom.priority ? parseInt(dom.priority.value, 10) : 0;
    var centerHzRaw = dom.centerHz ? parseInt(dom.centerHz.value, 10) : NaN;

    var sat = ensureSatelliteConfig();

    var entryData = {
      satellite: satellite,
      norad_id: noradId,
      bookmark_id: bmId,
      min_elevation_deg: isNaN(minEl) ? 5 : minEl,
      priority: isNaN(prio) ? 0 : prio,
      center_hz: !isNaN(centerHzRaw) && centerHzRaw > 0 ? centerHzRaw : null,
      bookmark_ids: [],
    };

    if (editIdx !== null) {
      var existing = sat.entries[editIdx];
      entryData.id = existing ? existing.id : ("sat_" + Date.now().toString(36));
      sat.entries[editIdx] = entryData;
    } else {
      entryData.id = "sat_" + Date.now().toString(36);
      sat.entries.push(entryData);
    }

    closeForm();
    renderEntries();
  }

  // ── Preset change handler ─────────────────────────────────────────
  function onPresetChange() {
    if (!dom.preset || !dom.preset.value) return;
    var parts = dom.preset.value.split("|");
    if (dom.name)  dom.name.value = parts[0] || "";
    if (dom.norad) dom.norad.value = parts[1] || "";
  }

  // ── Wire all events ───────────────────────────────────────────────
  function wireEvents() {
    if (dom.enabled) {
      dom.enabled.addEventListener("change", function () {
        if (dom.body) dom.body.style.display = dom.enabled.checked ? "" : "none";
      });
    }
    if (dom.addBtn)     dom.addBtn.addEventListener("click", function () { openForm(null, null); });
    if (dom.form)       dom.form.addEventListener("submit", onFormSubmit);
    if (dom.formCancel) dom.formCancel.addEventListener("click", closeForm);
    if (dom.preset)     dom.preset.addEventListener("change", onPresetChange);
  }

  // ── Public API ────────────────────────────────────────────────────
  window.satScheduler = {
    wireEvents:              wireEvents,
    renderSection:           renderSection,
    renderPassStatus:        renderPassStatus,
    collectSatelliteConfig:  collectSatelliteConfig,
  };
})();
