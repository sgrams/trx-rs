// --- SAT Plugin ---
// Live view: decoder state, latest image card
// History view: filterable table of all decoded images
// Predictions view: next 24 h passes for ham satellites

// ── DOM references (cached once) ───────────────────────────────────
const satDom = {
  status:           document.getElementById("sat-status"),
  liveView:         document.getElementById("sat-live-view"),
  historyView:      document.getElementById("sat-history-view"),
  predictionsView:  document.getElementById("sat-predictions-view"),
  liveLatest:       document.getElementById("sat-live-latest"),
  historyList:      document.getElementById("sat-history-list"),
  historyCount:     document.getElementById("sat-history-count"),
  filterInput:      document.getElementById("sat-filter"),
  sortSelect:       document.getElementById("sat-sort"),
  typeFilter:       document.getElementById("sat-type-filter"),
  lrptState:        document.getElementById("sat-lrpt-state"),
  viewLiveBtn:      document.getElementById("sat-view-live"),
  viewHistoryBtn:   document.getElementById("sat-view-history"),
  viewPredBtn:      document.getElementById("sat-view-predictions"),
  predFilter:       document.getElementById("sat-pred-filter"),
  predMinEl:        document.getElementById("sat-pred-min-el"),
  predCategory:     document.getElementById("sat-pred-category"),
  predCurrentList:  document.getElementById("sat-pred-current-list"),
  predUpcomingList: document.getElementById("sat-pred-list"),
  predCurrentSec:   document.getElementById("sat-pred-current-section"),
  predUpcomingSec:  document.getElementById("sat-pred-upcoming-section"),
  predStatus:       document.getElementById("sat-pred-status"),
};

// ── State ───────────────────────────────────────────────────────────
let satImageHistory = [];
const SAT_MAX_IMAGES = 100;
const SAT_PRED_PAGE_SIZE = 50;
let satPredShowAll = false;
let satFilterText = "";
let satActiveView = "live"; // "live" | "history" | "predictions"
let satPredData = [];
let satPredFilterText = "";
let satPredMinEl = 0;
let satPredCategory = "all";
let satPredSatCount = 0;
let satPredCountdownTimer = null;

// ── UI scheduler helper ─────────────────────────────────────────────
function scheduleSatUi(key, job) {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob(key, job);
    return;
  }
  job();
}

// ── View switching ──────────────────────────────────────────────────
function switchSatView(view) {
  const leavingPredictions = satActiveView === "predictions" && view !== "predictions";
  satActiveView = view;
  if (satDom.liveView)        satDom.liveView.style.display = view === "live" ? "" : "none";
  if (satDom.historyView)     satDom.historyView.style.display = view === "history" ? "" : "none";
  if (satDom.predictionsView) satDom.predictionsView.style.display = view === "predictions" ? "" : "none";
  if (satDom.viewLiveBtn)     satDom.viewLiveBtn.classList.toggle("sat-view-active", view === "live");
  if (satDom.viewHistoryBtn)  satDom.viewHistoryBtn.classList.toggle("sat-view-active", view === "history");
  if (satDom.viewPredBtn)     satDom.viewPredBtn.classList.toggle("sat-view-active", view === "predictions");
  if (leavingPredictions) clearPredictionDom();
  if (view === "history") {
    renderSatHistoryTable();
  } else if (view === "predictions") {
    satPredShowAll = false;
    loadSatPredictions();
  }
}

function clearPredictionDom() {
  stopCountdownTimer();
  if (satDom.predCurrentList)  satDom.predCurrentList.innerHTML = "";
  if (satDom.predUpcomingList) satDom.predUpcomingList.innerHTML = "";
}
window.clearSatPredictionDom = clearPredictionDom;

satDom.viewLiveBtn?.addEventListener("click", () => switchSatView("live"));
satDom.viewHistoryBtn?.addEventListener("click", () => switchSatView("history"));
satDom.viewPredBtn?.addEventListener("click", () => switchSatView("predictions"));

