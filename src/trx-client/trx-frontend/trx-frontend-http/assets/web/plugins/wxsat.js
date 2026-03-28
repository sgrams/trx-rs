// --- Weather Satellite Decoder Plugin ---
const wxsatStatus = document.getElementById("wxsat-status");
const wxsatImagesEl = document.getElementById("wxsat-images");

let wxsatImageHistory = [];
const WXSAT_MAX_IMAGES = 20;

function scheduleWxsatUi(key, job) {
  if (typeof window.trxScheduleUiFrameJob === "function") {
    window.trxScheduleUiFrameJob(key, job);
    return;
  }
  job();
}

function renderWxsatImage(img) {
  const card = document.createElement("div");
  card.className = "wxsat-image-card";
  card.style.cssText =
    "border:1px solid var(--border-color);border-radius:0.5rem;padding:0.5rem;margin-bottom:0.75rem;background:var(--bg-secondary);";

  const ts = img._ts || new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const decoder = img._decoder || "unknown";
  const satellite = img.satellite || "";
  const channels = img.channels || "";
  const lines = img.line_count || img.mcu_count || 0;

  let metaParts = [`<strong>${decoder === "lrpt" ? "Meteor LRPT" : "NOAA APT"}</strong>`];
  if (satellite) metaParts.push(satellite);
  if (channels) metaParts.push("ch " + channels);
  metaParts.push(lines + (decoder === "lrpt" ? " MCU rows" : " lines"));
  metaParts.push(ts);

  card.innerHTML =
    `<div style="font-size:0.82rem;color:var(--text-muted);margin-bottom:0.35rem;">${metaParts.join(" &middot; ")}</div>`;

  if (img.path) {
    const link = document.createElement("a");
    link.href = img.path;
    link.target = "_blank";
    link.textContent = "Download image";
    link.style.cssText = "font-size:0.8rem;color:var(--accent);";
    card.appendChild(link);
  }

  return card;
}

function renderWxsatHistory() {
  if (!wxsatImagesEl) return;
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < wxsatImageHistory.length; i += 1) {
    fragment.appendChild(renderWxsatImage(wxsatImageHistory[i]));
  }
  wxsatImagesEl.replaceChildren(fragment);
}

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
  scheduleWxsatUi("wxsat-history", () => renderWxsatHistory());
}

// Server-dispatched callbacks
window.onServerWxsatImage = function (msg) {
  if (wxsatStatus) wxsatStatus.textContent = "Image received (NOAA APT)";
  addWxsatImage(msg, "apt");
};

window.onServerLrptImage = function (msg) {
  if (wxsatStatus) wxsatStatus.textContent = "Image received (Meteor LRPT)";
  addWxsatImage(msg, "lrpt");
};

window.resetWxsatHistoryView = function () {
  wxsatImageHistory = [];
  if (wxsatImagesEl) wxsatImagesEl.innerHTML = "";
};

// Toggle buttons
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

// Clear history button
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

// Initial render
renderWxsatHistory();
