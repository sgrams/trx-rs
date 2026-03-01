// --- Bookmarks Tab ---

var bmList = [];
let bmEditId = null;

function bmFmtFreq(hz) {
  if (!Number.isFinite(hz) || hz <= 0) return "--";
  if (hz >= 1e9) return (hz / 1e9).toFixed(6).replace(/\.?0+$/, "") + "\u202fGHz";
  if (hz >= 1e6) return (hz / 1e6).toFixed(6).replace(/\.?0+$/, "") + "\u202fMHz";
  if (hz >= 1e3) return (hz / 1e3).toFixed(3).replace(/\.?0+$/, "") + "\u202fkHz";
  return hz + "\u202fHz";
}

function bmEsc(str) {
  const d = document.createElement("div");
  d.appendChild(document.createTextNode(String(str)));
  return d.innerHTML;
}

function bmCanControl() {
  return (
    (typeof authEnabled !== "undefined" && !authEnabled) ||
    (typeof authRole !== "undefined" && authRole === "control")
  );
}

// Show/hide the Add Bookmark button based on the current auth role.
function bmSyncAccess() {
  const addBtn = document.getElementById("bm-add-btn");
  if (addBtn) addBtn.style.display = bmCanControl() ? "" : "none";
}

async function bmFetch(categoryFilter) {
  let url = "/bookmarks";
  if (categoryFilter && categoryFilter !== "") {
    url += "?category=" + encodeURIComponent(categoryFilter);
  }
  try {
    const resp = await fetch(url);
    if (!resp.ok) throw new Error("HTTP " + resp.status);
    bmList = await resp.json();
  } catch (e) {
    console.error("Failed to fetch bookmarks:", e);
    bmList = [];
  }
  bmSyncAccess();
  bmRender(bmList);
  bmRefreshCategoryFilter(categoryFilter);
  if (typeof scheduleSpectrumDraw === "function") scheduleSpectrumDraw();
}

async function bmRefreshCategoryFilter(keepValue) {
  const sel = document.getElementById("bm-category-filter");
  if (!sel) return;
  try {
    const resp = await fetch("/bookmarks");
    if (!resp.ok) return;
    const all = await resp.json();
    const cats = [...new Set(all.map((b) => b.category || "").filter(Boolean))].sort();
    while (sel.options.length > 1) sel.remove(1);
    cats.forEach((cat) => {
      const opt = document.createElement("option");
      opt.value = cat;
      opt.textContent = cat;
      sel.add(opt);
    });
    if (keepValue && cats.includes(keepValue)) sel.value = keepValue;
  } catch (_) {}
}

function bmRender(list) {
  const tbody = document.getElementById("bm-tbody");
  const emptyEl = document.getElementById("bm-empty");
  if (!tbody) return;
  tbody.innerHTML = "";

  if (list.length === 0) {
    if (emptyEl) emptyEl.style.display = "";
    return;
  }
  if (emptyEl) emptyEl.style.display = "none";

  const canControl = bmCanControl();

  list.forEach((bm) => {
    const tr = document.createElement("tr");
    tr.dataset.bmId = bm.id;
    const bwCell = bm.bandwidth_hz ? bmFmtFreq(bm.bandwidth_hz) : "--";
    const catCell = bm.category || "Uncategorised";
    const decoderCell = (bm.decoders || []).join(", ").toUpperCase() || "--";
    const commentCell = bm.comment || "";
    tr.innerHTML =
      `<td class="bm-col-name">${bmEsc(bm.name)}</td>` +
      `<td class="bm-col-freq">${bmFmtFreq(bm.freq_hz)}</td>` +
      `<td class="bm-col-mode">${bmEsc(bm.mode)}</td>` +
      `<td class="bm-col-bw">${bwCell}</td>` +
      `<td class="bm-col-cat">${bmEsc(catCell)}</td>` +
      `<td class="bm-col-dec">${bmEsc(decoderCell)}</td>` +
      `<td class="bm-col-cmt">${bmEsc(commentCell)}</td>` +
      `<td class="bm-col-act">` +
        `<button class="bm-tune-btn" type="button" data-bm-id="${bmEsc(bm.id)}">Tune</button>` +
        (canControl
          ? `<button class="bm-edit-btn" type="button" data-bm-id="${bmEsc(bm.id)}">Edit</button>` +
            `<button class="bm-del-btn" type="button" data-bm-id="${bmEsc(bm.id)}">Del</button>`
          : "") +
      `</td>`;
    tbody.appendChild(tr);
  });
}

