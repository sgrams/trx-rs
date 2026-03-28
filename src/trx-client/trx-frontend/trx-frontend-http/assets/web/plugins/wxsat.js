// --- SAT Plugin ---
// Live view: decoder state, latest image card
// History view: filterable table of all decoded images
// Predictions view: next 24 h passes for ham satellites

// ── DOM references ──────────────────────────────────────────────────
const wxsatStatus = document.getElementById("wxsat-status");
const wxsatLiveView = document.getElementById("wxsat-live-view");
const wxsatHistoryView = document.getElementById("wxsat-history-view");
const wxsatPredictionsView = document.getElementById("wxsat-predictions-view");
const wxsatLiveLatest = document.getElementById("wxsat-live-latest");
const wxsatHistoryList = document.getElementById("wxsat-history-list");
const wxsatHistoryCount = document.getElementById("wxsat-history-count");
const wxsatFilterInput = document.getElementById("wxsat-filter");
const wxsatSortSelect = document.getElementById("wxsat-sort");
const wxsatTypeFilter = document.getElementById("wxsat-type-filter");
const wxsatAptState = document.getElementById("wxsat-apt-state");
const wxsatLrptState = document.getElementById("wxsat-lrpt-state");

// ── State ───────────────────────────────────────────────────────────
let wxsatImageHistory = [];
const WXSAT_MAX_IMAGES = 100;
let wxsatFilterText = "";
let wxsatActiveView = "live"; // "live" | "history" | "predictions"

// ── UI scheduler helper ─────────────────────────────────────────────
function scheduleWxsatUi(key, job) {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob(key, job);
    return;
  }
  job();
}

// ── View switching ──────────────────────────────────────────────────
const wxsatViewLiveBtn = document.getElementById("wxsat-view-live");
const wxsatViewHistoryBtn = document.getElementById("wxsat-view-history");
const wxsatViewPredictionsBtn = document.getElementById("wxsat-view-predictions");

function switchWxsatView(view) {
  wxsatActiveView = view;
  if (wxsatLiveView) wxsatLiveView.style.display = view === "live" ? "" : "none";
  if (wxsatHistoryView) wxsatHistoryView.style.display = view === "history" ? "" : "none";
  if (wxsatPredictionsView) wxsatPredictionsView.style.display = view === "predictions" ? "" : "none";
  if (wxsatViewLiveBtn) wxsatViewLiveBtn.classList.toggle("wxsat-view-active", view === "live");
  if (wxsatViewHistoryBtn) wxsatViewHistoryBtn.classList.toggle("wxsat-view-active", view === "history");
  if (wxsatViewPredictionsBtn) wxsatViewPredictionsBtn.classList.toggle("wxsat-view-active", view === "predictions");
  if (view === "history") {
    renderWxsatHistoryTable();
  } else if (view === "predictions") {
    loadSatPredictions();
  }
}

wxsatViewLiveBtn?.addEventListener("click", () => switchWxsatView("live"));
wxsatViewHistoryBtn?.addEventListener("click", () => switchWxsatView("history"));
wxsatViewPredictionsBtn?.addEventListener("click", () => switchWxsatView("predictions"));

// ── Live view: decoder state ────────────────────────────────────────
// Updated from app.js render() via window.updateWxsatLiveState
window.updateWxsatLiveState = function (update) {
  if (!wxsatAptState || !wxsatLrptState) return;
  const aptOn = !!update.wxsat_decode_enabled;
  const lrptOn = !!update.lrpt_decode_enabled;

  wxsatAptState.textContent = aptOn ? "Listening" : "Idle";
  wxsatAptState.className = "wxsat-live-value " + (aptOn ? "wxsat-state-listening" : "wxsat-state-idle");
  wxsatLrptState.textContent = lrptOn ? "Listening" : "Idle";
  wxsatLrptState.className = "wxsat-live-value " + (lrptOn ? "wxsat-state-listening" : "wxsat-state-idle");
};

