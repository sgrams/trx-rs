// --- SAT Plugin ---
// Live view: decoder state, latest image card
// History view: filterable table of all decoded images
// Predictions view: next 24 h passes for ham satellites

// ── DOM references ──────────────────────────────────────────────────
const satStatus = document.getElementById("sat-status");
const satLiveView = document.getElementById("sat-live-view");
const satHistoryView = document.getElementById("sat-history-view");
const satPredictionsView = document.getElementById("sat-predictions-view");
const satLiveLatest = document.getElementById("sat-live-latest");
const satHistoryList = document.getElementById("sat-history-list");
const satHistoryCount = document.getElementById("sat-history-count");
const satFilterInput = document.getElementById("sat-filter");
const satSortSelect = document.getElementById("sat-sort");
const satTypeFilter = document.getElementById("sat-type-filter");
const satAptState = document.getElementById("sat-apt-state");
const satLrptState = document.getElementById("sat-lrpt-state");

// ── State ───────────────────────────────────────────────────────────
let satImageHistory = [];
const SAT_MAX_IMAGES = 100;
const SAT_PRED_PAGE_SIZE = 50; // max rows before "Show more"
let satPredShowAll = false;
let satFilterText = "";
let satActiveView = "live"; // "live" | "history" | "predictions"

// ── UI scheduler helper ─────────────────────────────────────────────
function scheduleSatUi(key, job) {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob(key, job);
    return;
  }
  job();
}

// ── View switching ──────────────────────────────────────────────────
const satViewLiveBtn = document.getElementById("sat-view-live");
const satViewHistoryBtn = document.getElementById("sat-view-history");
const satViewPredictionsBtn = document.getElementById("sat-view-predictions");

function switchSatView(view) {
  const leavingPredictions = satActiveView === "predictions" && view !== "predictions";
  satActiveView = view;
  if (satLiveView) satLiveView.style.display = view === "live" ? "" : "none";
  if (satHistoryView) satHistoryView.style.display = view === "history" ? "" : "none";
  if (satPredictionsView) satPredictionsView.style.display = view === "predictions" ? "" : "none";
  if (satViewLiveBtn) satViewLiveBtn.classList.toggle("sat-view-active", view === "live");
  if (satViewHistoryBtn) satViewHistoryBtn.classList.toggle("sat-view-active", view === "history");
  if (satViewPredictionsBtn) satViewPredictionsBtn.classList.toggle("sat-view-active", view === "predictions");
  // Clear prediction DOM when leaving to reduce node count.
  if (leavingPredictions) {
    clearPredictionDom();
  }
  if (view === "history") {
    renderSatHistoryTable();
  } else if (view === "predictions") {
    satPredShowAll = false;
    loadSatPredictions();
  }
}

function clearPredictionDom() {
  if (satPredCountdownTimer) { clearInterval(satPredCountdownTimer); satPredCountdownTimer = null; }
  if (satPredCurrentList) satPredCurrentList.innerHTML = "";
  if (satPredUpcomingList) satPredUpcomingList.innerHTML = "";
}
window.clearSatPredictionDom = clearPredictionDom;

satViewLiveBtn?.addEventListener("click", () => switchSatView("live"));
satViewHistoryBtn?.addEventListener("click", () => switchSatView("history"));
satViewPredictionsBtn?.addEventListener("click", () => switchSatView("predictions"));

// ── Live view: decoder state ────────────────────────────────────────
// Updated from app.js render() via window.updateSatLiveState
window.updateSatLiveState = function (update) {
  if (!satAptState || !satLrptState) return;
  const aptOn = !!update.wxsat_decode_enabled;
  const lrptOn = !!update.lrpt_decode_enabled;

  satAptState.textContent = aptOn ? "Listening" : "Idle";
  satAptState.className = "sat-live-value " + (aptOn ? "sat-state-listening" : "sat-state-idle");
  satLrptState.textContent = lrptOn ? "Listening" : "Idle";
  satLrptState.className = "sat-live-value " + (lrptOn ? "sat-state-listening" : "sat-state-idle");
};

function renderSatLatestCard() {
  if (!satLiveLatest) return;
  if (satImageHistory.length === 0) {
    satLiveLatest.innerHTML =
      '<div style="color:var(--text-muted);font-size:0.82rem;">No images decoded yet. Enable a decoder and wait for a satellite pass.</div>';
    return;
  }

  const img = satImageHistory[0];
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

  let html = `<div class="sat-latest-card">`;
  html += `<div class="sat-latest-title">Latest decoded image</div>`;
  html += `<div class="sat-latest-meta">${meta.join(" &middot; ")}</div>`;
  if (img.path) {
    html += `<a href="${img.path}" target="_blank" style="font-size:0.8rem;color:var(--accent);display:inline-block;margin-top:0.25rem;">Download PNG</a>`;
  }
  if (img.geo_bounds) {
    html += ` <button type="button" class="sat-map-btn" onclick="window.satShowOnMap(${img.geo_bounds[0]},${img.geo_bounds[1]},${img.geo_bounds[2]},${img.geo_bounds[3]})" style="font-size:0.8rem;margin-top:0.25rem;margin-left:0.5rem;cursor:pointer;background:none;border:1px solid var(--accent);color:var(--accent);border-radius:3px;padding:1px 6px;">Show on Map</button>`;
  }
  html += `</div>`;
  satLiveLatest.innerHTML = html;
}