// Read decoder checkboxes and return an array of selected decoder names.
function bmReadDecoders() {
  const decoders = [];
  if (document.getElementById("bm-dec-ft8").checked) decoders.push("ft8");
  if (document.getElementById("bm-dec-wspr").checked) decoders.push("wspr");
  return decoders;
}

// Set decoder checkboxes to match the given array.
function bmWriteDecoders(decoders) {
  const list = decoders || [];
  document.getElementById("bm-dec-ft8").checked = list.includes("ft8");
  document.getElementById("bm-dec-wspr").checked = list.includes("wspr");
}

function bmOpenForm(bm) {
  const wrap = document.getElementById("bm-form-wrap");
  if (!wrap) return;
  bmEditId = bm ? bm.id : null;

  document.getElementById("bm-id").value = bm ? bm.id : "";
  document.getElementById("bm-name").value = bm ? bm.name : "";
  document.getElementById("bm-freq").value = bm ? bm.freq_hz : "";
  document.getElementById("bm-mode").value = bm ? bm.mode : "";
  document.getElementById("bm-bw").value = bm && bm.bandwidth_hz ? bm.bandwidth_hz : "";
  document.getElementById("bm-category-input").value = bm ? (bm.category || "") : "";
  document.getElementById("bm-comment").value = bm ? (bm.comment || "") : "";
  bmWriteDecoders(bm ? bm.decoders : []);
  document.getElementById("bm-form-title").textContent = bm ? "Edit Bookmark" : "Add Bookmark";

  wrap.style.display = "";
  document.getElementById("bm-name").focus();
}

function bmCloseForm() {
  const wrap = document.getElementById("bm-form-wrap");
  if (wrap) wrap.style.display = "none";
  bmEditId = null;
}

function bmPrefillFromStatus() {
  // Use globals maintained by app.js (updated by SSE stream)
  if (typeof lastFreqHz === "number" && Number.isFinite(lastFreqHz)) {
    document.getElementById("bm-freq").value = Math.round(lastFreqHz);
  }
  if (typeof lastModeName === "string" && lastModeName) {
    document.getElementById("bm-mode").value = lastModeName;
  }
  if (typeof currentBandwidthHz === "number" && currentBandwidthHz > 0) {
    document.getElementById("bm-bw").value = Math.round(currentBandwidthHz);
  }
}