function renderWxsatLatestCard() {
  if (!wxsatLiveLatest) return;
  if (wxsatImageHistory.length === 0) {
    wxsatLiveLatest.innerHTML =
      '<div style="color:var(--text-muted);font-size:0.82rem;">No images decoded yet. Enable a decoder and wait for a satellite pass.</div>';
    return;
  }

  const img = wxsatImageHistory[0];
  const decoder = img._decoder || "unknown";
  const typeName = decoder === "lrpt" ? "Meteor LRPT" : "NOAA APT";
  const satellite = img.satellite || "";
  const channels = img.channels || img.channel_a || "";
  const lines = img.line_count || img.mcu_count || 0;
  const unit = decoder === "lrpt" ? "MCU rows" : "lines";
  const ts = img._ts || "--";
  const date = img._tsMs ? new Date(img._tsMs).toLocaleDateString() : "";

  let meta = [typeName];
  if (satellite) meta.push(satellite);
  if (channels) meta.push(channels);
  meta.push(`${lines} ${unit}`);
  meta.push(`${date} ${ts}`);

  let html = `<div class="wxsat-latest-card">`;
  html += `<div class="wxsat-latest-title">Latest decoded image</div>`;
  html += `<div class="wxsat-latest-meta">${meta.join(" &middot; ")}</div>`;
  if (img.path) {
    html += `<a href="${img.path}" target="_blank" style="font-size:0.8rem;color:var(--accent);display:inline-block;margin-top:0.25rem;">Download PNG</a>`;
  }
  if (img.geo_bounds) {
    html += ` <button type="button" class="wxsat-map-btn" onclick="window.wxsatShowOnMap(${img.geo_bounds[0]},${img.geo_bounds[1]},${img.geo_bounds[2]},${img.geo_bounds[3]})" style="font-size:0.8rem;margin-top:0.25rem;margin-left:0.5rem;cursor:pointer;background:none;border:1px solid var(--accent);color:var(--accent);border-radius:3px;padding:1px 6px;">Show on Map</button>`;
  }
  html += `</div>`;
  wxsatLiveLatest.innerHTML = html;
}

// ── History view: table ─────────────────────────────────────────────
function getFilteredHistory() {
  let items = wxsatImageHistory;

  // Type filter
  const typeVal = wxsatTypeFilter ? wxsatTypeFilter.value : "all";
  if (typeVal === "apt") items = items.filter((i) => i._decoder === "apt");
  else if (typeVal === "lrpt") items = items.filter((i) => i._decoder === "lrpt");

  // Text filter
  if (wxsatFilterText) {
    items = items.filter((i) => {
      const haystack = [
        i._decoder === "lrpt" ? "meteor lrpt" : "noaa apt",
        i.satellite || "",
        i.channels || "",
        i.channel_a || "",
        i.channel_b || "",
      ]
        .join(" ")
        .toUpperCase();
      return haystack.includes(wxsatFilterText);
    });
  }

  // Sort
  const sortVal = wxsatSortSelect ? wxsatSortSelect.value : "newest";
  if (sortVal === "oldest") {
    items = items.slice().reverse();
  }

  return items;
}

function renderWxsatHistoryRow(img) {
  const row = document.createElement("div");
  row.className = "wxsat-history-row";

  const decoder = img._decoder || "unknown";
  const typeName = decoder === "lrpt" ? "Meteor LRPT" : "NOAA APT";
  const typeClass = decoder === "lrpt" ? "wxsat-type-lrpt" : "wxsat-type-apt";
  const ts = img._ts || "--";
  const date = img._tsMs ? new Date(img._tsMs).toLocaleDateString([], { month: "short", day: "numeric" }) : "";
  const satellite = img.satellite || "--";
  const channels = decoder === "lrpt" ? (img.channels || "--") : (img.channel_a && img.channel_b ? `A:${img.channel_a} B:${img.channel_b}` : img.channel_a || "--");
  const lines = img.line_count || img.mcu_count || 0;
  const unit = decoder === "lrpt" ? "MCU" : "ln";
  let link = img.path
    ? `<a href="${img.path}" target="_blank" style="color:var(--accent);">PNG</a>`
    : "--";
  if (img.geo_bounds) {
    link += ` <a href="javascript:void(0)" onclick="window.wxsatShowOnMap(${img.geo_bounds[0]},${img.geo_bounds[1]},${img.geo_bounds[2]},${img.geo_bounds[3]})" style="color:var(--accent);">Map</a>`;
  }

  row.innerHTML = [
    `<span>${date} ${ts}</span>`,
    `<span class="wxsat-col-type ${typeClass}">${typeName}</span>`,
    `<span>${satellite}</span>`,
    `<span>${channels}</span>`,
    `<span>${lines} ${unit}</span>`,
    `<span>${link}</span>`,
  ].join("");

  return row;
}