// ── Live view: decoder state ────────────────────────────────────────
let _lastSatLrptOn = null;
window.updateSatLiveState = function (update) {
  if (!satDom.lrptState) return;
  const lrptOn = !!update.lrpt_decode_enabled;
  if (lrptOn !== _lastSatLrptOn) {
    _lastSatLrptOn = lrptOn;
    satDom.lrptState.textContent = lrptOn ? "Listening" : "Idle";
    satDom.lrptState.className = "sat-live-value " + (lrptOn ? "sat-state-listening" : "sat-state-idle");
  }
};

function renderSatLatestCard() {
  if (!satDom.liveLatest) return;
  if (satImageHistory.length === 0) {
    satDom.liveLatest.innerHTML =
      '<div style="color:var(--text-muted);font-size:0.82rem;">No images decoded yet. Enable a decoder and wait for a satellite pass.</div>';
    return;
  }

  const img = satImageHistory[0];
  const decoder = img._decoder || "unknown";
  const typeName = "Meteor LRPT";
  const satellite = img.satellite || "";
  const channels = img.channels || img.channel_a || "";
  const lines = img.mcu_count || img.line_count || 0;
  const unit = "MCU rows";
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
  satDom.liveLatest.innerHTML = html;
}

// ── History view: table ─────────────────────────────────────────────
function getSatFilteredHistory() {
  let items = satImageHistory;

  const typeVal = satDom.typeFilter ? satDom.typeFilter.value : "all";
  if (typeVal === "lrpt") items = items.filter((i) => i._decoder === "lrpt");

  if (satFilterText) {
    items = items.filter((i) => {
      const haystack = [
        "meteor lrpt",
        i.satellite || "",
        i.channels || "",
        i.channel_a || "",
        i.channel_b || "",
      ].join(" ").toUpperCase();
      return haystack.includes(satFilterText);
    });
  }

  const sortVal = satDom.sortSelect ? satDom.sortSelect.value : "newest";
  if (sortVal === "oldest") items = items.slice().reverse();

  return items;
}

