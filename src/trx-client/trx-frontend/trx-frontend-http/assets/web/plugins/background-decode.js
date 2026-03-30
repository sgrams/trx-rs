// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

(function () {
  "use strict";

  const SUPPORTED_DECODERS = ["aprs", "ais", "ft8", "wspr", "hf-aprs"];

  let backgroundDecodeRole = null;
  let currentRigId = null;
  let currentConfig = null;
  let bookmarkList = [];
  let statusInterval = null;
  let bgdDirty = false;

  function initBackgroundDecode(rigId, role) {
    backgroundDecodeRole = role;
    currentRigId = rigId || null;
    if (currentRigId) loadBackgroundDecode();
    startStatusPolling();
  }

  function setBackgroundDecodeRig(rigId) {
    const nextRigId = rigId || null;
    if (nextRigId === currentRigId) return;
    currentRigId = nextRigId;
    if (!currentRigId) return;
    loadBackgroundDecode();
  }

  function apiGetConfig(rigId) {
    return fetch("/background-decode/" + encodeURIComponent(rigId)).then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function apiPutConfig(rigId, config) {
    return fetch("/background-decode/" + encodeURIComponent(rigId), {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(config),
    }).then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function apiResetConfig(rigId) {
    return fetch("/background-decode/" + encodeURIComponent(rigId), {
      method: "DELETE",
    }).then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function apiGetStatus(rigId) {
    return fetch("/background-decode/" + encodeURIComponent(rigId) + "/status").then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function apiGetBookmarks() {
    return fetch("/bookmarks").then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function loadBackgroundDecode() {
    const rigId = currentRigId;
    if (!rigId) return;
    Promise.all([apiGetConfig(rigId), apiGetBookmarks()])
      .then(function ([config, bookmarks]) {
        currentConfig = config || { remote: rigId, enabled: false, bookmark_ids: [] };
        bookmarkList = Array.isArray(bookmarks) ? bookmarks : [];
        renderBackgroundDecode();
        clearBgdDirty();
        pollBackgroundDecodeStatus();
      })
      .catch(function (err) {
        console.error("background decode load failed", err);
      });
  }

  function supportedBookmarks() {
    return bookmarkList.filter(function (bookmark) {
      return bookmarkDecoderKinds(bookmark).length > 0;
    });
  }

  function bookmarkDecoderKinds(bookmark) {
    const decoders = Array.isArray(bookmark && bookmark.decoders) ? bookmark.decoders : [];
    const supported = decoders
      .map(function (item) { return String(item || "").trim().toLowerCase(); })
      .filter(function (item, index, arr) {
        return SUPPORTED_DECODERS.includes(item) && arr.indexOf(item) === index;
      });
    if (supported.length > 0) return supported;
    const mode = String(bookmark && bookmark.mode || "").trim().toUpperCase();
    if (mode === "AIS") return ["ais"];
    if (mode === "PKT") return ["aprs"];
    return supported;
  }

  function renderBackgroundDecode() {
    if (!currentConfig) {
      currentConfig = { remote: currentRigId, enabled: false, bookmark_ids: [] };
    }
    setCheckbox("background-decode-enabled", !!currentConfig.enabled);
    renderBookmarkChecklist();

    const isControl = backgroundDecodeRole === "control" || (typeof authEnabled !== "undefined" && !authEnabled);
    const panel = document.getElementById("background-decode-panel");
    if (panel) {
      panel.querySelectorAll("input, select, button.sch-write").forEach(function (el) {
        el.disabled = !isControl;
      });
    }
    const saveBtn = document.getElementById("background-decode-save-btn");
    const resetBtn = document.getElementById("background-decode-reset-btn");
    if (saveBtn) saveBtn.style.display = isControl ? "" : "none";
    if (resetBtn) resetBtn.style.display = isControl ? "" : "none";
  }

  function renderBookmarkChecklist(filterText) {
    const container = document.getElementById("bgd-bookmark-checklist");
    if (!container) return;
    container.innerHTML = "";

    const selectedIds = new Set(
      currentConfig && Array.isArray(currentConfig.bookmark_ids) ? currentConfig.bookmark_ids : []
    );
    const all = supportedBookmarks();
    const filter = (filterText || "").trim().toLowerCase();

    const filtered = filter
      ? all.filter(function (bm) {
          var text = (bm.name + " " + formatFreq(bm.freq_hz) + " " + bm.mode).toLowerCase();
          return text.indexOf(filter) >= 0;
        })
      : all;

    if (filtered.length === 0) {
      container.innerHTML = '<div class="bgd-checklist-empty">' +
        (all.length === 0 ? "No supported bookmarks available." : "No bookmarks match filter.") +
        '</div>';
      return;
    }

    filtered.forEach(function (bookmark) {
      var row = document.createElement("label");
      row.className = "bgd-checklist-row";
      var decoders = bookmarkDecoderKinds(bookmark);
      var checked = selectedIds.has(bookmark.id) ? " checked" : "";
      row.innerHTML =
        '<input type="checkbox"' + checked + ' data-bm-id="' + escHtml(bookmark.id) + '" />' +
        '<span class="bgd-checklist-name">' + escHtml(bookmark.name) + '</span>' +
        '<span class="bgd-checklist-meta">' + escHtml(formatFreq(bookmark.freq_hz) + " " + bookmark.mode + " · " + decoders.join("/").toUpperCase()) + '</span>';
      row.querySelector("input").addEventListener("change", function (e) {
        onChecklistToggle(bookmark.id, e.target.checked);
      });
      container.appendChild(row);
    });
  }

  function onChecklistToggle(bookmarkId, checked) {
    if (!currentConfig) {
      currentConfig = { remote: currentRigId, enabled: false, bookmark_ids: [] };
    }
    if (!Array.isArray(currentConfig.bookmark_ids)) currentConfig.bookmark_ids = [];
    if (checked && !currentConfig.bookmark_ids.includes(bookmarkId)) {
      currentConfig.bookmark_ids.push(bookmarkId);
    } else if (!checked) {
      currentConfig.bookmark_ids = currentConfig.bookmark_ids.filter(function (id) { return id !== bookmarkId; });
    }
    markBgdDirty();
  }

  function saveBackgroundDecode() {
    const rigId = currentRigId;
    if (!rigId) return;
    const payload = {
      remote: rigId,
      enabled: !!document.getElementById("background-decode-enabled").checked,
      bookmark_ids: Array.isArray(currentConfig && currentConfig.bookmark_ids) ? currentConfig.bookmark_ids.slice() : [],
    };
    const btn = document.getElementById("background-decode-save-btn");
    if (btn) btn.disabled = true;
    apiPutConfig(rigId, payload)
      .then(function (saved) {
        currentConfig = saved;
        renderBackgroundDecode();
        clearBgdDirty();
        pollBackgroundDecodeStatus();
        showToast("Background decode saved.");
      })
      .catch(function (err) {
        showToast("Save failed: " + err.message, true);
      })
      .finally(function () {
        if (btn) btn.disabled = false;
      });
  }

  function resetBackgroundDecode() {
    const rigId = currentRigId;
    if (!rigId) return;
    if (!confirm("Reset background decode configuration? This cannot be undone.")) return;
    apiResetConfig(rigId)
      .then(function (saved) {
        currentConfig = saved;
        renderBackgroundDecode();
        clearBgdDirty();
        pollBackgroundDecodeStatus();
        showToast("Background decode reset.");
      })
      .catch(function (err) {
        showToast("Reset failed: " + err.message, true);
      });
  }

  function startStatusPolling() {
    if (statusInterval) clearInterval(statusInterval);
    statusInterval = setInterval(pollBackgroundDecodeStatus, 15000);
  }

  function pollBackgroundDecodeStatus() {
    const rigId = currentRigId;
    if (!rigId) return;
    apiGetStatus(rigId)
      .then(renderStatus)
      .catch(function () {});
  }

  function renderStatus(status) {
    const card = document.getElementById("background-decode-status-card");
    if (!card) return;
    const entries = Array.isArray(status && status.entries) ? status.entries : [];
    if (!entries.length) {
      card.textContent = "No background decode bookmarks configured.";
      return;
    }
    const summary = [];
    if (status.active_rig) {
      if (Number.isFinite(status.center_hz)) summary.push("Center " + formatFreq(status.center_hz));
      if (Number.isFinite(status.sample_rate) && status.sample_rate > 0) summary.push("Span ±" + formatFreq(status.sample_rate / 2));
    } else {
      summary.push("This rig is not currently selected for audio.");
    }
    let html = summary.length ? '<div style="margin-bottom:0.8rem;color:var(--text-muted);">' + escHtml(summary.join(" · ")) + "</div>" : "";
    html += '<div class="bgd-status-list">';
    entries.forEach(function (entry) {
      const name = entry.bookmark_name || entry.bookmark_id || "Unknown bookmark";
      const parts = [];
      if (Number.isFinite(entry.freq_hz)) parts.push(formatFreq(entry.freq_hz));
      if (entry.mode) parts.push(entry.mode);
      if (Array.isArray(entry.decoder_kinds) && entry.decoder_kinds.length) {
        parts.push(entry.decoder_kinds.join("/").toUpperCase());
      }
      html +=
        '<div class="bgd-status-row">' +
          '<div>' +
            '<div class="bgd-status-name">' + escHtml(name) + '</div>' +
            '<div class="bgd-status-meta">' + escHtml(parts.join(" · ")) + '</div>' +
          '</div>' +
          '<div class="bgd-status-state" data-state="' + escHtml(entry.state || "inactive") + '">' +
            '<svg class="bgd-state-dot" viewBox="0 0 8 8"><circle cx="4" cy="4" r="3.5"/></svg>' +
            escHtml(prettyState(entry.state)) + '</div>' +
        '</div>';
    });
    html += "</div>";
    card.innerHTML = html;
  }

  function prettyState(state) {
    switch (state) {
      case "active": return "\u2713 Active";
      case "out_of_span": return "\u25B3 Out of span";
      case "waiting_for_spectrum": return "\u25B3 Waiting";
      case "waiting_for_user": return "\u25B3 No user";
      case "missing_bookmark": return "\u2717 Missing";
      case "no_supported_decoders": return "\u2717 Unsupported";
      case "disabled": return "\u25B3 Disabled";
      case "handled_by_scheduler": return "\u25B3 Scheduler";
      case "scheduler_has_control": return "\u25B3 Scheduler";
      case "handled_by_virtual_channel": return "\u25B3 VChan";
      default: return "\u25B3 Inactive";
    }
  }

  function setCheckbox(id, value) {
    const el = document.getElementById(id);
    if (el) el.checked = !!value;
  }

  function formatFreq(hz) {
    if (!Number.isFinite(hz) || hz <= 0) return "--";
    if (hz >= 1e6) return (hz / 1e6).toFixed(3).replace(/\.?0+$/, "") + " MHz";
    if (hz >= 1e3) return (hz / 1e3).toFixed(1).replace(/\.?0+$/, "") + " kHz";
    return hz + " Hz";
  }

  function escHtml(value) {
    return String(value == null ? "" : value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function markBgdDirty() {
    if (bgdDirty) return;
    bgdDirty = true;
    var btn = document.getElementById("background-decode-save-btn");
    if (btn) btn.classList.add("sch-dirty");
  }

  function clearBgdDirty() {
    bgdDirty = false;
    var btn = document.getElementById("background-decode-save-btn");
    if (btn) btn.classList.remove("sch-dirty");
  }

  function showToast(msg, isError) {
    const el = document.getElementById("background-decode-toast");
    if (!el) return;
    el.textContent = msg;
    el.style.background = isError ? "var(--color-error, #c00)" : "var(--accent-green)";
    el.style.display = "block";
    setTimeout(function () {
      el.style.display = "none";
    }, 3000);
  }

  function selectAllBookmarks() {
    if (!currentConfig) {
      currentConfig = { remote: currentRigId, enabled: false, bookmark_ids: [] };
    }
    var ids = supportedBookmarks().map(function (bm) { return bm.id; });
    currentConfig.bookmark_ids = ids;
    renderBookmarkChecklist(document.getElementById("bgd-bookmark-filter")?.value);
    markBgdDirty();
  }

  function deselectAllBookmarks() {
    if (!currentConfig) {
      currentConfig = { remote: currentRigId, enabled: false, bookmark_ids: [] };
    }
    currentConfig.bookmark_ids = [];
    renderBookmarkChecklist(document.getElementById("bgd-bookmark-filter")?.value);
    markBgdDirty();
  }

  function wireBackgroundDecodeEvents() {
    const filterInput = document.getElementById("bgd-bookmark-filter");
    if (filterInput && !filterInput._wired) {
      filterInput._wired = true;
      filterInput.addEventListener("input", function () {
        renderBookmarkChecklist(filterInput.value);
      });
    }

    const enabledCb = document.getElementById("background-decode-enabled");
    if (enabledCb && !enabledCb._wired) {
      enabledCb._wired = true;
      enabledCb.addEventListener("change", function () { markBgdDirty(); });
    }

    const selectAllBtn = document.getElementById("bgd-select-all-btn");
    if (selectAllBtn && !selectAllBtn._wired) {
      selectAllBtn._wired = true;
      selectAllBtn.addEventListener("click", selectAllBookmarks);
    }

    const deselectAllBtn = document.getElementById("bgd-deselect-all-btn");
    if (deselectAllBtn && !deselectAllBtn._wired) {
      deselectAllBtn._wired = true;
      deselectAllBtn.addEventListener("click", deselectAllBookmarks);
    }

    const saveBtn = document.getElementById("background-decode-save-btn");
    if (saveBtn && !saveBtn._wired) {
      saveBtn._wired = true;
      saveBtn.addEventListener("click", saveBackgroundDecode);
    }

    const resetBtn = document.getElementById("background-decode-reset-btn");
    if (resetBtn && !resetBtn._wired) {
      resetBtn._wired = true;
      resetBtn.addEventListener("click", resetBackgroundDecode);
    }
  }

  window.initBackgroundDecode = initBackgroundDecode;
  window.wireBackgroundDecodeEvents = wireBackgroundDecodeEvents;
  window.setBackgroundDecodeRig = setBackgroundDecodeRig;
})();