function renderWxsatHistoryTable() {
  if (!wxsatHistoryList) return;
  const items = getFilteredHistory();
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < items.length; i += 1) {
    fragment.appendChild(renderWxsatHistoryRow(items[i]));
  }
  wxsatHistoryList.replaceChildren(fragment);

  if (wxsatHistoryCount) {
    const total = wxsatImageHistory.length;
    const shown = items.length;
    wxsatHistoryCount.textContent =
      total === 0
        ? "No images yet"
        : shown === total
          ? `${total} image${total === 1 ? "" : "s"}`
          : `${shown} of ${total} images`;
  }
}

// ── Add image to history ────────────────────────────────────────────
function addWxsatImage(img, decoder) {
  const tsMs = Number.isFinite(img.ts_ms) ? Number(img.ts_ms) : Date.now();
  img._tsMs = tsMs;
  img._ts = new Date(tsMs).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
  img._decoder = decoder;

  wxsatImageHistory.unshift(img);
  if (wxsatImageHistory.length > WXSAT_MAX_IMAGES) {
    wxsatImageHistory = wxsatImageHistory.slice(0, WXSAT_MAX_IMAGES);
  }

  scheduleWxsatUi("wxsat-latest", () => renderWxsatLatestCard());
  if (wxsatActiveView === "history") {
    scheduleWxsatUi("wxsat-history", () => renderWxsatHistoryTable());
  }
}

// ── Server callbacks ────────────────────────────────────────────────
window.onServerWxsatImage = function (msg) {
  if (wxsatStatus) wxsatStatus.textContent = "Image received (NOAA APT)";
  addWxsatImage(msg, "apt");
  if (msg.geo_bounds && msg.path && window.addWxsatMapOverlay) {
    window.addWxsatMapOverlay(msg);
  }
};

window.onServerLrptImage = function (msg) {
  if (wxsatStatus) wxsatStatus.textContent = "Image received (Meteor LRPT)";
  addWxsatImage(msg, "lrpt");
  if (msg.geo_bounds && msg.path && window.addWxsatMapOverlay) {
    window.addWxsatMapOverlay(msg);
  }
};

window.resetWxsatHistoryView = function () {
  wxsatImageHistory = [];
  if (wxsatHistoryList) wxsatHistoryList.innerHTML = "";
  renderWxsatLatestCard();
  renderWxsatHistoryTable();
  if (window.clearWxsatMapOverlays) window.clearWxsatMapOverlays();
};

window.pruneWxsatHistoryView = function () {
  renderWxsatHistoryTable();
  renderWxsatLatestCard();
};

// ── Toggle buttons ──────────────────────────────────────────────────
const wxsatDecodeToggleBtn = document.getElementById("wxsat-decode-toggle-btn");
wxsatDecodeToggleBtn?.addEventListener("click", async () => {
  try {
    await window.takeSchedulerControlForDecoderDisable?.(wxsatDecodeToggleBtn);
    await postPath("/toggle_wxsat_decode");
  } catch (e) {
    console.error("WXSAT toggle failed", e);
  }
});

const lrptDecodeToggleBtn = document.getElementById("lrpt-decode-toggle-btn");
lrptDecodeToggleBtn?.addEventListener("click", async () => {
  try {
    await window.takeSchedulerControlForDecoderDisable?.(lrptDecodeToggleBtn);
    await postPath("/toggle_lrpt_decode");
  } catch (e) {
    console.error("LRPT toggle failed", e);
  }
});

// ── Filter / sort event listeners ───────────────────────────────────
wxsatFilterInput?.addEventListener("input", () => {
  wxsatFilterText = wxsatFilterInput.value.trim().toUpperCase();
  renderWxsatHistoryTable();
});