function renderSatHistoryRow(img) {
  const row = document.createElement("div");
  row.className = "sat-history-row";

  const decoder = img._decoder || "unknown";
  const typeName = "Meteor LRPT";
  const typeClass = "sat-type-lrpt";
  const ts = img._ts || "--";
  const date = img._tsMs ? new Date(img._tsMs).toLocaleDateString([], { month: "short", day: "numeric" }) : "";
  const satellite = img.satellite || "--";
  const channels = img.channels || "--";
  const lines = img.mcu_count || img.line_count || 0;
  const unit = "MCU";
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
  if (!satDom.historyList) return;
  const items = getSatFilteredHistory();
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < items.length; i += 1) {
    fragment.appendChild(renderSatHistoryRow(items[i]));
  }
  satDom.historyList.replaceChildren(fragment);

  if (satDom.historyCount) {
    const total = satImageHistory.length;
    const shown = items.length;
    satDom.historyCount.textContent =
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
window.onServerLrptImage = function (msg) {
  if (satDom.status) satDom.status.textContent = "Image received (Meteor LRPT)";
  addSatImage(msg, "lrpt");
  if (msg.geo_bounds && msg.path && window.addSatMapOverlay) {
    window.addSatMapOverlay(msg);
  }
};

window.resetSatHistoryView = function () {
  satImageHistory = [];
  if (satDom.historyList) satDom.historyList.innerHTML = "";
  renderSatLatestCard();
  renderSatHistoryTable();
  if (window.clearSatMapOverlays) window.clearSatMapOverlays();
};

window.pruneSatHistoryView = function () {
  renderSatHistoryTable();
  renderSatLatestCard();
};

// ── Toggle buttons ──────────────────────────────────────────────────
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
satDom.filterInput?.addEventListener("input", () => {
  satFilterText = satDom.filterInput.value.trim().toUpperCase();
  renderSatHistoryTable();
});

satDom.sortSelect?.addEventListener("change", () => renderSatHistoryTable());
satDom.typeFilter?.addEventListener("change", () => renderSatHistoryTable());

// ── Settings: clear history ─────────────────────────────────────────
document
  .getElementById("settings-clear-sat-history")
  ?.addEventListener("click", async () => {
    try {
      await postPath("/clear_lrpt_decode");
      window.resetSatHistoryView();
    } catch (e) {
      console.error("Weather satellite history clear failed", e);
    }
  });

// ── Predictions: helpers ────────────────────────────────────────────
function azToCardinal(deg) {
  const dirs = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
  return dirs[Math.round(deg / 45) % 8];
}

function formatPredTime(ms) {
  const d = new Date(ms);
  const now = new Date();
  const dayNames = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const day = d.getUTCDay() !== now.getUTCDay() ? dayNames[d.getUTCDay()] + " " : "";
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

function elevationClass(deg) {
  if (deg >= 45) return "sat-pred-el-high";
  if (deg >= 10) return "sat-pred-el-mid";
  return "sat-pred-el-low";
}

// ── Predictions: countdown timer management ─────────────────────────
function stopCountdownTimer() {
  if (satPredCountdownTimer) {
    clearInterval(satPredCountdownTimer);
    satPredCountdownTimer = null;
  }
}

function startCountdownTimer(container) {
  const countdownEls = container ? container.querySelectorAll(".sat-pred-col-countdown") : [];
  if (countdownEls.length === 0) return;

  satPredCountdownTimer = setInterval(() => {
    if (satActiveView !== "predictions") {
      stopCountdownTimer();
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
      stopCountdownTimer();
      renderSatPredictions(getFilteredPredictions());
    }
  }, 1000);
}

// ── Predictions: row builders ───────────────────────────────────────
function buildCurrentPassRow(pass, now) {
  const row = document.createElement("div");
  row.className = "sat-pred-row-current";
  const dir = `${azToCardinal(pass.azimuth_aos_deg)} \u2192 ${azToCardinal(pass.azimuth_los_deg)}`;
  const remaining = Math.max(0, pass.los_ms - now);
  row.innerHTML = [
    `<span class="sat-pred-col-sat">${pass.satellite}</span>`,
    `<span class="sat-pred-col-el ${elevationClass(pass.max_elevation_deg)}">${pass.max_elevation_deg.toFixed(1)}\u00B0</span>`,
    `<span class="sat-pred-col-time">${formatPredTime(pass.aos_ms)}</span>`,
    `<span class="sat-pred-col-time">${formatPredTime(pass.los_ms)}</span>`,
    `<span class="sat-pred-col-countdown" data-los="${pass.los_ms}">${formatCountdown(remaining)}</span>`,
    `<span class="sat-pred-col-dir">${dir}</span>`,
  ].join("");
  return row;
}

function buildUpcomingPassRow(pass) {
  const row = document.createElement("div");
  row.className = "sat-pred-row";
  const dir = `${azToCardinal(pass.azimuth_aos_deg)} \u2192 ${azToCardinal(pass.azimuth_los_deg)}`;
  row.innerHTML = [
    `<span class="sat-pred-col-time">${formatPredTime(pass.aos_ms)}</span>`,
    `<span class="sat-pred-col-sat">${pass.satellite}</span>`,
    `<span class="sat-pred-col-el ${elevationClass(pass.max_elevation_deg)}">${pass.max_elevation_deg.toFixed(1)}\u00B0</span>`,
    `<span class="sat-pred-col-dur">${formatPredDuration(pass.duration_s)}</span>`,
    `<span class="sat-pred-col-dir">${dir}</span>`,
  ].join("");
  return row;
}

// ── Predictions: filter state ───────────────────────────────────────
function getFilteredPredictions() {
  let items = satPredData;
  if (satPredCategory !== "all") items = items.filter((p) => p.category === satPredCategory);
  if (satPredMinEl > 0) items = items.filter((p) => p.max_elevation_deg >= satPredMinEl);
  if (satPredFilterText) items = items.filter((p) => p.satellite.toUpperCase().includes(satPredFilterText));
  return items;
}

function applyPredFilters() {
  renderSatPredictions(getFilteredPredictions());
}

satDom.predFilter?.addEventListener("input", () => {
  satPredFilterText = satDom.predFilter.value.trim().toUpperCase();
  applyPredFilters();
});

satDom.predMinEl?.addEventListener("change", () => {
  satPredMinEl = parseInt(satDom.predMinEl.value, 10) || 0;
  applyPredFilters();
});

satDom.predCategory?.addEventListener("change", () => {
  satPredCategory = satDom.predCategory.value;
  applyPredFilters();
});

// ── Predictions: main render ────────────────────────────────────────
function renderSatPredictions(passes, error) {
  stopCountdownTimer();

  if (error) {
    if (satDom.predCurrentList)  satDom.predCurrentList.innerHTML = "";
    if (satDom.predUpcomingList) satDom.predUpcomingList.innerHTML = "";
    if (satDom.predCurrentSec)   satDom.predCurrentSec.style.display = "none";
    if (satDom.predUpcomingSec)  satDom.predUpcomingSec.style.display = "none";
    if (satDom.predStatus)       satDom.predStatus.textContent = error;
    return;
  }

  if (!Array.isArray(passes) || passes.length === 0) {
    if (satDom.predCurrentList)  satDom.predCurrentList.innerHTML = "";
    if (satDom.predUpcomingList) satDom.predUpcomingList.innerHTML = "";
    if (satDom.predCurrentSec)   satDom.predCurrentSec.style.display = "none";
    if (satDom.predUpcomingSec)  satDom.predUpcomingSec.style.display = "none";
    if (satDom.predStatus)       satDom.predStatus.textContent = "No passes found in the next 24 hours.";
    return;
  }

  const now = Date.now();
  const current = passes.filter((p) => p.aos_ms <= now && p.los_ms > now);
  const upcoming = passes.filter((p) => p.aos_ms > now);

  // ── Current passes ──
  if (satDom.predCurrentSec) satDom.predCurrentSec.style.display = current.length > 0 ? "" : "none";
  if (satDom.predCurrentList) {
    if (current.length === 0) {
      satDom.predCurrentList.innerHTML = "";
    } else {
      const frag = document.createDocumentFragment();
      for (const pass of current) frag.appendChild(buildCurrentPassRow(pass, now));
      satDom.predCurrentList.replaceChildren(frag);
    }
  }

  // ── Upcoming passes ──
  const upcomingLimit = satPredShowAll ? upcoming.length : SAT_PRED_PAGE_SIZE;
  const visibleUpcoming = upcoming.slice(0, upcomingLimit);
  const hiddenCount = upcoming.length - visibleUpcoming.length;
  if (satDom.predUpcomingSec) satDom.predUpcomingSec.style.display = upcoming.length > 0 ? "" : "none";
  if (satDom.predUpcomingList) {
    const frag = document.createDocumentFragment();
    for (const pass of visibleUpcoming) frag.appendChild(buildUpcomingPassRow(pass));
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
    satDom.predUpcomingList.replaceChildren(frag);
  }

  // ── Status ──
  if (satDom.predStatus) {
    let text = `${current.length} active \u00B7 ${upcoming.length} upcoming \u00B7 times in UTC`;
    if (satPredSatCount > 0) text += ` \u00B7 ${satPredSatCount} satellites tracked`;
    satDom.predStatus.textContent = text;
  }

  // ── Countdown timer ──
  if (current.length > 0 && satActiveView === "predictions") {
    startCountdownTimer(satDom.predCurrentList);
  }
}

// ── Predictions: data loading ───────────────────────────────────────
async function loadSatPredictions() {
  if (satDom.predStatus)       satDom.predStatus.textContent = "Loading predictions\u2026";
  if (satDom.predCurrentList)  satDom.predCurrentList.innerHTML = "";
  if (satDom.predUpcomingList) satDom.predUpcomingList.innerHTML = "";
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
  if (typeof window.enableMapSourceFilter === "function") {
    window.enableMapSourceFilter("sat");
  }
  const lat = (south + north) / 2;
  const lon = (west + east) / 2;
  if (window.navigateToAprsMap) {
    window.navigateToAprsMap(lat, lon);
  }
};

// ── Initial render ──────────────────────────────────────────────────
renderSatLatestCard();
renderSatHistoryTable();