async function bmSave(e) {
  e.preventDefault();
  const id = document.getElementById("bm-id").value;
  const name = document.getElementById("bm-name").value.trim();
  const freqStr = document.getElementById("bm-freq").value;
  const freq_hz = parseInt(freqStr, 10);
  const mode = document.getElementById("bm-mode").value.trim();
  const bwStr = document.getElementById("bm-bw").value;
  const bandwidth_hz = bwStr ? parseInt(bwStr, 10) : null;
  const category = document.getElementById("bm-category-input").value.trim();
  const comment = document.getElementById("bm-comment").value.trim();
  const decoders = bmReadDecoders();

  if (!name || !Number.isFinite(freq_hz) || !mode) {
    alert("Name, Frequency, and Mode are required.");
    return;
  }

  const body = { name, freq_hz, mode, bandwidth_hz, category, comment, decoders };

  try {
    let resp;
    if (id) {
      resp = await fetch("/bookmarks/" + encodeURIComponent(id), {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
    } else {
      resp = await fetch("/bookmarks", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
    }
    if (!resp.ok) {
      const text = await resp.text();
      if (resp.status === 409) {
        throw new Error("A bookmark for that frequency already exists.");
      }
      throw new Error(text || "HTTP " + resp.status);
    }
    bmCloseForm();
    await bmFetch(document.getElementById("bm-category-filter").value);
  } catch (err) {
    console.error("Failed to save bookmark:", err);
    alert("Failed to save bookmark: " + err.message);
  }
}

async function bmDelete(id) {
  if (!confirm("Delete this bookmark?")) return;
  try {
    const resp = await fetch("/bookmarks/" + encodeURIComponent(id), {
      method: "DELETE",
    });
    if (!resp.ok) throw new Error("HTTP " + resp.status);
    await bmFetch(document.getElementById("bm-category-filter").value);
  } catch (err) {
    console.error("Failed to delete bookmark:", err);
    alert("Failed to delete bookmark: " + err.message);
  }
}

async function bmApply(bm) {
  try {
    await postPath("/set_freq?hz=" + bm.freq_hz);
    await postPath("/set_mode?mode=" + encodeURIComponent(bm.mode));
    if (bm.bandwidth_hz) {
      await postPath("/set_bandwidth?hz=" + bm.bandwidth_hz);
    }
    // Toggle decoders when in DIG mode
    if (bm.mode === "DIG" && Array.isArray(bm.decoders)) {
      const statusResp = await fetch("/status");
      if (statusResp.ok) {
        const st = await statusResp.json();
        const wantFt8 = bm.decoders.includes("ft8");
        if (wantFt8 !== !!st.ft8_decode_enabled) {
          await postPath("/toggle_ft8_decode");
        }
        const wantWspr = bm.decoders.includes("wspr");
        if (wantWspr !== !!st.wspr_decode_enabled) {
          await postPath("/toggle_wspr_decode");
        }
      }
    }
  } catch (err) {
    console.error("Failed to apply bookmark:", err);
  }
}

// --- Event wiring ---
(function initBookmarks() {
  // Set initial button visibility (auth may already be resolved by the time
  // scripts run if auth is disabled; otherwise bmFetch() will sync it).
  bmSyncAccess();

  // Refresh list and sync access when the Bookmarks tab is activated
  document.querySelector(".tab-bar").addEventListener("click", (e) => {
    const btn = e.target.closest('.tab[data-tab="bookmarks"]');
    if (!btn) return;
    bmFetch(document.getElementById("bm-category-filter").value);
  });

  // Add Bookmark button — open form and prefill from current rig state
  document.getElementById("bm-add-btn").addEventListener("click", () => {
    bmOpenForm(null);
    bmPrefillFromStatus();
  });

  // Category filter dropdown
  document.getElementById("bm-category-filter").addEventListener("change", (e) => {
    bmFetch(e.target.value);
  });

  // Form submit
  document.getElementById("bm-form").addEventListener("submit", bmSave);

  // Form cancel
  document.getElementById("bm-form-cancel").addEventListener("click", bmCloseForm);

  // Table action buttons (event delegation)
  document.getElementById("bm-tbody").addEventListener("click", async (e) => {
    const tuneBtn = e.target.closest(".bm-tune-btn");
    const editBtn = e.target.closest(".bm-edit-btn");
    const delBtn = e.target.closest(".bm-del-btn");

    if (tuneBtn) {
      const bm = bmList.find((b) => b.id === tuneBtn.dataset.bmId);
      if (bm) await bmApply(bm);
    } else if (editBtn) {
      const bm = bmList.find((b) => b.id === editBtn.dataset.bmId);
      if (bm) bmOpenForm(bm);
    } else if (delBtn) {
      await bmDelete(delBtn.dataset.bmId);
    }
  });

  // Pre-load bookmarks so spectrum markers are visible immediately.
  bmFetch("");
})();