wxsatSortSelect?.addEventListener("change", () => renderWxsatHistoryTable());
wxsatTypeFilter?.addEventListener("change", () => renderWxsatHistoryTable());

// ── Settings: clear history ─────────────────────────────────────────
document
  .getElementById("settings-clear-wxsat-history")
  ?.addEventListener("click", async () => {
    try {
      await postPath("/clear_wxsat_decode");
      await postPath("/clear_lrpt_decode");
      window.resetWxsatHistoryView();
    } catch (e) {
      console.error("Weather satellite history clear failed", e);
    }
  });

// ── Predictions view ────────────────────────────────────────────────

function azToCardinal(deg) {
  const dirs = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
  return dirs[Math.round(deg / 45) % 8];
}

function formatPredTime(ms) {
  const d = new Date(ms);
  const now = new Date();
  const dayNames = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const day = d.getUTCDay() !== now.getUTCDay()
    ? dayNames[d.getUTCDay()] + " "
    : "";
  const hh = String(d.getUTCHours()).padStart(2, "0");
  const mm = String(d.getUTCMinutes()).padStart(2, "0");
  return `${day}${hh}:${mm}`;
}

function formatPredDuration(s) {
  if (s >= 60) return `${Math.round(s / 60)} min`;
  return `${s}s`;
}

function renderSatPredictions(passes, error) {
  const list = document.getElementById("sat-pred-list");
  const status = document.getElementById("sat-pred-status");
  if (!list) return;

  if (error) {
    list.innerHTML = "";
    if (status) status.textContent = error;
    return;
  }

  if (!Array.isArray(passes) || passes.length === 0) {
    list.innerHTML = "";
    if (status) status.textContent = "No passes found in the next 24 hours.";
    return;
  }

  const fragment = document.createDocumentFragment();
  for (const pass of passes) {
    const row = document.createElement("div");
    row.className = "sat-pred-row";
    const elClass = pass.max_elevation_deg >= 45
      ? "sat-pred-el-high"
      : pass.max_elevation_deg >= 10
        ? "sat-pred-el-mid"
        : "sat-pred-el-low";
    const dir = `${azToCardinal(pass.azimuth_aos_deg)} → ${azToCardinal(pass.azimuth_los_deg)}`;
    row.innerHTML = [
      `<span class="sat-pred-col-time">${formatPredTime(pass.aos_ms)}</span>`,
      `<span class="sat-pred-col-sat">${pass.satellite}</span>`,
      `<span class="sat-pred-col-el ${elClass}">${pass.max_elevation_deg.toFixed(1)}°</span>`,
      `<span class="sat-pred-col-dur">${formatPredDuration(pass.duration_s)}</span>`,
      `<span class="sat-pred-col-dir">${dir}</span>`,
    ].join("");
    fragment.appendChild(row);
  }
  list.replaceChildren(fragment);
  if (status) status.textContent = `${passes.length} pass${passes.length === 1 ? "" : "es"} in the next 24 h · times in UTC`;
}

async function loadSatPredictions() {
  const status = document.getElementById("sat-pred-status");
  const list = document.getElementById("sat-pred-list");
  if (status) status.textContent = "Loading predictions\u2026";
  if (list) list.innerHTML = "";
  try {
    const resp = await fetch("/sat_passes");
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json();
    if (data.error) {
      renderSatPredictions([], data.error);
    } else {
      renderSatPredictions(data.passes || []);
    }
  } catch (e) {
    renderSatPredictions([], `Failed to load predictions: ${e.message}`);
  }
}

// ── Navigate to map centered on satellite image bounds ──────────────
window.wxsatShowOnMap = function (south, west, north, east) {
  // Enable wxsat filter if not active
  if (typeof window.enableMapSourceFilter === "function") {
    window.enableMapSourceFilter("wxsat");
  }
  // Navigate to the center of the image bounds
  const lat = (south + north) / 2;
  const lon = (west + east) / 2;
  if (window.navigateToAprsMap) {
    window.navigateToAprsMap(lat, lon);
  }
};

// ── Initial render ──────────────────────────────────────────────────
renderWxsatLatestCard();
renderWxsatHistoryTable();