// ── History view: table ─────────────────────────────────────────────
function getSatFilteredHistory() {
  let items = satImageHistory;

  // Type filter
  const typeVal = satTypeFilter ? satTypeFilter.value : "all";
  if (typeVal === "apt") items = items.filter((i) => i._decoder === "apt");
  else if (typeVal === "lrpt") items = items.filter((i) => i._decoder === "lrpt");

  // Text filter
  if (satFilterText) {
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
      return haystack.includes(satFilterText);
    });
  }

  // Sort
  const sortVal = satSortSelect ? satSortSelect.value : "newest";
  if (sortVal === "oldest") {
    items = items.slice().reverse();
  }

  return items;
}

function renderSatHistoryRow(img) {
  const row = document.createElement("div");
  row.className = "sat-history-row";

  const decoder = img._decoder || "unknown";
  const typeName = decoder === "lrpt" ? "Meteor LRPT" : "NOAA APT";
  const typeClass = decoder === "lrpt" ? "sat-type-lrpt" : "sat-type-apt";
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
    link += ` <a href="javascript:void(0)" onclick="window.satShowOnMap(${img.geo_bounds[0]},${img.geo_bounds[1]},${img.geo_bounds[2]},${img.geo_bounds[3]})" style="color:var(--accent);">Map</a>`;
  }

  row.innerHTML = [
    `<span>${date} ${ts}</span>`,
    `<span class="sat-col-type ${typeClass}">${typeName}</span>`,
    `<span>${satellite}</span>`,
    `<span>${channels}</span>`,
    `<span>${lines} ${unit}</span>`,
    `<span>${link}</span>`,
  ].join("");

  return row;
}

function renderSatHistoryTable() {
  if (!satHistoryList) return;
  const items = getSatFilteredHistory();
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < items.length; i += 1) {
    fragment.appendChild(renderSatHistoryRow(items[i]));
  }
  satHistoryList.replaceChildren(fragment);

  if (satHistoryCount) {
    const total = satImageHistory.length;
    const shown = items.length;
    satHistoryCount.textContent =
      total === 0
        ? "No images yet"
        : shown === total
          ? `${total} image${total === 1 ? "" : "s"}`
          : `${shown} of ${total} images`;
  }
}

// ── Add image to history ────────────────────────────────────────────
function addSatImage(img, decoder) {
  const tsMs = Number.isFinite(img.ts_ms) ? Number(img.ts_ms) : Date.now();
  img._tsMs = tsMs;
  img._ts = new Date(tsMs).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
  img._decoder = decoder;

  satImageHistory.unshift(img);
  if (satImageHistory.length > SAT_MAX_IMAGES) {
    satImageHistory = satImageHistory.slice(0, SAT_MAX_IMAGES);
  }

  scheduleSatUi("sat-latest", () => renderSatLatestCard());
  if (satActiveView === "history") {
    scheduleSatUi("sat-history", () => renderSatHistoryTable());
  }
}

// ── Server callbacks ────────────────────────────────────────────────
window.onServerSatImage = function (msg) {
  if (satStatus) satStatus.textContent = "Image received (NOAA APT)";
  addSatImage(msg, "apt");
  if (msg.geo_bounds && msg.path && window.addSatMapOverlay) {
    window.addSatMapOverlay(msg);
  }
};

window.onServerLrptImage = function (msg) {
  if (satStatus) satStatus.textContent = "Image received (Meteor LRPT)";
  addSatImage(msg, "lrpt");
  if (msg.geo_bounds && msg.path && window.addSatMapOverlay) {
    window.addSatMapOverlay(msg);
  }
};

window.resetSatHistoryView = function () {
  satImageHistory = [];
  if (satHistoryList) satHistoryList.innerHTML = "";
  renderSatLatestCard();
  renderSatHistoryTable();
  if (window.clearSatMapOverlays) window.clearSatMapOverlays();
};

window.pruneSatHistoryView = function () {
  renderSatHistoryTable();
  renderSatLatestCard();
};

