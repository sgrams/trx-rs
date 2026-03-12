// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
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
        currentConfig = config || { rig_id: rigId, enabled: false, bookmark_ids: [] };
        bookmarkList = Array.isArray(bookmarks) ? bookmarks : [];
        renderBookmarkPick();
        renderBackgroundDecode();
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

  function renderBookmarkPick() {
    const sel = document.getElementById("background-decode-bookmark-pick");
    if (!sel) return;
    const selectedIds = new Set(currentConfig && Array.isArray(currentConfig.bookmark_ids) ? currentConfig.bookmark_ids : []);
    sel.innerHTML = '<option value="">- select bookmark -</option>';
    supportedBookmarks().forEach(function (bookmark) {
      if (selectedIds.has(bookmark.id)) return;
      const opt = document.createElement("option");
      opt.value = bookmark.id;
      opt.textContent = bookmark.name + " (" + formatFreq(bookmark.freq_hz) + " " + bookmark.mode + ")";
      sel.appendChild(opt);
    });
  }

  function renderBackgroundDecode() {
    if (!currentConfig) {
      currentConfig = { rig_id: currentRigId, enabled: false, bookmark_ids: [] };
    }
    setCheckbox("background-decode-enabled", !!currentConfig.enabled);
    renderBookmarkList();

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

  function renderBookmarkList() {
    const container = document.getElementById("background-decode-bookmark-list");
    if (!container) return;
    container.innerHTML = "";
    const ids = currentConfig && Array.isArray(currentConfig.bookmark_ids) ? currentConfig.bookmark_ids : [];
    if (!ids.length) {
      container.textContent = "No background decode bookmarks selected.";
      return;
    }
    ids.forEach(function (id) {
      const bookmark = bookmarkList.find(function (item) { return item.id === id; });
      const chip = document.createElement("div");
      chip.className = "sch-extra-bm-tag bgd-bookmark-tag";
      const decoders = bookmarkDecoderKinds(bookmark);
      chip.innerHTML =
        '<span>' + escHtml(bookmark ? bookmark.name : id) + '</span>' +
        '<span class="bgd-bookmark-meta">' + escHtml(bookmark ? (formatFreq(bookmark.freq_hz) + " " + bookmark.mode + " · " + decoders.join("/").toUpperCase()) : "Missing bookmark") + '</span>';
      const btn = document.createElement("span");
      btn.className = "sch-extra-bm-rm";
      btn.textContent = "×";
      btn.addEventListener("click", function () {
        removeBookmark(id);
      });
      chip.appendChild(btn);
      container.appendChild(chip);
    });
  }

  function removeBookmark(id) {
    if (!currentConfig || !Array.isArray(currentConfig.bookmark_ids)) return;
    currentConfig.bookmark_ids = currentConfig.bookmark_ids.filter(function (item) { return item !== id; });
    renderBookmarkPick();
    renderBackgroundDecode();
  }

  function addBookmark() {
    const sel = document.getElementById("background-decode-bookmark-pick");
    if (!sel || !sel.value) return;
    if (!currentConfig) {
      currentConfig = { rig_id: currentRigId, enabled: false, bookmark_ids: [] };
    }
    if (!Array.isArray(currentConfig.bookmark_ids)) currentConfig.bookmark_ids = [];
    if (!currentConfig.bookmark_ids.includes(sel.value)) currentConfig.bookmark_ids.push(sel.value);
    sel.value = "";
    renderBookmarkPick();
    renderBackgroundDecode();
  }

  function saveBackgroundDecode() {
    const rigId = currentRigId;
    if (!rigId) return;
    const payload = {
      rig_id: rigId,
      enabled: !!document.getElementById("background-decode-enabled").checked,
      bookmark_ids: Array.isArray(currentConfig && currentConfig.bookmark_ids) ? currentConfig.bookmark_ids.slice() : [],
    };
    const btn = document.getElementById("background-decode-save-btn");
    if (btn) btn.disabled = true;
    apiPutConfig(rigId, payload)
      .then(function (saved) {
        currentConfig = saved;
        renderBookmarkPick();
        renderBackgroundDecode();
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
    apiResetConfig(rigId)
      .then(function (saved) {
        currentConfig = saved;
        renderBookmarkPick();
        renderBackgroundDecode();
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
          '<div class="bgd-status-state" data-state="' + escHtml(entry.state || "inactive") + '">' + escHtml(prettyState(entry.state)) + '</div>' +
        '</div>';
    });
    html += "</div>";
    card.innerHTML = html;
  }

  function prettyState(state) {
    switch (state) {
      case "active": return "Active";
      case "out_of_span": return "Out of span";
      case "waiting_for_spectrum": return "Waiting";
      case "waiting_for_user": return "No user";
      case "missing_bookmark": return "Missing";
      case "no_supported_decoders": return "Unsupported";
      case "disabled": return "Disabled";
      case "handled_by_scheduler": return "Scheduler";
      case "scheduler_has_control": return "Scheduler";
      case "handled_by_virtual_channel": return "VChan";
      default: return "Inactive";
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

  function wireBackgroundDecodeEvents() {
    const addBtn = document.getElementById("background-decode-bookmark-add");
    if (addBtn && !addBtn._wired) {
      addBtn._wired = true;
      addBtn.addEventListener("click", addBookmark);
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