// ── Toggle buttons ──────────────────────────────────────────────────
const satDecodeToggleBtn = document.getElementById("sat-decode-toggle-btn");
satDecodeToggleBtn?.addEventListener("click", async () => {
  try {
    await window.takeSchedulerControlForDecoderDisable?.(satDecodeToggleBtn);
    await postPath("/toggle_wxsat_decode");
  } catch (e) {
    console.error("SAT toggle failed", e);
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
satFilterInput?.addEventListener("input", () => {
  satFilterText = satFilterInput.value.trim().toUpperCase();
  renderSatHistoryTable();
});

satSortSelect?.addEventListener("change", () => renderSatHistoryTable());
satTypeFilter?.addEventListener("change", () => renderSatHistoryTable());

// ── Settings: clear history ─────────────────────────────────────────
document
  .getElementById("settings-clear-sat-history")
  ?.addEventListener("click", async () => {
    try {
      await postPath("/clear_wxsat_decode");
      await postPath("/clear_lrpt_decode");
      window.resetSatHistoryView();
    } catch (e) {
      console.error("Weather satellite history clear failed", e);
    }
  });

// ── Predictions view ────────────────────────────────────────────────
let satPredData = [];
let satPredFilterText = "";
let satPredMinEl = 0;
let satPredCategory = "all";
let satPredSatCount = 0;
let satPredCountdownTimer = null;
const satPredFilterInput = document.getElementById("sat-pred-filter");
const satPredMinElSelect = document.getElementById("sat-pred-min-el");
const satPredCategorySelect = document.getElementById("sat-pred-category");
const satPredCurrentList = document.getElementById("sat-pred-current-list");
const satPredUpcomingList = document.getElementById("sat-pred-list");
const satPredCurrentSection = document.getElementById("sat-pred-current-section");
const satPredUpcomingSection = document.getElementById("sat-pred-upcoming-section");
const satPredStatus = document.getElementById("sat-pred-status");

function getFilteredPredictions() {
  let items = satPredData;
  if (satPredCategory !== "all") {
    items = items.filter((p) => p.category === satPredCategory);
  }
  if (satPredMinEl > 0) {
    items = items.filter((p) => p.max_elevation_deg >= satPredMinEl);
  }
  if (satPredFilterText) {
    items = items.filter((p) => p.satellite.toUpperCase().includes(satPredFilterText));
  }
  return items;
}

function applyPredFilters() {
  renderSatPredictions(getFilteredPredictions());
}

satPredFilterInput?.addEventListener("input", () => {
  satPredFilterText = satPredFilterInput.value.trim().toUpperCase();
  applyPredFilters();
});

satPredMinElSelect?.addEventListener("change", () => {
  satPredMinEl = parseInt(satPredMinElSelect.value, 10) || 0;
  applyPredFilters();
});

satPredCategorySelect?.addEventListener("change", () => {
  satPredCategory = satPredCategorySelect.value;
  applyPredFilters();
});

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

function formatCountdown(ms) {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

function renderSatPredictions(passes, error) {
  const currentList = satPredCurrentList;
  const upcomingList = satPredUpcomingList;
  const currentSection = satPredCurrentSection;
  const upcomingSection = satPredUpcomingSection;
  const status = satPredStatus;

  // Stop any previous countdown timer
  if (satPredCountdownTimer) { clearInterval(satPredCountdownTimer); satPredCountdownTimer = null; }

  if (error) {
    if (currentList) currentList.innerHTML = "";
    if (upcomingList) upcomingList.innerHTML = "";
    if (currentSection) currentSection.style.display = "none";
    if (upcomingSection) upcomingSection.style.display = "none";
    if (status) status.textContent = error;
    return;
  }

  if (!Array.isArray(passes) || passes.length === 0) {
    if (currentList) currentList.innerHTML = "";
    if (upcomingList) upcomingList.innerHTML = "";
    if (currentSection) currentSection.style.display = "none";
    if (upcomingSection) upcomingSection.style.display = "none";
    if (status) status.textContent = "No passes found in the next 24 hours.";
    return;
  }

  const now = Date.now();
  const current = passes.filter((p) => p.aos_ms <= now && p.los_ms > now);
  const upcoming = passes.filter((p) => p.aos_ms > now);

  // ── Current passes ──
  if (currentSection) currentSection.style.display = current.length > 0 ? "" : "none";
  if (currentList) {
    if (current.length === 0) {
      currentList.innerHTML = "";
    } else {
      const frag = document.createDocumentFragment();
      for (const pass of current) {
        const row = document.createElement("div");
        row.className = "sat-pred-row-current";
        const elClass = pass.max_elevation_deg >= 45
          ? "sat-pred-el-high"
          : pass.max_elevation_deg >= 10
            ? "sat-pred-el-mid"
            : "sat-pred-el-low";
        const dir = `${azToCardinal(pass.azimuth_aos_deg)} → ${azToCardinal(pass.azimuth_los_deg)}`;
        const remaining = Math.max(0, pass.los_ms - now);
        row.innerHTML = [
          `<span class="sat-pred-col-sat">${pass.satellite}</span>`,
          `<span class="sat-pred-col-el ${elClass}">${pass.max_elevation_deg.toFixed(1)}°</span>`,
          `<span class="sat-pred-col-time">${formatPredTime(pass.aos_ms)}</span>`,
          `<span class="sat-pred-col-time">${formatPredTime(pass.los_ms)}</span>`,
          `<span class="sat-pred-col-countdown" data-los="${pass.los_ms}">${formatCountdown(remaining)}</span>`,
          `<span class="sat-pred-col-dir">${dir}</span>`,
        ].join("");
        frag.appendChild(row);
      }
      currentList.replaceChildren(frag);
    }
  }

  // ── Upcoming passes (capped to reduce DOM node count) ──
  const upcomingLimit = satPredShowAll ? upcoming.length : SAT_PRED_PAGE_SIZE;
  const visibleUpcoming = upcoming.slice(0, upcomingLimit);
  const hiddenCount = upcoming.length - visibleUpcoming.length;
  if (upcomingSection) upcomingSection.style.display = upcoming.length > 0 ? "" : "none";
  if (upcomingList) {
    const frag = document.createDocumentFragment();
    for (const pass of visibleUpcoming) {
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
      frag.appendChild(row);
    }
    if (hiddenCount > 0) {
      const moreRow = document.createElement("div");
      moreRow.className = "sat-pred-row";
      moreRow.style.cursor = "pointer";
      moreRow.style.textAlign = "center";
      moreRow.innerHTML = `<span style="grid-column:1/-1;color:var(--accent);font-size:0.82rem;">Show ${hiddenCount} more passes\u2026</span>`;
      moreRow.addEventListener("click", () => {
        satPredShowAll = true;
        renderSatPredictions(getFilteredPredictions());
      });
      frag.appendChild(moreRow);
    }
    upcomingList.replaceChildren(frag);
  }

  // ── Status ──
  if (status) {
    let text = `${current.length} active · ${upcoming.length} upcoming · times in UTC`;
    if (satPredSatCount > 0) text += ` · ${satPredSatCount} satellites tracked`;
    status.textContent = text;
  }

  // ── Countdown timer: update "time left" every second ──
  // Only run when predictions view is actually visible.
  if (current.length > 0 && satActiveView === "predictions") {
    const countdownEls = currentList ? currentList.querySelectorAll(".sat-pred-col-countdown") : [];
    if (countdownEls.length > 0) {
      satPredCountdownTimer = setInterval(() => {
        // Pause timer if predictions view was hidden (e.g. switched tabs).
        if (satActiveView !== "predictions") {
          clearInterval(satPredCountdownTimer);
          satPredCountdownTimer = null;
          return;
        }
        const n = Date.now();
        let anyActive = false;
        for (const el of countdownEls) {
          const los = parseInt(el.dataset.los, 10);
          const rem = los - n;
          if (rem > 0) {
            el.textContent = formatCountdown(rem);
            anyActive = true;
          } else {
            el.textContent = "0:00";
          }
        }
        if (!anyActive) {
          clearInterval(satPredCountdownTimer);
          satPredCountdownTimer = null;
          renderSatPredictions(getFilteredPredictions());
        }
      }, 1000);
    }
  }
}

async function loadSatPredictions() {
  if (satPredStatus) satPredStatus.textContent = "Loading predictions\u2026";
  if (satPredCurrentList) satPredCurrentList.innerHTML = "";
  if (satPredUpcomingList) satPredUpcomingList.innerHTML = "";
  try {
    const resp = await fetch("/sat_passes");
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json();
    satPredSatCount = data.satellite_count || 0;
    if (data.error) {
      satPredData = [];
      renderSatPredictions([], data.error);
    } else {
      satPredData = data.passes || [];
      renderSatPredictions(getFilteredPredictions());
    }
  } catch (e) {
    renderSatPredictions([], `Failed to load predictions: ${e.message}`);
  }
}

// ── Navigate to map centered on satellite image bounds ──────────────
window.satShowOnMap = function (south, west, north, east) {
  // Enable sat filter if not active
  if (typeof window.enableMapSourceFilter === "function") {
    window.enableMapSourceFilter("sat");
  }
  // Navigate to the center of the image bounds
  const lat = (south + north) / 2;
  const lon = (west + east) / 2;
  if (window.navigateToAprsMap) {
    window.navigateToAprsMap(lat, lon);
  }
};

// ── Initial render ──────────────────────────────────────────────────
renderSatLatestCard();
renderSatHistoryTable();
