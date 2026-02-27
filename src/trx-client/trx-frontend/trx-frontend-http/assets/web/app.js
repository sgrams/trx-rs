// --- Persistent settings (localStorage) ---
const STORAGE_PREFIX = "trx_";
function saveSetting(key, value) {
  try { localStorage.setItem(STORAGE_PREFIX + key, JSON.stringify(value)); } catch(e) {}
}
function loadSetting(key, fallback) {
  try {
    const v = localStorage.getItem(STORAGE_PREFIX + key);
    return v !== null ? JSON.parse(v) : fallback;
  } catch(e) { return fallback; }
}

// --- Authentication ---
let authRole = null;  // null (not authenticated), "rx" (read-only), or "control" (full access)
let authEnabled = true;

async function checkAuthStatus() {
  try {
    const resp = await fetch("/auth/session");
    if (resp.status === 404) {
      // Auth API not exposed -> treat as auth-disabled mode.
      return { authenticated: true, role: "control", auth_disabled: true };
    }
    if (!resp.ok) return { authenticated: false };
    const data = await resp.json();
    return data;
  } catch (e) {
    console.error("Auth check failed:", e);
    return { authenticated: false };
  }
}

async function authLogin(passphrase) {
  try {
    const resp = await fetch("/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ passphrase }),
    });
    if (resp.status === 404) {
      return { authenticated: true, role: "control", auth_disabled: true };
    }
    if (!resp.ok) {
      const text = await resp.text();
      throw new Error(text || "Login failed");
    }
    const data = await resp.json();
    return data;
  } catch (e) {
    throw e;
  }
}

async function authLogout() {
  try {
    const resp = await fetch("/auth/logout", { method: "POST" });
    if (resp.status !== 404 && !resp.ok) throw new Error("Logout failed");
    authRole = null;
    // Disconnect and show auth gate without page reload
    disconnect();
    document.getElementById("content").style.display = "none";
    document.getElementById("loading").style.display = "none";
    document.getElementById("auth-passphrase").value = "";
    updateAuthUI();

    // Check if guest mode is available after logout
    const authStatus = await checkAuthStatus();
    const allowGuest = authStatus.role === "rx";
    showAuthGate(allowGuest);
  } catch (e) {
    console.error("Logout failed:", e);
    showAuthError("Logout failed");
  }
}

function showAuthGate(allowGuest = false) {
  if (!authEnabled) return;
  document.getElementById("loading").style.display = "none";
  document.getElementById("content").style.display = "none";
  document.getElementById("auth-gate").style.display = "block";
  document.getElementById("tab-bar").style.display = "none";

  // Hide rig picker since no rigs are accessible without auth
  const rigSwitch = document.querySelector(".header-rig-switch");
  if (rigSwitch) {
    rigSwitch.style.display = "none";
  }

  // Hide all tab panels
  document.querySelectorAll(".tab-panel").forEach(panel => {
    panel.style.display = "none";
  });

  // Show guest button if guest mode is available
  const guestBtn = document.getElementById("auth-guest-btn");
  if (guestBtn) {
    guestBtn.style.display = allowGuest ? "block" : "none";
  }
}

function hideAuthGate() {
  document.getElementById("auth-gate").style.display = "none";
  document.getElementById("loading").style.display = "block";
  document.getElementById("tab-bar").style.display = "";

  // Show rig picker again now that user is authenticated
  const rigSwitch = document.querySelector(".header-rig-switch");
  if (rigSwitch) {
    rigSwitch.style.display = "";
  }

  // Show Main tab by default and hide all other tabs
  document.querySelectorAll(".tab-panel").forEach(panel => {
    panel.style.display = "none";
  });
  const mainTab = document.getElementById("tab-main");
  if (mainTab) {
    mainTab.style.display = "";
  }

  // Mark Main tab button as active
  document.querySelectorAll(".tab-bar .tab").forEach(btn => {
    btn.classList.remove("active");
  });
  const mainTabBtn = document.querySelector(".tab-bar .tab[data-tab='main']");
  if (mainTabBtn) {
    mainTabBtn.classList.add("active");
  }
}

function showAuthError(msg) {
  const el = document.getElementById("auth-error");
  el.textContent = msg;
  el.style.display = "block";
  setTimeout(() => {
    el.style.display = "none";
  }, 5000);
}

function updateAuthUI() {
  const badge = document.getElementById("auth-badge");
  const badgeRole = document.getElementById("auth-role-badge");
  const headerAuthBtn = document.getElementById("header-auth-btn");

  if (!authEnabled) {
    if (badge) badge.style.display = "none";
    if (headerAuthBtn) headerAuthBtn.style.display = "none";
    return;
  }

  if (authRole) {
    badge.style.display = "block";
    badgeRole.textContent = authRole === "control" ? "Control (full access)" : "RX (read-only)";
    if (headerAuthBtn) {
      headerAuthBtn.textContent = "Logout";
      headerAuthBtn.style.display = "block";
    }
  } else {
    badge.style.display = "none";
    if (headerAuthBtn) {
      headerAuthBtn.textContent = "Login";
      headerAuthBtn.style.display = "block";
    }
  }
}

function applyAuthRestrictions() {
  if (!authRole) return;

  // Disable TX/PTT/frequency/mode/VFO controls for rx role
  if (authRole === "rx") {
    const pttBtn = document.getElementById("ptt-btn");
    const powerBtn = document.getElementById("power-btn");
    const lockBtn = document.getElementById("lock-btn");
    const freqInput = document.getElementById("freq");
    const modeSelect = document.getElementById("mode");
    const txLimitInput = document.getElementById("tx-limit");
    const txLimitBtn = document.getElementById("tx-limit-btn");
    const txAudioBtn = document.getElementById("tx-audio-btn");
    const txLimitRow = document.getElementById("tx-limit-row");
    const vfoPicker = document.getElementById("vfo-picker");
    const jogUp = document.getElementById("jog-up");
    const jogDown = document.getElementById("jog-down");
    const jogButtons = document.querySelectorAll(".jog-step button");
    const vfoButtons = document.querySelectorAll("#vfo-picker button");

    // Disable TX buttons
    if (pttBtn) pttBtn.disabled = true;
    if (powerBtn) powerBtn.disabled = true;
    if (lockBtn) lockBtn.disabled = true;
    if (txAudioBtn) txAudioBtn.disabled = true;
    if (txLimitBtn) txLimitBtn.disabled = true;

    // Disable frequency/mode inputs
    if (freqInput) freqInput.disabled = true;
    if (modeSelect) modeSelect.disabled = true;
    if (txLimitInput) txLimitInput.disabled = true;

    // Disable VFO selector
    vfoButtons.forEach(btn => btn.disabled = true);

    // Disable jog controls
    const jogWheel = document.getElementById("jog-wheel");
    if (jogUp) jogUp.disabled = true;
    if (jogDown) jogDown.disabled = true;
    if (jogWheel) jogWheel.style.opacity = "0.5";
    jogButtons.forEach(btn => btn.disabled = true);

    // Disable plugin enable/disable buttons and decode history clear buttons
    // Note: sig-clear-btn is allowed for RX (clears local measurements only)
    const pluginToggleBtns = [
      "ft8-decode-toggle-btn",
      "wspr-decode-toggle-btn",
      "cw-auto",
      "aprs-clear-btn",
      "ft8-clear-btn",
      "wspr-clear-btn",
      "cw-clear-btn"
    ];
    pluginToggleBtns.forEach(id => {
      const btn = document.getElementById(id);
      if (btn && btn.tagName === "BUTTON") {
        btn.disabled = true;
      } else if (btn && btn.type === "checkbox") {
        btn.disabled = true;
      }
    });

    // Hide TX-specific UI but keep controls visible (disabled)
    if (txLimitRow) txLimitRow.style.opacity = "0.5";
  }
}

function applyCapabilities(caps) {
  if (!caps) return;

  // PTT / TX controls
  const pttBtn = document.getElementById("ptt-btn");
  const txMetersRow = document.getElementById("tx-meters");
  if (pttBtn) pttBtn.style.display = caps.tx ? "" : "none";
  if (txMetersRow) txMetersRow.style.display = caps.tx ? "" : "none";

  // TX limit row
  const txLimitRow = document.getElementById("tx-limit-row");
  if (txLimitRow && !caps.tx_limit) txLimitRow.style.display = "none";

  // VFO row
  const vfoRow = document.getElementById("vfo-row");
  if (vfoRow) vfoRow.style.display = caps.vfo_switch ? "" : "none";

  // Signal meter row
  const sigRow = document.querySelector(".full-row.label-below-row");
  // Find signal row by content check rather than class (it may share classes)
  document.querySelectorAll(".full-row.label-below-row").forEach(row => {
    const label = row.querySelector(".label span");
    if (label && label.textContent === "Signal") {
      row.style.display = caps.signal_meter ? "" : "none";
    }
  });

  // Filters panel
  const filtersPanel = document.getElementById("filters-panel");
  if (filtersPanel) filtersPanel.style.display = caps.filter_controls ? "" : "none";

  // Spectrum panel (SDR-only)
  const spectrumPanel = document.getElementById("spectrum-panel");
  if (spectrumPanel) {
    if (caps.filter_controls) {
      spectrumPanel.style.display = "";
      startSpectrumPolling();
    } else {
      spectrumPanel.style.display = "none";
      stopSpectrumPolling();
    }
  }
}

const freqEl = document.getElementById("freq");
const wavelengthEl = document.getElementById("wavelength");
const modeEl = document.getElementById("mode");
const bandLabel = document.getElementById("band-label");
const powerBtn = document.getElementById("power-btn");
const powerHint = document.getElementById("power-hint");
const vfoPicker = document.getElementById("vfo-picker");
const signalBar = document.getElementById("signal-bar");
const signalValue = document.getElementById("signal-value");
const pttBtn = document.getElementById("ptt-btn");
const txLimitInput = document.getElementById("tx-limit");
const txLimitBtn = document.getElementById("tx-limit-btn");
const txLimitRow = document.getElementById("tx-limit-row");
const lockBtn = document.getElementById("lock-btn");
const txMeters = document.getElementById("tx-meters");
const pwrBar = document.getElementById("pwr-bar");
const pwrValue = document.getElementById("pwr-value");
const swrBar = document.getElementById("swr-bar");
const swrValue = document.getElementById("swr-value");
const loadingEl = document.getElementById("loading");
const contentEl = document.getElementById("content");
const serverSubtitle = document.getElementById("server-subtitle");
const rigSubtitle = document.getElementById("rig-subtitle");
const ownerSubtitle = document.getElementById("owner-subtitle");
const loadingTitle = document.getElementById("loading-title");
const loadingSub = document.getElementById("loading-sub");
const headerSigCanvas = document.getElementById("header-sig-canvas");
const themeToggleBtn = document.getElementById("theme-toggle");
const rigSwitchSelect = document.getElementById("rig-switch-select");
const rigSwitchBtn = document.getElementById("rig-switch-btn");
const headerRigSwitchSelect = document.getElementById("header-rig-switch-select");
const headerRigSwitchBtn = document.getElementById("header-rig-switch-btn");

let lastControl;
let lastTxEn = null;
let lastRendered = null;
let hintTimer = null;
let sigMeasuring = false;
let sigLastSUnits = null;
let sigMeasureTimer = null;
let sigMeasureLastTickMs = 0;
let sigMeasureAccumMs = 0;
let sigMeasureWeighted = 0;
let sigMeasurePeak = null;
let lastFreqHz = null;
let jogStep = loadSetting("jogStep", 1000);
let minFreqStepHz = 1;
const VFO_COLORS = ["var(--accent-green)", "var(--accent-yellow)"];
function vfoColor(idx) {
  if (idx < VFO_COLORS.length) return VFO_COLORS[idx];
  // Deterministic pseudo-random hue for extra VFOs
  const hue = ((idx * 137) % 360);
  return `hsl(${hue}, 70%, 55%)`;
}
let jogAngle = 0;
let lastClientCount = null;
let lastLocked = false;
let lastRigIds = [];
let lastRigDisplayNames = {};
const originalTitle = document.title;
const savedTheme = loadSetting("theme", null);

function currentTheme() {
  return document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark";
}

function setTheme(theme) {
  const next = theme === "light" ? "light" : "dark";
  document.documentElement.setAttribute("data-theme", next);
  saveSetting("theme", next);
  if (themeToggleBtn) {
    themeToggleBtn.textContent = next === "dark" ? "☀️ Light" : "🌙 Dark";
    themeToggleBtn.title = next === "dark" ? "Switch to light mode" : "Switch to dark mode";
  }
}

if (savedTheme === "light" || savedTheme === "dark") {
  setTheme(savedTheme);
} else {
  const prefersLight = window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches;
  setTheme(prefersLight ? "light" : "dark");
}

if (themeToggleBtn) {
  themeToggleBtn.addEventListener("click", () => {
    setTheme(currentTheme() === "dark" ? "light" : "dark");
    updateMapBaseLayerForTheme(currentTheme());
  });
}

function readyText() {
  return lastClientCount !== null ? `Ready \u00b7 ${lastClientCount} user${lastClientCount !== 1 ? "s" : ""}` : "Ready";
}

function populateRigPicker(selectEl, rigIds, activeRigId, disabled) {
  if (!selectEl) return;
  const selectedBefore = selectEl.value;
  selectEl.innerHTML = "";
  rigIds.forEach((id) => {
    const opt = document.createElement("option");
    opt.value = id;
    opt.textContent = id;
    selectEl.appendChild(opt);
  });
  const preferred = (typeof activeRigId === "string" && rigIds.includes(activeRigId))
    ? activeRigId
    : selectedBefore;
  if (preferred && rigIds.includes(preferred)) {
    selectEl.value = preferred;
  }
  selectEl.disabled = disabled;
}

function updateRigSubtitle(activeRigId) {
  if (!rigSubtitle) return;
  const name = (activeRigId && lastRigDisplayNames[activeRigId]) || activeRigId || "--";
  rigSubtitle.textContent = `Rig: ${name}`;
}

function applyRigList(activeRigId, rigIds, displayNames) {
  if (!Array.isArray(rigIds)) return;
  lastRigIds = rigIds.filter((id) => typeof id === "string" && id.length > 0);
  if (displayNames && typeof displayNames === "object") {
    lastRigDisplayNames = { ...displayNames };
  }
  const aboutList = document.getElementById("about-rig-list");
  if (aboutList) {
    aboutList.textContent = lastRigIds.length ? lastRigIds.join(", ") : "--";
  }
  if (typeof activeRigId === "string" && activeRigId.length > 0) {
    const aboutActive = document.getElementById("about-active-rig");
    if (aboutActive) aboutActive.textContent = activeRigId;
  }
  const disableSwitch = lastRigIds.length === 0 || authRole === "rx";
  populateRigPicker(rigSwitchSelect, lastRigIds, activeRigId, lastRigIds.length === 0);
  populateRigPicker(headerRigSwitchSelect, lastRigIds, activeRigId, lastRigIds.length === 0);
  if (rigSwitchBtn) rigSwitchBtn.disabled = disableSwitch;
  if (headerRigSwitchBtn) headerRigSwitchBtn.disabled = disableSwitch;
  updateRigSubtitle(activeRigId);
}

async function refreshRigList() {
  try {
    const resp = await fetch("/rigs", { cache: "no-store" });
    if (!resp.ok) return;
    const data = await resp.json();
    const rigs = Array.isArray(data.rigs) ? data.rigs : [];
    const rigIds = rigs.map((r) => r && r.rig_id).filter(Boolean);
    const displayNames = {};
    rigs.forEach((r) => {
      if (!r || !r.rig_id) return;
      if (typeof r.display_name === "string" && r.display_name.length > 0) {
        displayNames[r.rig_id] = r.display_name;
      } else {
        displayNames[r.rig_id] = r.rig_id;
      }
    });
    applyRigList(data.active_rig_id, rigIds, displayNames);
  } catch (e) {
    // Non-fatal: SSE/status path still drives main UI.
  }
}

function showHint(msg, duration) {
  powerHint.textContent = msg;
  if (hintTimer) clearTimeout(hintTimer);
  if (duration) hintTimer = setTimeout(() => { powerHint.textContent = readyText(); }, duration);
}
let supportedModes = [];
let supportedBands = [];
let freqDirty = false;
let initialized = false;
let lastEventAt = Date.now();
let es;
let esHeartbeat;
let reconnectTimer = null;
let headerSigSamples = [];
let headerSigTimer = null;
const HEADER_SIG_WINDOW_MS = 10_000;

function resizeHeaderSignalCanvas() {
  if (!headerSigCanvas) return;
  const cssW = Math.floor(headerSigCanvas.clientWidth);
  const cssH = Math.floor(headerSigCanvas.clientHeight);
  if (cssW <= 0 || cssH <= 0) return;
  const dpr = window.devicePixelRatio || 1;
  const nextW = Math.floor(cssW * dpr);
  const nextH = Math.floor(cssH * dpr);
  if (headerSigCanvas.width !== nextW || headerSigCanvas.height !== nextH) {
    headerSigCanvas.width = nextW;
    headerSigCanvas.height = nextH;
  }
  drawHeaderSignalGraph();
}

function pushHeaderSignalSample(sUnits) {
  if (!headerSigCanvas) return;
  const now = Date.now();
  const sample = Number.isFinite(sUnits) ? Math.max(0, Math.min(20, sUnits)) : 0;
  headerSigSamples.push({ t: now, v: sample });
  while (headerSigSamples.length && now - headerSigSamples[0].t > HEADER_SIG_WINDOW_MS) {
    headerSigSamples.shift();
  }
  drawHeaderSignalGraph();
}

function startHeaderSignalSampling() {
  if (!headerSigCanvas || headerSigTimer) return;
  headerSigTimer = setInterval(() => {
    pushHeaderSignalSample(Number.isFinite(sigLastSUnits) ? sigLastSUnits : 0);
  }, 120);
}

function drawHeaderSignalGraph() {
  if (!headerSigCanvas) return;
  const ctx = headerSigCanvas.getContext("2d");
  if (!ctx) return;
  const isLight = currentTheme() === "light";
  const dpr = window.devicePixelRatio || 1;
  const w = headerSigCanvas.width / dpr;
  const h = headerSigCanvas.height / dpr;
  if (w <= 0 || h <= 0) return;

  ctx.save();
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);

  // Soft horizontal guides for readability.
  ctx.strokeStyle = isLight ? "rgba(71, 85, 105, 0.26)" : "rgba(148, 163, 184, 0.16)";
  ctx.lineWidth = 1;
  for (let i = 1; i <= 3; i++) {
    const y = Math.round((h * i) / 4) + 0.5;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
  }

  // Minimal S-unit scale markers.
  const yFor = (v) => h - (Math.max(0, Math.min(20, v)) / 20) * (h - 2) - 1;
  ctx.fillStyle = isLight ? "rgba(30, 41, 59, 0.62)" : "rgba(154, 164, 181, 0.55)";
  ctx.font = "10px sans-serif";
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  [["S9+", 18], ["S9", 9], ["S0", 0]].forEach(([label, val]) => {
    const y = yFor(val);
    ctx.fillText(label, w - 4, y);
    ctx.strokeStyle = isLight ? "rgba(51, 65, 85, 0.22)" : "rgba(154, 164, 181, 0.22)";
    ctx.beginPath();
    ctx.moveTo(2, y + 0.5);
    ctx.lineTo(w - 36, y + 0.5);
    ctx.stroke();
  });

  if (headerSigSamples.length > 1) {
    const maxVal = 20; // includes S9+ scale overshoot.
    const toY = (v) => h - (Math.max(0, Math.min(maxVal, v)) / maxVal) * (h - 2) - 1;
    const now = Date.now();
    const windowStart = now - HEADER_SIG_WINDOW_MS;
    const toX = (t) => ((t - windowStart) / HEADER_SIG_WINDOW_MS) * w;
    const strengthGrad = ctx.createLinearGradient(0, h, 0, 0);
    const fillGrad = ctx.createLinearGradient(0, h, 0, 0);
    if (isLight) {
      // Higher-contrast palette for bright backgrounds.
      strengthGrad.addColorStop(0.0, "rgba(0, 86, 255, 0.95)");   // weak: deep blue
      strengthGrad.addColorStop(0.5, "rgba(0, 179, 255, 0.95)");
      strengthGrad.addColorStop(0.8, "rgba(255, 133, 0, 0.97)");
      strengthGrad.addColorStop(1.0, "rgba(224, 36, 36, 0.98)");  // strong: red
      fillGrad.addColorStop(0.0, "rgba(0, 86, 255, 0.18)");
      fillGrad.addColorStop(0.5, "rgba(0, 179, 255, 0.20)");
      fillGrad.addColorStop(0.8, "rgba(255, 133, 0, 0.22)");
      fillGrad.addColorStop(1.0, "rgba(224, 36, 36, 0.24)");
    } else {
      strengthGrad.addColorStop(0.0, "rgba(64, 120, 255, 0.88)"); // weak: blue
      strengthGrad.addColorStop(0.5, "rgba(106, 186, 255, 0.9)");
      strengthGrad.addColorStop(0.8, "rgba(255, 166, 77, 0.9)");
      strengthGrad.addColorStop(1.0, "rgba(255, 78, 78, 0.92)"); // strong: red
      fillGrad.addColorStop(0.0, "rgba(64, 120, 255, 0.12)");
      fillGrad.addColorStop(0.5, "rgba(106, 186, 255, 0.16)");
      fillGrad.addColorStop(0.8, "rgba(255, 166, 77, 0.18)");
      fillGrad.addColorStop(1.0, "rgba(255, 78, 78, 0.2)");
    }

    ctx.beginPath();
    headerSigSamples.forEach((sample, i) => {
      const x = toX(sample.t);
      const y = toY(sample.v);
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.lineTo(w, h);
    ctx.lineTo(0, h);
    ctx.closePath();
    ctx.fillStyle = fillGrad;
    ctx.fill();

    ctx.beginPath();
    headerSigSamples.forEach((sample, i) => {
      const x = toX(sample.t);
      const y = toY(sample.v);
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.strokeStyle = strengthGrad;
    ctx.lineWidth = 1.25;
    ctx.stroke();
  }

  ctx.restore();
}

function formatFreq(hz) {
  if (!Number.isFinite(hz)) return "--";
  if (hz >= 1_000_000_000) {
    return `${(hz / 1_000_000_000).toFixed(3)} GHz`;
  }
  if (hz >= 10_000_000) {
    return `${(hz / 1_000_000).toFixed(3)} MHz`;
  }
  return `${(hz / 1_000).toFixed(1)} kHz`;
}

function formatFreqForStep(hz, step) {
  if (!Number.isFinite(hz)) return "--";
  if (step >= 1_000_000) return (hz / 1_000_000).toFixed(6);
  if (step >= 1_000) return (hz / 1_000).toFixed(3);
  if (step >= 1) return String(Math.round(hz));
  return formatFreq(hz);
}

function formatWavelength(hz) {
  if (!Number.isFinite(hz) || hz <= 0) return "--";
  const meters = 299_792_458 / hz;
  if (meters >= 1) return `${Math.round(meters)} m`;
  return `${Math.round(meters * 100)} cm`;
}

function refreshWavelengthDisplay(hz) {
  if (!wavelengthEl) return;
  wavelengthEl.textContent = formatWavelength(hz);
}

function refreshFreqDisplay() {
  if (lastFreqHz == null || freqDirty) return;
  freqEl.value = formatFreqForStep(lastFreqHz, jogStep);
  refreshWavelengthDisplay(lastFreqHz);
}

function parseFreqInput(val, defaultStep) {
  if (!val) return null;
  const trimmed = val.trim().toLowerCase();
  const match = trimmed.match(/^([0-9]+(?:[.,][0-9]+)?)\s*([kmg]hz|[kmg]|hz)?$/);
  if (!match) return null;
  let num = parseFloat(match[1].replace(",", "."));
  const unit = match[2] || "";
  if (Number.isNaN(num)) return null;
  if (unit.startsWith("gh") || unit === "g") {
    num *= 1_000_000_000;
  } else if (unit.startsWith("mh") || unit === "m") {
    num *= 1_000_000;
  } else if (unit.startsWith("kh") || unit === "k") {
    num *= 1_000;
  } else if (!unit) {
    // Use currently selected input unit when user omits suffix.
    if (defaultStep >= 1_000_000) {
      num *= 1_000_000;
    } else if (defaultStep >= 1_000) {
      num *= 1_000;
    } else if (defaultStep >= 1) {
      // already Hz
    } else {
      // Fallback heuristic.
      if (num >= 1_000_000) {
        // Assume already Hz.
      } else if (num >= 1_000) {
        num *= 1_000;
      } else {
        num *= 1_000_000;
      }
    }
  }
  return Math.round(num);
}

function normalizeMinFreqStep(cap) {
  const val = Number(cap && cap.min_freq_step_hz);
  if (!Number.isFinite(val) || val < 1) return 1;
  return Math.round(val);
}

function alignFreqToRigStep(hz) {
  if (!Number.isFinite(hz)) return hz;
  const step = Math.max(1, minFreqStepHz);
  return Math.round(hz / step) * step;
}

function updateJogStepSupport(cap) {
  const nextMinStep = normalizeMinFreqStep(cap);
  minFreqStepHz = nextMinStep;

  const stepRoot = document.getElementById("jog-step");
  if (!stepRoot) return;
  const buttons = Array.from(stepRoot.querySelectorAll("button[data-step]"));
  if (buttons.length === 0) return;

  buttons.forEach((btn) => {
    const base = Number(btn.dataset.baseStep || btn.dataset.step);
    if (Number.isFinite(base) && base > 0) {
      btn.dataset.baseStep = String(Math.round(base));
      btn.dataset.step = String(Math.max(Math.round(base), minFreqStepHz));
    }
  });

  const steps = buttons
    .map((btn) => Number(btn.dataset.step))
    .filter((s) => Number.isFinite(s) && s > 0);
  if (steps.length === 0) return;

  const current = Number(jogStep);
  const desired =
    Number.isFinite(current) && current >= minFreqStepHz ? current : Math.max(steps[0], minFreqStepHz);

  jogStep = steps.reduce((best, s) => (Math.abs(s - desired) < Math.abs(best - desired) ? s : best), steps[0]);
  saveSetting("jogStep", jogStep);

  buttons.forEach((btn) => {
    btn.classList.toggle("active", Number(btn.dataset.step) === jogStep);
  });

  refreshFreqDisplay();
}

function normalizeMode(modeVal) {
  if (typeof modeVal === "string") return modeVal;
  if (modeVal && typeof modeVal === "object") {
    const entries = Object.entries(modeVal);
    if (entries.length > 0) {
      const [variant, value] = entries[0];
      if (variant === "Other" && typeof value === "string") return value;
      return variant;
    }
  }
  return "";
}

function updateSupportedBands(cap) {
  if (cap && Array.isArray(cap.supported_bands)) {
    supportedBands = cap.supported_bands
      .filter((b) => typeof b.low_hz === "number" && typeof b.high_hz === "number" && b.tx_allowed === true)
      .map((b) => ({ low: b.low_hz, high: b.high_hz }));
  } else {
    supportedBands = [];
  }
}

function freqAllowed(hz) {
  if (!Number.isFinite(hz)) return false;
  if (supportedBands.length === 0) return true; // if unknown, don't block
  return supportedBands.some((b) => hz >= b.low && hz <= b.high);
}

// Convert dBm (wire format) to S-units (S1=-121dBm, S9=-73dBm, 6dB/S-unit).
// Above S9, returns 9 + (overshoot in S-unit-equivalent, i.e. dB/10).
function dbmToSUnits(dbm) {
  if (!Number.isFinite(dbm)) return 0;
  // Guard against bogus backend values to keep display in a realistic range.
  const clampedDbm = Math.max(-140, Math.min(20, dbm));
  if (clampedDbm <= -121) return 0;
  if (clampedDbm >= -73) return 9 + (clampedDbm + 73) / 10;
  return (clampedDbm + 121) / 6;
}

function formatSignal(sUnits) {
  if (!Number.isFinite(sUnits) || sUnits <= 9) return `S${Math.max(0, sUnits || 0).toFixed(1)}`;
  // S9+60dB is already extremely strong; cap anything beyond that.
  const overDb = Math.min(60, (sUnits - 9) * 10);
  return `S9 + ${overDb.toFixed(0)}dB`;
}

function setDisabled(disabled) {
  [freqEl, modeEl, pttBtn, powerBtn, txLimitInput, txLimitBtn, lockBtn].forEach((el) => {
    if (el) el.disabled = disabled;
  });
}

let serverVersion = null;
let serverBuildDate = null;
let serverCallsign = null;
let ownerCallsign = null;
let serverLat = null;
let serverLon = null;

function updateFooterBuildInfo() {
  const serverEl = document.getElementById("footer-server-build");
  if (!serverEl) return;
  const ver = serverVersion || "--";
  const build = serverBuildDate || "--";
  serverEl.textContent = `trx-server v${ver} ${build}`;
}

function updateTitle() {
  document.getElementById("rig-title").textContent = originalTitle;
}

function render(update) {
  if (!update) return;
  if (update.server_version) serverVersion = update.server_version;
  if (update.server_build_date) serverBuildDate = update.server_build_date;
  if (update.server_callsign) serverCallsign = update.server_callsign;
  if (typeof update.owner_callsign === "string" && update.owner_callsign.length > 0) {
    ownerCallsign = update.owner_callsign;
  }
  if (update.server_latitude != null) serverLat = update.server_latitude;
  if (update.server_longitude != null) serverLon = update.server_longitude;
  updateTitle();
  updateFooterBuildInfo();

  initialized = !!update.initialized;
  const hasUsableSnapshot =
    !!update.info &&
    !!update.status &&
    !!update.status.freq &&
    typeof update.status.freq.hz === "number";
  if (!initialized) {
    const manu = (update.info && update.info.manufacturer) || rigName || "Rig";
    const model = (update.info && update.info.model) || rigName || "Rig";
    const rev = (update.info && update.info.revision) || "";
    const parts = [manu, model, rev].filter(Boolean).join(" ");
    if (!hasUsableSnapshot) {
      loadingTitle.textContent = `Initializing ${parts}…`;
      loadingSub.textContent = "";
      console.info("Rig initializing:", { manufacturer: manu, model, revision: rev });
      loadingEl.style.display = "";
      if (contentEl) contentEl.style.display = "none";
      powerHint.textContent = "Initializing rig…";
      setDisabled(true);
      return;
    }
    loadingEl.style.display = "none";
    if (contentEl) contentEl.style.display = "";
    powerHint.textContent = "Rig not fully initialized yet";
  } else {
    loadingEl.style.display = "none";
    if (contentEl) contentEl.style.display = "";
  }
  // Server subtitle: "trx-server vX.Y.Z hosted by CALL"
  if (serverSubtitle) {
    if (update.server_version && update.server_callsign) {
      const safeCallsign = escapeMapHtml(update.server_callsign);
      const encodedCallsign = encodeURIComponent(update.server_callsign);
      serverSubtitle.innerHTML =
        `trx-server v${update.server_version} hosted by <a href="https://qrzcq.com/call/${encodedCallsign}" target="_blank" rel="noopener">${safeCallsign}</a>`;
    } else if (update.server_version) {
      serverSubtitle.textContent = `trx-server v${update.server_version}`;
    } else if (update.server_callsign) {
      const safeCallsign = escapeMapHtml(update.server_callsign);
      const encodedCallsign = encodeURIComponent(update.server_callsign);
      serverSubtitle.innerHTML =
        `trx-server hosted by <a href="https://qrzcq.com/call/${encodedCallsign}" target="_blank" rel="noopener">${safeCallsign}</a>`;
    }
  }
  updateRigSubtitle(update.active_rig_id);
  if (ownerSubtitle) {
    if (ownerCallsign) {
      const safeOwner = escapeMapHtml(ownerCallsign);
      const encodedOwner = encodeURIComponent(ownerCallsign);
      ownerSubtitle.innerHTML =
        `Owner: <a href="https://qrzcq.com/call/${encodedOwner}" target="_blank" rel="noopener">${safeOwner}</a>`;
    } else {
      ownerSubtitle.textContent = "Owner: --";
    }
  }
  setDisabled(false);
  if (update.info && update.info.capabilities && Array.isArray(update.info.capabilities.supported_modes)) {
    const modes = update.info.capabilities.supported_modes.map(normalizeMode).filter(Boolean);
    if (JSON.stringify(modes) !== JSON.stringify(supportedModes)) {
      supportedModes = modes;
      modeEl.innerHTML = "";
      supportedModes.forEach((m) => {
        const opt = document.createElement("option");
        opt.value = m;
        opt.textContent = m;
        modeEl.appendChild(opt);
      });
    }
  }
  if (update.info && update.info.capabilities) {
    updateJogStepSupport(update.info.capabilities);
    updateSupportedBands(update.info.capabilities);
    applyCapabilities(update.info.capabilities);
  }
  // Sync filter state (SDR backends only)
  if (update.filter) {
    const bwSlider = document.getElementById("bw-slider");
    const bwValue = document.getElementById("bw-value");
    const firSelect = document.getElementById("fir-taps-select");
    if (bwSlider && typeof update.filter.bandwidth_hz === "number") {
      bwSlider.value = update.filter.bandwidth_hz;
      if (bwValue) bwValue.textContent = (update.filter.bandwidth_hz / 1000).toFixed(1) + " kHz";
    }
    if (firSelect && typeof update.filter.fir_taps === "number") {
      firSelect.value = String(update.filter.fir_taps);
    }
  }
  if (update.status && update.status.freq && typeof update.status.freq.hz === "number") {
    lastFreqHz = update.status.freq.hz;
    refreshWavelengthDisplay(lastFreqHz);
    if (!freqDirty) {
      refreshFreqDisplay();
    }
    window.ft8BaseHz = update.status.freq.hz;
    if (window.updateFt8RfDisplay) {
      window.updateFt8RfDisplay();
    }
  }
  if (update.status && update.status.mode) {
    const mode = normalizeMode(update.status.mode);
    modeEl.value = mode ? mode.toUpperCase() : "";
  }
  const modeUpper = update.status && update.status.mode ? normalizeMode(update.status.mode).toUpperCase() : "";
  const aprsStatus = document.getElementById("aprs-status");
  const cwStatus = document.getElementById("cw-status");
  const ft8Status = document.getElementById("ft8-status");
  const wsprStatus = document.getElementById("wspr-status");
  if (aprsStatus && modeUpper !== "PKT" && aprsStatus.textContent === "Receiving") {
    aprsStatus.textContent = "Connected, listening for packets";
  }
  if (cwStatus && modeUpper !== "CW" && modeUpper !== "CWR" && cwStatus.textContent === "Receiving") {
    cwStatus.textContent = "Connected, listening for packets";
  }
  const ft8Enabled = !!update.ft8_decode_enabled;
  if (ft8Status && (!ft8Enabled || (modeUpper !== "DIG" && modeUpper !== "USB")) && ft8Status.textContent === "Receiving") {
    ft8Status.textContent = "Connected, listening for packets";
  }
  const wsprEnabled = !!update.wspr_decode_enabled;
  if (wsprStatus && (!wsprEnabled || (modeUpper !== "DIG" && modeUpper !== "USB")) && wsprStatus.textContent === "Receiving") {
    wsprStatus.textContent = "Connected, listening for packets";
  }
  if (update.status && typeof update.status.tx_en === "boolean") {
    lastTxEn = update.status.tx_en;
    pttBtn.textContent = update.status.tx_en ? "PTT On" : "PTT Off";
    if (update.status.tx_en) {
      pttBtn.style.background = "var(--accent-red)";
      pttBtn.style.borderColor = "var(--accent-red)";
      pttBtn.style.color = "white";
    } else {
      pttBtn.style.background = "";
      pttBtn.style.borderColor = "";
      pttBtn.style.color = "";
    }
  }
  const ft8ToggleBtn = document.getElementById("ft8-decode-toggle-btn");
  if (ft8ToggleBtn) {
    const ft8On = !!update.ft8_decode_enabled;
    ft8ToggleBtn.textContent = ft8On ? "Disable FT8" : "Enable FT8";
    ft8ToggleBtn.style.borderColor = ft8On ? "#00d17f" : "";
    ft8ToggleBtn.style.color = ft8On ? "#00d17f" : "";
  }
  const wsprToggleBtn = document.getElementById("wspr-decode-toggle-btn");
  if (wsprToggleBtn) {
    const wsprOn = !!update.wspr_decode_enabled;
    wsprToggleBtn.textContent = wsprOn ? "Disable WSPR" : "Enable WSPR";
    wsprToggleBtn.style.borderColor = wsprOn ? "#00d17f" : "";
    wsprToggleBtn.style.color = wsprOn ? "#00d17f" : "";
  }
  const cwAutoEl = document.getElementById("cw-auto");
  const cwWpmEl = document.getElementById("cw-wpm");
  const cwToneEl = document.getElementById("cw-tone");
  if (cwAutoEl && typeof update.cw_auto === "boolean") {
    cwAutoEl.checked = update.cw_auto;
  }
  if (cwWpmEl && typeof update.cw_wpm === "number") {
    cwWpmEl.value = update.cw_wpm;
  }
  if (cwToneEl && typeof update.cw_tone_hz === "number") {
    cwToneEl.value = update.cw_tone_hz;
  }
  if (cwWpmEl && cwToneEl && typeof update.cw_auto === "boolean") {
    const disabled = update.cw_auto;
    cwWpmEl.disabled = disabled;
    cwWpmEl.readOnly = disabled;
    cwToneEl.disabled = disabled;
    cwToneEl.readOnly = disabled;
  }
  if (update.status && update.status.vfo && Array.isArray(update.status.vfo.entries)) {
    const entries = update.status.vfo.entries;
    const activeIdx = Number.isInteger(update.status.vfo.active) ? update.status.vfo.active : null;
    vfoPicker.innerHTML = "";
    entries.forEach((entry, idx) => {
      const hz = entry && entry.freq && typeof entry.freq.hz === "number" ? entry.freq.hz : null;
      if (hz === null) return;
      const mode = entry.mode ? normalizeMode(entry.mode) : "";
      const modeText = mode ? ` [${mode}]` : "";
      const label = `${entry.name || String.fromCharCode(65 + idx)}: ${formatFreq(hz)}${modeText}`;
      const btn = document.createElement("button");
      btn.type = "button";
      btn.textContent = label;
      const color = vfoColor(idx);
      if (activeIdx === idx) {
        btn.classList.add("active");
        btn.style.color = color;
        freqEl.style.color = color;
      } else btn.addEventListener("click", async () => {
        btn.disabled = true;
        showHint("Toggling VFO…");
        try {
          await postPath("/toggle_vfo");
          showHint("VFO toggled", 1200);
        } catch (err) {
          showHint("VFO toggle failed", 2000);
          console.error(err);
        } finally {
          btn.disabled = false;
        }
      });
      vfoPicker.appendChild(btn);
    });
  } else {
    vfoPicker.innerHTML = "<button type=\"button\" class=\"active\">--</button>";
  }
  if (update.status && update.status.rx && typeof update.status.rx.sig === "number") {
    const sUnits = dbmToSUnits(update.status.rx.sig);
    sigLastSUnits = sUnits;
    const pct = sUnits <= 9 ? Math.max(0, Math.min(100, (sUnits / 9) * 100)) : 100;
    signalBar.style.width = `${pct}%`;
    signalValue.textContent = formatSignal(sUnits);
  } else {
    sigLastSUnits = null;
    signalBar.style.width = "0%";
    signalValue.textContent = "--";
  }
  if (bandLabel) {
    bandLabel.textContent = typeof update.band === "string" ? update.band : "--";
  }
  if (typeof update.enabled === "boolean") {
    powerBtn.disabled = false;
    powerBtn.textContent = update.enabled ? "Power Off" : "Power On";
  } else {
    powerBtn.disabled = true;
    powerBtn.textContent = "Toggle Power";
    powerHint.textContent = "State unknown";
  }
  lastControl = update.enabled;

  if (update.status && update.status.tx && typeof update.status.tx.limit === "number") {
    txLimitInput.value = update.status.tx.limit;
    txLimitRow.style.display = "";
  } else {
    txLimitInput.value = "";
    txLimitRow.style.display = "none";
  }

  if (typeof update.clients === "number") lastClientCount = update.clients;
  // Populate About tab
  if (update.server_version) {
    document.getElementById("about-server-ver").textContent = `trx-server v${update.server_version}`;
  }
  document.getElementById("about-server-addr").textContent = location.host;
  if (update.server_callsign) {
    document.getElementById("about-server-call").textContent = update.server_callsign;
  }
  if (update.pskreporter_status) {
    document.getElementById("about-pskreporter").textContent = update.pskreporter_status;
  }
  if (update.info) {
    const parts = [update.info.manufacturer, update.info.model, update.info.revision].filter(Boolean).join(" ");
    if (parts) document.getElementById("about-rig-info").textContent = parts;
    const access = update.info.access;
    if (access) {
      if (access.Serial) {
        const serialPath = access.Serial.path || access.Serial.port || "?";
        document.getElementById("about-rig-access").textContent = `Serial (${serialPath}, ${access.Serial.baud || "?"} baud)`;
      } else if (access.Tcp) {
        document.getElementById("about-rig-access").textContent = `TCP (${access.Tcp.host || "?"}:${access.Tcp.port || "?"})`;
      } else {
        const key = Object.keys(access)[0];
        if (key) document.getElementById("about-rig-access").textContent = key;
      }
    }
    if (update.info.capabilities) {
      const cap = update.info.capabilities;
      if (Array.isArray(cap.supported_modes) && cap.supported_modes.length) {
        document.getElementById("about-modes").textContent = cap.supported_modes.map(normalizeMode).filter(Boolean).join(", ");
      }
      if (typeof cap.num_vfos === "number") {
        document.getElementById("about-vfos").textContent = cap.num_vfos;
      }
    }
  }
  if (typeof update.clients === "number") {
    document.getElementById("about-clients").textContent = update.clients;
  }
  if (typeof update.active_rig_id === "string" && update.active_rig_id.length > 0) {
    document.getElementById("about-active-rig").textContent = update.active_rig_id;
  }
  if (Array.isArray(update.rig_ids)) {
    applyRigList(update.active_rig_id, update.rig_ids);
  }
  if (typeof update.rigctl_clients === "number") {
    document.getElementById("about-rigctl-clients").textContent = update.rigctl_clients;
  }
  if (typeof update.rigctl_addr === "string" && update.rigctl_addr.length > 0) {
    document.getElementById("about-rigctl-endpoint").textContent = update.rigctl_addr;
  }
  powerHint.textContent = readyText();
  lastLocked = update.status && update.status.lock === true;
  lockBtn.textContent = lastLocked ? "Unlock" : "Lock";

  const tx = update.status && update.status.tx ? update.status.tx : null;
  txMeters.style.display = "";
  if (tx && typeof tx.power === "number") {
    const pct = Math.max(0, Math.min(100, tx.power));
    pwrBar.style.width = `${pct}%`;
    pwrValue.textContent = `PWR ${tx.power.toFixed(0)}%`;
  } else {
    pwrBar.style.width = "0%";
    pwrValue.textContent = "PWR --";
  }
  if (tx && typeof tx.swr === "number") {
    const swr = Math.max(1, tx.swr);
    const pct = Math.max(0, Math.min(100, ((swr - 1) / 2) * 100));
    swrBar.style.width = `${pct}%`;
    swrValue.textContent = `SWR ${tx.swr.toFixed(2)}`;
  } else {
    swrBar.style.width = "0%";
    swrValue.textContent = "SWR --";
  }
}

function scheduleReconnect(delayMs = 1000) {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, delayMs);
}

async function pollFreshSnapshot() {
  try {
    const resp = await fetch("/status", { cache: "no-store" });
    if (!resp.ok) return;
    const data = await resp.json();
    render(data);
    refreshRigList();
    lastEventAt = Date.now();
  } catch (e) {
    // Ignore network errors; connect() retry loop handles reconnection.
  }
}

function connect() {
  if (es) {
    es.close();
  }
  if (esHeartbeat) {
    clearInterval(esHeartbeat);
  }
  pollFreshSnapshot();
  es = new EventSource("/events");
  lastEventAt = Date.now();
  es.onopen = () => {
    pollFreshSnapshot();
    refreshRigList();
  };
  es.onmessage = (evt) => {
    try {
      if (evt.data === lastRendered) return;
      const data = JSON.parse(evt.data);
      lastRendered = evt.data;
      render(data);
      lastEventAt = Date.now();
      if (data.initialized) {
        powerHint.textContent = readyText();
      }
    } catch (e) {
      console.error("Bad event data", e);
    }
  };
  es.onerror = () => {
    // Check if this is an auth error by looking at readyState
    if (es.readyState === EventSource.CLOSED) {
      powerHint.textContent = "Disconnected, retrying…";
      es.close();
      pollFreshSnapshot();
      scheduleReconnect(1000);
    }
  };

  esHeartbeat = setInterval(() => {
    const now = Date.now();
    if (now - lastEventAt > 15000) {
      es.close();
      pollFreshSnapshot();
      scheduleReconnect(250);
    }
  }, 5000);
}

function disconnect() {
  // Close event sources
  if (es) {
    es.close();
    es = null;
  }
  if (decodeSource) {
    decodeSource.close();
    decodeSource = null;
  }
  // Clear timers
  if (esHeartbeat) {
    clearInterval(esHeartbeat);
    esHeartbeat = null;
  }
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
}

async function postPath(path) {
  const resp = await fetch(path, { method: "POST" });
  if (authEnabled && resp.status === 401) {
    // Not authenticated - return to login
    authRole = null;
    if (es) es.close();
    showAuthGate();
    throw new Error("Authentication required");
  }
  if (resp.status === 403) {
    // Authenticated but insufficient permissions - don't redirect
    throw new Error("Insufficient permissions");
  }
  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(text || resp.statusText);
  }
  return resp;
}

async function switchRigFromSelect(selectEl) {
  if (!selectEl || !selectEl.value) {
    showHint("No rig selected", 1500);
    return;
  }
  if (authRole === "rx") {
    showHint("Control role required", 1500);
    return;
  }
  if (!lastRigIds.includes(selectEl.value)) {
    showHint("Unknown rig", 1500);
    return;
  }
  if (rigSwitchBtn) rigSwitchBtn.disabled = true;
  if (headerRigSwitchBtn) headerRigSwitchBtn.disabled = true;
  showHint("Switching rig…");
  try {
    await postPath(`/select_rig?rig_id=${encodeURIComponent(selectEl.value)}`);
    refreshRigList();
    showHint("Rig switch requested", 1500);
  } catch (err) {
    showHint("Rig switch failed", 2000);
    console.error(err);
  } finally {
    const disableSwitch = lastRigIds.length === 0 || authRole === "rx";
    if (rigSwitchBtn) rigSwitchBtn.disabled = disableSwitch;
    if (headerRigSwitchBtn) headerRigSwitchBtn.disabled = disableSwitch;
  }
}

if (headerRigSwitchBtn) {
  headerRigSwitchBtn.addEventListener("click", () => switchRigFromSelect(headerRigSwitchSelect));
}

powerBtn.addEventListener("click", async () => {
  powerBtn.disabled = true;
  showHint("Sending...");
  try {
    await postPath("/toggle_power");
    showHint("Toggled, waiting for update…");
  } catch (err) {
    showHint("Toggle failed", 2000);
    console.error(err);
  } finally {
    powerBtn.disabled = false;
  }
});

pttBtn.addEventListener("click", async () => {
  pttBtn.disabled = true;
  showHint("Toggling PTT…");
  try {
    const desired = lastTxEn ? "false" : "true";
    await postPath(`/set_ptt?ptt=${desired}`);
    showHint("PTT command sent", 1500);
  } catch (err) {
    showHint("PTT toggle failed", 2000);
    console.error(err);
  } finally {
    pttBtn.disabled = false;
  }
});

async function applyFreqFromInput() {
  const parsedRaw = parseFreqInput(freqEl.value, jogStep);
  const parsed = alignFreqToRigStep(parsedRaw);
  if (parsed === null) {
    showHint("Freq missing", 1500);
    return;
  }
  if (!freqAllowed(parsed)) {
    showHint("Out of supported bands", 1500);
    return;
  }
  freqDirty = false;
  freqEl.disabled = true;
  showHint("Setting frequency…");
  try {
    await postPath(`/set_freq?hz=${parsed}`);
    showHint("Freq set", 1500);
  } catch (err) {
    showHint("Set freq failed", 2000);
    console.error(err);
  } finally {
    freqEl.disabled = false;
  }
}

freqEl.addEventListener("keydown", (e) => {
  freqDirty = true;
  if (e.key === "Enter") {
    e.preventDefault();
    applyFreqFromInput();
  }
});
freqEl.addEventListener("wheel", (e) => {
  e.preventDefault();
  const direction = e.deltaY < 0 ? 1 : -1;
  jogFreq(direction);
}, { passive: false });

// --- Jog wheel ---
const jogWheel = document.getElementById("jog-wheel");
const jogIndicator = document.getElementById("jog-indicator");
const jogDownBtn = document.getElementById("jog-down");
const jogUpBtn = document.getElementById("jog-up");
const jogStepEl = document.getElementById("jog-step");

async function jogFreq(direction) {
  if (lastLocked) { showHint("Locked", 1500); return; }
  if (lastFreqHz === null) return;
  const newHz = alignFreqToRigStep(lastFreqHz + direction * jogStep);
  if (!freqAllowed(newHz)) {
    showHint("Out of supported bands", 1500);
    return;
  }
  jogAngle = (jogAngle + direction * 15) % 360;
  jogIndicator.style.transform = `translateX(-50%) rotate(${jogAngle}deg)`;
  showHint("Setting frequency…");
  try {
    await postPath(`/set_freq?hz=${newHz}`);
    showHint("Freq set", 1000);
  } catch (err) {
    showHint("Set freq failed", 2000);
    console.error(err);
  }
}

jogDownBtn.addEventListener("click", () => jogFreq(-1));
jogUpBtn.addEventListener("click", () => jogFreq(1));

jogWheel.addEventListener("wheel", (e) => {
  e.preventDefault();
  const direction = e.deltaY < 0 ? 1 : -1;
  jogFreq(direction);
}, { passive: false });

// Touch drag on jog wheel
let jogTouchY = null;
jogWheel.addEventListener("touchstart", (e) => {
  e.preventDefault();
  jogTouchY = e.touches[0].clientY;
}, { passive: false });
jogWheel.addEventListener("touchmove", (e) => {
  e.preventDefault();
  if (jogTouchY === null) return;
  const dy = jogTouchY - e.touches[0].clientY;
  if (Math.abs(dy) > 12) {
    jogFreq(dy > 0 ? 1 : -1);
    jogTouchY = e.touches[0].clientY;
  }
}, { passive: false });
jogWheel.addEventListener("touchend", () => { jogTouchY = null; });

// Mouse drag on jog wheel
let jogMouseY = null;
jogWheel.addEventListener("mousedown", (e) => {
  e.preventDefault();
  jogMouseY = e.clientY;
  jogWheel.style.cursor = "grabbing";
});
window.addEventListener("mousemove", (e) => {
  if (jogMouseY === null) return;
  const dy = jogMouseY - e.clientY;
  if (Math.abs(dy) > 10) {
    jogFreq(dy > 0 ? 1 : -1);
    jogMouseY = e.clientY;
  }
});
window.addEventListener("mouseup", () => {
  jogMouseY = null;
  if (jogWheel) jogWheel.style.cursor = "grab";
});

// Step selector
jogStepEl.addEventListener("click", (e) => {
  const btn = e.target.closest("button[data-step]");
  if (!btn) return;
  jogStep = Math.max(parseInt(btn.dataset.step, 10), minFreqStepHz);
  jogStepEl.querySelectorAll("button").forEach((b) => b.classList.remove("active"));
  btn.classList.add("active");
  saveSetting("jogStep", jogStep);
  refreshFreqDisplay();
});

// Restore active jog step button from saved setting
{
  const buttons = Array.from(jogStepEl.querySelectorAll("button[data-step]"));
  const active =
    buttons.find((b) => parseInt(b.dataset.step, 10) === jogStep) ||
    buttons.find((b) => parseInt(b.dataset.step, 10) === 1000) ||
    buttons[0];
  if (active) {
    jogStep = parseInt(active.dataset.step, 10);
    buttons.forEach((b) => b.classList.toggle("active", b === active));
  }
}

async function applyModeFromPicker() {
  const mode = modeEl.value || "";
  if (!mode) {
    showHint("Mode missing", 1500);
    return;
  }
  modeEl.disabled = true;
  showHint("Setting mode…");
  try {
    await postPath(`/set_mode?mode=${encodeURIComponent(mode)}`);
    showHint("Mode set", 1500);
  } catch (err) {
    showHint("Set mode failed", 2000);
    console.error(err);
  } finally {
    modeEl.disabled = false;
  }
}

modeEl.addEventListener("change", applyModeFromPicker);

txLimitInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    txLimitBtn.click();
  }
});

txLimitBtn.addEventListener("click", async () => {
  const limit = txLimitInput.value;
  if (limit === "" || limit === "--") {
    showHint("Limit missing", 1500);
    return;
  }
  txLimitBtn.disabled = true;
  showHint("Setting TX limit…");
  try {
    await postPath(`/set_tx_limit?limit=${encodeURIComponent(limit)}`);
    showHint("TX limit set", 1500);
  } catch (err) {
    showHint("TX limit failed", 2000);
    console.error(err);
  } finally {
    txLimitBtn.disabled = false;
  }
});

lockBtn.addEventListener("click", async () => {
  lockBtn.disabled = true;
  showHint("Toggling lock…");
  try {
    const nextLock = lockBtn.textContent === "Lock";
    await postPath(nextLock ? "/lock" : "/unlock");
    showHint("Lock toggled", 1500);
  } catch (err) {
    showHint("Lock toggle failed", 2000);
    console.error(err);
  } finally {
    lockBtn.disabled = false;
  }
});

// --- Filter controls ---
(function () {
  const bwSlider = document.getElementById("bw-slider");
  const bwValue = document.getElementById("bw-value");
  const firSelect = document.getElementById("fir-taps-select");

  if (bwSlider) {
    bwSlider.addEventListener("input", () => {
      const hz = Number(bwSlider.value);
      if (bwValue) bwValue.textContent = (hz / 1000).toFixed(1) + " kHz";
    });
    bwSlider.addEventListener("change", async () => {
      const hz = Number(bwSlider.value);
      try {
        await postPath(`/set_bandwidth?hz=${encodeURIComponent(hz)}`);
      } catch (err) {
        showHint("Bandwidth set failed", 2000);
        console.error(err);
      }
    });
  }

  if (firSelect) {
    firSelect.addEventListener("change", async () => {
      const taps = Number(firSelect.value);
      try {
        await postPath(`/set_fir_taps?taps=${encodeURIComponent(taps)}`);
      } catch (err) {
        showHint("FIR taps set failed", 2000);
        console.error(err);
      }
    });
  }
})();

// --- Tab navigation ---
document.querySelector(".tab-bar").addEventListener("click", (e) => {
  const btn = e.target.closest(".tab[data-tab]");
  if (!btn) return;
  document.querySelectorAll(".tab-bar .tab").forEach((t) => t.classList.remove("active"));
  btn.classList.add("active");
  document.querySelectorAll(".tab-panel").forEach((p) => p.style.display = "none");
  document.getElementById(`tab-${btn.dataset.tab}`).style.display = "";
});

// --- Auth startup sequence ---
async function initializeApp() {
  const authStatus = await checkAuthStatus();
  authEnabled = !authStatus.auth_disabled;

  if (!authEnabled) {
    authRole = "control";
    hideAuthGate();
    updateAuthUI();
    connect();
    resizeHeaderSignalCanvas();
    startHeaderSignalSampling();
    return;
  }

  if (authStatus.authenticated) {
    // User has valid session
    authRole = authStatus.role;
    hideAuthGate();
    updateAuthUI();
    applyAuthRestrictions();
    connect();
    resizeHeaderSignalCanvas();
    startHeaderSignalSampling();
  } else {
    // No valid session - show auth gate
    // Guest button is shown if guest mode is available (role granted without auth)
    const allowGuest = authStatus.role === "rx";
    showAuthGate(allowGuest);
  }
}

// Setup auth form
document.getElementById("auth-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const passphrase = document.getElementById("auth-passphrase").value;
  const btn = document.querySelector("#auth-form button[type=submit]");
  btn.disabled = true;
  btn.textContent = "Logging in...";

  try {
    const result = await authLogin(passphrase);
    authRole = result.role;
    document.getElementById("auth-passphrase").value = "";
    hideAuthGate();
    updateAuthUI();
    applyAuthRestrictions();
    connect();
    resizeHeaderSignalCanvas();
    startHeaderSignalSampling();
  } catch (err) {
    showAuthError("Invalid passphrase");
    console.error("Login error:", err);
  } finally {
    btn.disabled = false;
    btn.textContent = "Login";
  }
});

// Setup guest button
const guestBtn = document.getElementById("auth-guest-btn");
if (guestBtn) {
  guestBtn.addEventListener("click", async () => {
    authRole = "rx";
    document.getElementById("auth-passphrase").value = "";
    hideAuthGate();
    updateAuthUI();
    applyAuthRestrictions();
    connect();
    resizeHeaderSignalCanvas();
    startHeaderSignalSampling();
  });
}

// Setup header auth button (Login/Logout)
const headerAuthBtn = document.getElementById("header-auth-btn");
if (headerAuthBtn) {
  headerAuthBtn.addEventListener("click", async () => {
    if (authRole) {
      // Logged in - show logout confirmation
      if (confirm("Are you sure you want to logout?")) {
        await authLogout();
      }
    } else {
      // Not logged in - show auth gate
      showAuthGate(false);
    }
  });
}

// Start the app
initializeApp();
window.addEventListener("resize", resizeHeaderSignalCanvas);

// --- Leaflet Map (lazy-initialized) ---
let aprsMap = null;
let aprsMapBaseLayer = null;
let aprsMapReceiverMarker = null;
const stationMarkers = new Map();
const locatorMarkers = new Map();
const mapMarkers = new Set();
const mapFilter = { aprs: true, ft8: true, wspr: true };

function mapTileSpecForTheme(theme) {
  if (theme === "dark") {
    return {
      url: "https://{s}.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}{r}.png",
      options: {
        maxZoom: 19,
        subdomains: "abcd",
        attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> &copy; <a href="https://carto.com/attributions">CARTO</a>',
      },
    };
  }
  return {
    url: "https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png",
    options: {
      maxZoom: 19,
      attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>',
    },
  };
}

function updateMapBaseLayerForTheme(theme) {
  if (!aprsMap) return;
  if (aprsMapBaseLayer) {
    aprsMap.removeLayer(aprsMapBaseLayer);
    aprsMapBaseLayer = null;
  }
  const spec = mapTileSpecForTheme(theme);
  aprsMapBaseLayer = L.tileLayer(spec.url, spec.options).addTo(aprsMap);
}

function initAprsMap() {
  const mapEl = document.getElementById("aprs-map");
  if (!mapEl) return;
  sizeAprsMapToViewport();
  if (aprsMap) return;

  const hasLocation = serverLat != null && serverLon != null;
  const center = hasLocation ? [serverLat, serverLon] : [20, 0];
  const zoom = hasLocation ? 10 : 2;

  aprsMap = L.map("aprs-map").setView(center, zoom);
  updateMapBaseLayerForTheme(currentTheme());

  if (hasLocation) {
    const popupText = serverCallsign ? serverCallsign : "Receiver";
    aprsMapReceiverMarker = L.circleMarker([serverLat, serverLon], {
      radius: 8, color: "#3388ff", fillColor: "#3388ff", fillOpacity: 0.8
    }).addTo(aprsMap).bindPopup(popupText);
  }

  const aprsFilter = document.getElementById("map-filter-aprs");
  const ft8Filter = document.getElementById("map-filter-ft8");
  const wsprFilter = document.getElementById("map-filter-wspr");
  if (aprsFilter) {
    aprsFilter.addEventListener("change", () => {
      mapFilter.aprs = aprsFilter.checked;
      applyMapFilter();
    });
  }
  if (ft8Filter) {
    ft8Filter.addEventListener("change", () => {
      mapFilter.ft8 = ft8Filter.checked;
      applyMapFilter();
    });
  }
  if (wsprFilter) {
    wsprFilter.addEventListener("change", () => {
      mapFilter.wspr = wsprFilter.checked;
      applyMapFilter();
    });
  }
}

function sizeAprsMapToViewport() {
  const mapEl = document.getElementById("aprs-map");
  if (!mapEl) return;
  const topPadding = parseFloat(getComputedStyle(document.body).paddingTop) || 0;
  const available = Math.max(0, window.innerHeight - topPadding);
  const target = Math.max(150, Math.floor(available * 0.6));
  mapEl.style.height = `${target}px`;
  if (aprsMap) aprsMap.invalidateSize();
}

function aprsSymbolIcon(symbolTable, symbolCode) {
  if (!symbolTable || !symbolCode) return null;
  const sheet = symbolTable === "/" ? 0 : 1;
  const code = symbolCode.charCodeAt(0) - 33;
  const col = code % 16;
  const row = Math.floor(code / 16);
  const bgX = -(col * 24);
  const bgY = -(row * 24);
  const url = `https://raw.githubusercontent.com/hessu/aprs-symbols/master/png/aprs-symbols-24-${sheet}.png`;
  return L.divIcon({
    className: "",
    html: `<div style="width:24px;height:24px;background:url('${url}') ${bgX}px ${bgY}px / 384px 192px no-repeat;"></div>`,
    iconSize: [24, 24],
    iconAnchor: [12, 12],
    popupAnchor: [0, -12]
  });
}

window.aprsMapAddStation = function(call, lat, lon, info, symbolTable, symbolCode) {
  if (!aprsMap) initAprsMap();
  if (!aprsMap) return;
  const popupContent = `<b>${call}</b><br>${info}`;
  const existing = stationMarkers.get(call);
  if (existing) {
    existing.marker.setLatLng([lat, lon]);
    existing.marker.setPopupContent(popupContent);
  } else {
    const icon = aprsSymbolIcon(symbolTable, symbolCode);
    const marker = icon
      ? L.marker([lat, lon], { icon }).addTo(aprsMap).bindPopup(popupContent)
      : L.circleMarker([lat, lon], {
          radius: 6, color: "#00d17f", fillColor: "#00d17f", fillOpacity: 0.8
        }).addTo(aprsMap).bindPopup(popupContent);
    marker.__trxType = "aprs";
    stationMarkers.set(call, { marker, type: "aprs" });
    mapMarkers.add(marker);
    applyMapFilter();
  }
};

function maidenheadToBounds(grid) {
  if (!grid || grid.length < 4) return null;
  const g = grid.toUpperCase();
  const A = "A".charCodeAt(0);
  const fieldLon = (g.charCodeAt(0) - A) * 20 - 180;
  const fieldLat = (g.charCodeAt(1) - A) * 10 - 90;
  const squareLon = parseInt(g[2], 10) * 2;
  const squareLat = parseInt(g[3], 10) * 1;

  let lon = fieldLon + squareLon;
  let lat = fieldLat + squareLat;
  let lonSpan = 2;
  let latSpan = 1;

  if (g.length >= 6) {
    const subLon = (g.charCodeAt(4) - A) * (5 / 60);
    const subLat = (g.charCodeAt(5) - A) * (2.5 / 60);
    lon += subLon;
    lat += subLat;
    lonSpan = 5 / 60;
    latSpan = 2.5 / 60;
  }

  return [
    [lat, lon],
    [lat + latSpan, lon + lonSpan],
  ];
}

function applyMapFilter() {
  if (!aprsMap) return;
  mapMarkers.forEach((marker) => {
    const type = marker.__trxType;
    const visible =
      (type === "aprs" && mapFilter.aprs) ||
      (type === "ft8" && mapFilter.ft8) ||
      (type === "wspr" && mapFilter.wspr);
    const onMap = aprsMap.hasLayer(marker);
    if (visible && !onMap) marker.addTo(aprsMap);
    if (!visible && onMap) marker.removeFrom(aprsMap);
  });
}

function escapeMapHtml(input) {
  return String(input)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("\"", "&quot;");
}

function locatorStyleForCount(count, type) {
  const safeCount = Math.max(1, Number.isFinite(count) ? count : 1);
  const intensity = Math.min(1, Math.log2(safeCount + 1) / 5);
  const isWspr = type === "wspr";
  return {
    color: isWspr ? "#ff8f2a" : "#ffb020",
    opacity: 0.45 + intensity * 0.5,
    weight: 1 + intensity * 1.2,
    fillColor: isWspr ? "#ff6a3d" : "#ff9b1a",
    fillOpacity: 0.18 + intensity * 0.55,
  };
}

window.ft8MapAddLocator = function(message, grids, type = "ft8", station = null) {
  if (!aprsMap) initAprsMap();
  if (!aprsMap) return;
  if (!Array.isArray(grids) || grids.length === 0) return;
  const markerType = type === "wspr" ? "wspr" : "ft8";
  const unique = [...new Set(grids.map((g) => String(g).toUpperCase()))];
  const locatorsLines = unique.map((g) => escapeMapHtml(g)).join("<br>");
  for (const grid of unique) {
    const bounds = maidenheadToBounds(grid);
    if (!bounds) continue;
    const key = `${markerType}:${grid}`;
    const stationId = station && String(station).trim() ? String(station).trim().toUpperCase() : "";
    const existing = locatorMarkers.get(key);
    if (existing) {
      if (stationId) existing.stations.add(stationId);
      const count = existing.stations.size || 1;
      existing.marker.setStyle(locatorStyleForCount(count, markerType));
      existing.marker.setPopupContent(
        `<b>${escapeMapHtml(grid)}</b><br>Stations: ${count}<br>${locatorsLines}`
      );
      continue;
    }

    const stations = new Set();
    if (stationId) stations.add(stationId);
    const count = stations.size || 1;
    const marker = L.rectangle(bounds, locatorStyleForCount(count, markerType))
      .addTo(aprsMap)
      .bindPopup(`<b>${escapeMapHtml(grid)}</b><br>Stations: ${count}<br>${locatorsLines}`);
    marker.__trxType = markerType;
    locatorMarkers.set(key, { marker, stations });
    mapMarkers.add(marker);
  }
  applyMapFilter();
};

// --- Sub-tab navigation (Plugins tab) ---
document.querySelectorAll(".sub-tab-bar").forEach((bar) => {
  bar.addEventListener("click", (e) => {
    const btn = e.target.closest(".sub-tab[data-subtab]");
    if (!btn) return;
    bar.querySelectorAll(".sub-tab").forEach((t) => t.classList.remove("active"));
    btn.classList.add("active");
    const parent = bar.parentElement;
    parent.querySelectorAll(".sub-tab-panel").forEach((p) => p.style.display = "none");
    parent.querySelector(`#subtab-${btn.dataset.subtab}`).style.display = "";
    if (btn.dataset.subtab === "map") {
      initAprsMap();
      sizeAprsMapToViewport();
      if (aprsMap) setTimeout(() => aprsMap.invalidateSize(), 50);
    }
  });
});

window.addEventListener("resize", () => {
  const mapTab = document.getElementById("subtab-map");
  if (!mapTab || mapTab.style.display === "none") return;
  sizeAprsMapToViewport();
});

// --- Signal measurement ---
const sigMeasureBtn = document.getElementById("sig-measure-btn");
const sigClearBtn = document.getElementById("sig-clear-btn");
const sigResult = document.getElementById("sig-result");

function resetSignalMeasurementState() {
  sigMeasureLastTickMs = 0;
  sigMeasureAccumMs = 0;
  sigMeasureWeighted = 0;
  sigMeasurePeak = null;
}

function updateSignalMeasurement(nowMs) {
  if (!sigMeasuring) return;
  if (sigMeasureLastTickMs === 0) {
    sigMeasureLastTickMs = nowMs;
    return;
  }
  const dt = Math.max(0, nowMs - sigMeasureLastTickMs);
  sigMeasureLastTickMs = nowMs;
  if (!Number.isFinite(sigLastSUnits)) return;

  sigMeasureAccumMs += dt;
  sigMeasureWeighted += sigLastSUnits * dt;
  if (sigMeasurePeak === null || sigLastSUnits > sigMeasurePeak) {
    sigMeasurePeak = sigLastSUnits;
  }
}

function stopSignalMeasurement() {
  if (sigMeasureTimer) {
    clearInterval(sigMeasureTimer);
    sigMeasureTimer = null;
  }
  sigMeasuring = false;
  sigMeasureBtn.textContent = "Measure";
  sigMeasureBtn.style.borderColor = "";
  sigMeasureBtn.style.color = "";
}

sigMeasureBtn.addEventListener("click", () => {
  if (!sigMeasuring) {
    resetSignalMeasurementState();
    sigMeasuring = true;
    sigMeasureBtn.textContent = "Stop (0.0s)";
    sigMeasureBtn.style.borderColor = "#00d17f";
    sigMeasureBtn.style.color = "#00d17f";
    sigMeasureTimer = setInterval(() => {
      const now = Date.now();
      updateSignalMeasurement(now);
      sigMeasureBtn.textContent = `Stop (${(sigMeasureAccumMs / 1000).toFixed(1)}s)`;
    }, 200);
  } else {
    updateSignalMeasurement(Date.now());
    stopSignalMeasurement();
    if (sigMeasureAccumMs > 0) {
      const avg = sigMeasureWeighted / sigMeasureAccumMs;
      const peak = sigMeasurePeak;
      sigResult.textContent = `Avg ${formatSignal(avg)} / Peak ${formatSignal(peak)} (${(sigMeasureAccumMs / 1000).toFixed(1)}s)`;
    }
  }
});

sigClearBtn.addEventListener("click", () => {
  stopSignalMeasurement();
  resetSignalMeasurementState();
  sigResult.textContent = "";
});

// --- Audio streaming ---
const rxAudioBtn = document.getElementById("rx-audio-btn");
const txAudioBtn = document.getElementById("tx-audio-btn");
const audioStatus = document.getElementById("audio-status");
const audioLevelFill = document.getElementById("audio-level-fill");
const audioRow = document.getElementById("audio-row");

// Hide audio row if audio is not configured on the server
fetch("/audio", { method: "GET" }).then((r) => {
  if (r.status === 404) audioRow.style.display = "none";
}).catch(() => {});

let audioWs = null;
let audioCtx = null;
let rxActive = false;
let txActive = false;
let txStream = null;
let txProcessor = null;
let streamInfo = null;
let opusDecoder = null;
let txEncoder = null;
let nextPlayTime = 0;
let lastLevelUpdate = 0;
let rxGainNode = null;
let txGainNode = null;
const rxVolSlider = document.getElementById("rx-vol");
const txVolSlider = document.getElementById("tx-vol");
const TX_TIMEOUT_SECS = 120;
let txTimeoutTimer = null;
let txTimeoutRemaining = 0;
let txTimeoutInterval = null;
const hasWebCodecs = typeof AudioDecoder !== "undefined" && typeof AudioEncoder !== "undefined";

// Show compatibility warning for non-Chromium browsers
if (!hasWebCodecs) {
  rxAudioBtn.disabled = true;
  txAudioBtn.disabled = true;
  audioStatus.textContent = "Audio requires Chrome/Edge";
}

function resetTxTimeout() {
  txTimeoutRemaining = TX_TIMEOUT_SECS;
  if (txTimeoutTimer) clearTimeout(txTimeoutTimer);
  txTimeoutTimer = setTimeout(() => {
    console.warn("PTT safety timeout — stopping TX");
    stopTxAudio();
  }, TX_TIMEOUT_SECS * 1000);
}

function startTxTimeoutCountdown() {
  txTimeoutRemaining = TX_TIMEOUT_SECS;
  if (txTimeoutInterval) clearInterval(txTimeoutInterval);
  txTimeoutInterval = setInterval(() => {
    txTimeoutRemaining--;
    if (txTimeoutRemaining <= 10 && txTimeoutRemaining > 0 && txActive) {
      audioStatus.textContent = `TX timeout ${txTimeoutRemaining}s`;
    }
  }, 1000);
}

function clearTxTimeout() {
  if (txTimeoutTimer) { clearTimeout(txTimeoutTimer); txTimeoutTimer = null; }
  if (txTimeoutInterval) { clearInterval(txTimeoutInterval); txTimeoutInterval = null; }
  txTimeoutRemaining = 0;
}

function startRxAudio() {
  if (rxActive) { stopRxAudio(); return; }
  if (!hasWebCodecs) {
    audioStatus.textContent = "Audio requires Chrome/Edge";
    return;
  }
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  audioWs = new WebSocket(`${proto}//${location.host}/audio`);
  audioWs.binaryType = "arraybuffer";
  audioStatus.textContent = "Connecting…";

  audioWs.onopen = () => {
    audioStatus.textContent = "Connected";
  };

  audioWs.onmessage = (evt) => {
    if (typeof evt.data === "string") {
      // Stream info JSON
      try {
        streamInfo = JSON.parse(evt.data);
        audioCtx = new AudioContext({ sampleRate: streamInfo.sample_rate || 48000 });
        rxGainNode = audioCtx.createGain();
        rxGainNode.gain.value = rxVolSlider.value / 100;
        rxGainNode.connect(audioCtx.destination);
        rxActive = true;
        rxAudioBtn.style.borderColor = "#00d17f";
        rxAudioBtn.style.color = "#00d17f";
        audioStatus.textContent = "RX";
      } catch (e) {
        console.error("Audio stream info parse error", e);
      }
      return;
    }

    // Binary Opus data — decode via WebCodecs AudioDecoder if available
    if (!audioCtx) return;
    const data = new Uint8Array(evt.data);

    // Throttle level indicator updates to max 10/sec
    const now = Date.now();
    if (now - lastLevelUpdate >= 100) {
      const level = Math.min(100, (data.length / 120) * 100);
      audioLevelFill.style.width = `${level}%`;
      lastLevelUpdate = now;
    }

    // Use WebCodecs AudioDecoder for Opus if available
    if (typeof AudioDecoder !== "undefined" && !opusDecoder) {
      try {
        const channels = (streamInfo && streamInfo.channels) || 1;
        const sampleRate = (streamInfo && streamInfo.sample_rate) || 48000;
        opusDecoder = new AudioDecoder({
          output: (frame) => {
            const buf = new Float32Array(frame.numberOfFrames * frame.numberOfChannels);
            frame.copyTo(buf, { planeIndex: 0 });
            const ab = audioCtx.createBuffer(frame.numberOfChannels, frame.numberOfFrames, frame.sampleRate);
            for (let ch = 0; ch < frame.numberOfChannels; ch++) {
              const chData = new Float32Array(frame.numberOfFrames);
              for (let i = 0; i < frame.numberOfFrames; i++) {
                chData[i] = buf[i * frame.numberOfChannels + ch];
              }
              ab.copyToChannel(chData, ch);
            }
            const src = audioCtx.createBufferSource();
            src.buffer = ab;
            src.connect(rxGainNode);
            const now = audioCtx.currentTime;
            const schedTime = Math.max(now, (nextPlayTime || now));
            src.start(schedTime);
            nextPlayTime = schedTime + ab.duration;
            frame.close();
          },
          error: (e) => { console.error("AudioDecoder error", e); }
        });
        opusDecoder.configure({
          codec: "opus",
          sampleRate: sampleRate,
          numberOfChannels: channels,
        });
      } catch (e) {
        console.warn("WebCodecs AudioDecoder not available for Opus", e);
        opusDecoder = null;
      }
    }
    if (opusDecoder) {
      try {
        opusDecoder.decode(new EncodedAudioChunk({
          type: "key",
          timestamp: performance.now() * 1000,
          data: data,
        }));
      } catch (e) {
        // Ignore decode errors for individual frames
      }
    }
  };

  audioWs.onclose = () => {
    // If TX was active when WS closed, release PTT
    if (txActive) { stopTxAudio(); }
    rxActive = false;
    rxAudioBtn.style.borderColor = "";
    rxAudioBtn.style.color = "";
    audioStatus.textContent = "Off";
    audioLevelFill.style.width = "0%";
    rxGainNode = null;
    if (opusDecoder) {
      try { opusDecoder.close(); } catch(e) {}
      opusDecoder = null;
    }
    nextPlayTime = 0;
  };

  audioWs.onerror = () => {
    audioStatus.textContent = "Error";
  };
}

function stopRxAudio() {
  rxActive = false;
  if (audioWs) { audioWs.close(); audioWs = null; }
  if (audioCtx) { audioCtx.close(); audioCtx = null; }
  rxGainNode = null;
  if (opusDecoder) {
    try { opusDecoder.close(); } catch(e) {}
    opusDecoder = null;
  }
  nextPlayTime = 0;
  rxAudioBtn.style.borderColor = "";
  rxAudioBtn.style.color = "";
  audioStatus.textContent = "Off";
  audioLevelFill.style.width = "0%";
}

function startTxAudio() {
  if (txActive) { stopTxAudio(); return; }
  if (!hasWebCodecs) {
    audioStatus.textContent = "Audio requires Chrome/Edge";
    return;
  }
  if (!audioWs || audioWs.readyState !== WebSocket.OPEN) {
    audioStatus.textContent = "RX first";
    return;
  }
  if (!streamInfo) return;

  navigator.mediaDevices.getUserMedia({
    audio: { sampleRate: streamInfo.sample_rate || 48000, channelCount: streamInfo.channels || 1 }
  }).then(async (stream) => {
    txStream = stream;
    txActive = true;
    txAudioBtn.style.borderColor = "#e55353";
    txAudioBtn.style.color = "#e55353";
    audioStatus.textContent = "RX+TX";

    // Start PTT safety timeout
    resetTxTimeout();
    startTxTimeoutCountdown();

    // Engage PTT automatically
    try { await postPath("/set_ptt?ptt=true"); } catch (e) { console.error("PTT on failed", e); }

    const sampleRate = streamInfo.sample_rate || 48000;
    const channels = streamInfo.channels || 1;
    const encoder = new AudioEncoder({
      output: (chunk) => {
        const buf = new ArrayBuffer(chunk.byteLength);
        chunk.copyTo(buf);
        if (audioWs && audioWs.readyState === WebSocket.OPEN) {
          audioWs.send(buf);
        }
      },
      error: (e) => { console.error("AudioEncoder error", e); }
    });
    encoder.configure({
      codec: "opus",
      sampleRate: sampleRate,
      numberOfChannels: channels,
      bitrate: (streamInfo.bitrate_bps || 24000),
    });
    txEncoder = encoder;

    // Use AudioWorklet or ScriptProcessor to feed encoder
    if (!audioCtx) audioCtx = new AudioContext({ sampleRate: sampleRate });
    const source = audioCtx.createMediaStreamSource(stream);
    const frameDuration = (streamInfo.frame_duration_ms || 20) / 1000;
    const frameSize = Math.floor(sampleRate * frameDuration);
    // Use ScriptProcessorNode (deprecated but widely supported)
    const processor = audioCtx.createScriptProcessor(frameSize, channels, channels);
    let tsCounter = 0;
    processor.onaudioprocess = (e) => {
      if (!txActive || !txEncoder) return;
      const input = e.inputBuffer;
      // Reset PTT safety timeout on each audio callback
      resetTxTimeout();
      // Use mono (channel 0) for f32-planar format
      const monoData = input.getChannelData(0);
      try {
        const frame = new AudioData({
          format: "f32-planar",
          sampleRate: input.sampleRate,
          numberOfFrames: input.length,
          numberOfChannels: 1,
          timestamp: tsCounter,
          data: monoData,
        });
        tsCounter += (input.length / input.sampleRate) * 1_000_000;
        txEncoder.encode(frame);
        frame.close();
      } catch (e) {
        // Ignore
      }
    };
    txGainNode = audioCtx.createGain();
    txGainNode.gain.value = txVolSlider.value / 100;
    source.connect(txGainNode);
    txGainNode.connect(processor);
    processor.connect(audioCtx.destination);
    txProcessor = { source, processor };
  }).catch((err) => {
    console.error("getUserMedia failed:", err);
    audioStatus.textContent = "Mic denied";
  });
}

async function stopTxAudio() {
  if (!txActive) return;
  txActive = false;
  clearTxTimeout();

  // Release PTT automatically
  try { await postPath("/set_ptt?ptt=false"); } catch (e) { console.error("PTT off failed", e); }

  if (txStream) {
    txStream.getTracks().forEach(t => t.stop());
    txStream = null;
  }
  if (txProcessor) {
    txProcessor.source.disconnect();
    txProcessor.processor.disconnect();
    txProcessor = null;
  }
  if (txEncoder) {
    try { txEncoder.close(); } catch(e) {}
    txEncoder = null;
  }
  txGainNode = null;
  txAudioBtn.style.borderColor = "";
  txAudioBtn.style.color = "";
  audioStatus.textContent = rxActive ? "RX" : "Off";
}

rxAudioBtn.addEventListener("click", startRxAudio);
txAudioBtn.addEventListener("click", startTxAudio);

const rxVolPct = document.getElementById("rx-vol-pct");
const txVolPct = document.getElementById("tx-vol-pct");

// Restore saved volumes
rxVolSlider.value = loadSetting("rxVol", 80);
txVolSlider.value = loadSetting("txVol", 80);
rxVolPct.textContent = `${rxVolSlider.value}%`;
txVolPct.textContent = `${txVolSlider.value}%`;

function updateVolSlider(slider, pctEl, gainNode) {
  pctEl.textContent = `${slider.value}%`;
  if (gainNode) gainNode.gain.value = slider.value / 100;
}

rxVolSlider.addEventListener("input", () => { updateVolSlider(rxVolSlider, rxVolPct, rxGainNode); saveSetting("rxVol", Number(rxVolSlider.value)); });
txVolSlider.addEventListener("input", () => { updateVolSlider(txVolSlider, txVolPct, txGainNode); saveSetting("txVol", Number(txVolSlider.value)); });

function volWheel(slider, pctEl, getGain, storageKey) {
  slider.addEventListener("wheel", (e) => {
    e.preventDefault();
    const step = e.deltaY < 0 ? 2 : -2;
    slider.value = Math.max(0, Math.min(100, Number(slider.value) + step));
    updateVolSlider(slider, pctEl, getGain());
    saveSetting(storageKey, Number(slider.value));
  }, { passive: false });
}
volWheel(rxVolSlider, rxVolPct, () => rxGainNode, "rxVol");
volWheel(txVolSlider, txVolPct, () => txGainNode, "txVol");

document.getElementById("copyright-year").textContent = new Date().getFullYear();

// --- Server-side decode SSE ---
let decodeSource = null;
let decodeConnected = false;
function updateDecodeStatus(text) {
  const aprs = document.getElementById("aprs-status");
  const cw = document.getElementById("cw-status");
  const ft8 = document.getElementById("ft8-status");
  if (aprs && aprs.textContent !== "Receiving") aprs.textContent = text;
  if (cw && cw.textContent !== "Receiving") cw.textContent = text;
  if (ft8 && ft8.textContent !== "Receiving") ft8.textContent = text;
}
function connectDecode() {
  if (decodeSource) { decodeSource.close(); }
  decodeSource = new EventSource("/decode");
  decodeSource.onopen = () => {
    decodeConnected = true;
    updateDecodeStatus("Connected, listening for packets");
  };
  decodeSource.onmessage = (evt) => {
    try {
      const msg = JSON.parse(evt.data);
      if (msg.type === "aprs" && window.onServerAprs) window.onServerAprs(msg);
      if (msg.type === "cw" && window.onServerCw) window.onServerCw(msg);
      if (msg.type === "ft8" && window.onServerFt8) window.onServerFt8(msg);
      if (msg.type === "wspr" && window.onServerWspr) window.onServerWspr(msg);
    } catch (e) {
      // ignore parse errors
    }
  };
  decodeSource.onerror = () => {
    // readyState CLOSED (2) = server rejected (404/error), CONNECTING (0) = temporary drop
    const wasClosed = decodeSource.readyState === 2;
    decodeSource.close();
    decodeConnected = false;
    if (wasClosed) {
      updateDecodeStatus("Decode not available (check client audio config)");
      setTimeout(connectDecode, 10000);
    } else {
      updateDecodeStatus("Decode disconnected, retrying…");
      setTimeout(connectDecode, 5000);
    }
  };
}
connectDecode();

// Release PTT on page unload to prevent stuck transmit
window.addEventListener("beforeunload", () => {
  if (txActive) {
    navigator.sendBeacon("/set_ptt?ptt=false", "");
  }
});

// ── Spectrum display ────────────────────────────────────────────────────────
const spectrumCanvas = document.getElementById("spectrum-canvas");
const spectrumFreqAxis = document.getElementById("spectrum-freq-axis");
let spectrumPollTimer = null;
let lastSpectrumData = null;

function startSpectrumPolling() {
  if (spectrumPollTimer !== null) return;
  spectrumPollTimer = setInterval(fetchSpectrum, 200);
  fetchSpectrum();
}

function stopSpectrumPolling() {
  if (spectrumPollTimer !== null) {
    clearInterval(spectrumPollTimer);
    spectrumPollTimer = null;
  }
  lastSpectrumData = null;
  clearSpectrumCanvas();
}

async function fetchSpectrum() {
  try {
    const resp = await fetch("/spectrum", { cache: "no-store" });
    if (resp.status === 204) {
      lastSpectrumData = null;
      clearSpectrumCanvas();
      return;
    }
    if (!resp.ok) return;
    const data = await resp.json();
    lastSpectrumData = data;
    drawSpectrum(data);
  } catch (_) {
    // ignore fetch errors (connection lost etc.)
  }
}

function clearSpectrumCanvas() {
  if (!spectrumCanvas) return;
  const ctx = spectrumCanvas.getContext("2d");
  const w = spectrumCanvas.width, h = spectrumCanvas.height;
  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = "#0a0f18";
  ctx.fillRect(0, 0, w, h);
}

function drawSpectrum(data) {
  if (!spectrumCanvas) return;
  const dpr = window.devicePixelRatio || 1;
  const cssW = spectrumCanvas.clientWidth || 600;
  const cssH = spectrumCanvas.clientHeight || 120;
  const W = Math.round(cssW * dpr);
  const H = Math.round(cssH * dpr);
  if (spectrumCanvas.width !== W || spectrumCanvas.height !== H) {
    spectrumCanvas.width = W;
    spectrumCanvas.height = H;
  }

  const ctx = spectrumCanvas.getContext("2d");
  // Background
  ctx.fillStyle = "#0a0f18";
  ctx.fillRect(0, 0, W, H);

  const bins = data.bins;
  const n = bins.length;
  if (!n) return;

  // dBFS range for display
  const DB_MIN = -80;
  const DB_MAX = 0;
  const dbRange = DB_MAX - DB_MIN;

  // Grid lines (horizontal dBFS)
  ctx.strokeStyle = "rgba(255,255,255,0.06)";
  ctx.lineWidth = 1;
  for (let db = DB_MIN; db <= DB_MAX; db += 20) {
    const y = Math.round(H * (1 - (db - DB_MIN) / dbRange));
    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(W, y); ctx.stroke();
  }

  // Spectrum line
  ctx.beginPath();
  ctx.strokeStyle = "#00e676";
  ctx.lineWidth = Math.max(1, dpr);
  for (let i = 0; i < n; i++) {
    const x = (i / (n - 1)) * W;
    const db = Math.max(DB_MIN, Math.min(DB_MAX, bins[i]));
    const y = H * (1 - (db - DB_MIN) / dbRange);
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  }
  ctx.stroke();

  // Fill under spectrum line
  ctx.lineTo(W, H); ctx.lineTo(0, H); ctx.closePath();
  ctx.fillStyle = "rgba(0,230,118,0.08)";
  ctx.fill();

  // Tuned-frequency marker
  if (lastFreqHz != null && data.center_hz && data.sample_rate) {
    const halfBw = data.sample_rate / 2;
    const loHz = data.center_hz - halfBw;
    const hiHz = data.center_hz + halfBw;
    const frac = (lastFreqHz - loHz) / (hiHz - loHz);
    if (frac >= 0 && frac <= 1) {
      const xf = Math.round(frac * W);
      ctx.save();
      ctx.setLineDash([4 * dpr, 4 * dpr]);
      ctx.strokeStyle = "#ff1744";
      ctx.lineWidth = Math.max(1, dpr);
      ctx.beginPath(); ctx.moveTo(xf, 0); ctx.lineTo(xf, H); ctx.stroke();
      ctx.restore();
    }
  }

  // Frequency axis labels
  updateSpectrumFreqAxis(data);
}

function updateSpectrumFreqAxis(data) {
  if (!spectrumFreqAxis || !data.center_hz || !data.sample_rate) return;
  const halfBw = data.sample_rate / 2;
  const loHz = data.center_hz - halfBw;
  const hiHz = data.center_hz + halfBw;

  // Choose label step: aim for ~5 labels
  const spanMHz = (hiHz - loHz) / 1e6;
  let stepMHz = 1;
  if (spanMHz <= 1) stepMHz = 0.1;
  else if (spanMHz <= 2) stepMHz = 0.2;
  else if (spanMHz <= 5) stepMHz = 0.5;
  else if (spanMHz <= 10) stepMHz = 1;
  else if (spanMHz <= 20) stepMHz = 2;
  else stepMHz = 5;

  const stepHz = stepMHz * 1e6;
  const firstHz = Math.ceil(loHz / stepHz) * stepHz;

  // Rebuild axis spans
  spectrumFreqAxis.innerHTML = "";
  for (let hz = firstHz; hz <= hiHz; hz += stepHz) {
    const frac = (hz - loHz) / (hiHz - loHz);
    const pct = (frac * 100).toFixed(2);
    const label = hz >= 1e6
      ? (hz / 1e6).toFixed(stepMHz < 1 ? 1 : 0) + " MHz"
      : (hz / 1e3).toFixed(0) + " kHz";
    const span = document.createElement("span");
    span.textContent = label;
    span.style.left = pct + "%";
    spectrumFreqAxis.appendChild(span);
  }
}

// Click on spectrum canvas → tune to that frequency
if (spectrumCanvas) {
  spectrumCanvas.addEventListener("click", (e) => {
    if (!lastSpectrumData || !lastSpectrumData.center_hz || !lastSpectrumData.sample_rate) return;
    const rect = spectrumCanvas.getBoundingClientRect();
    const frac = (e.clientX - rect.left) / rect.width;
    const halfBw = lastSpectrumData.sample_rate / 2;
    const loHz = lastSpectrumData.center_hz - halfBw;
    const hiHz = lastSpectrumData.center_hz + halfBw;
    const targetHz = Math.round(loHz + frac * (hiHz - loHz));
    setFreq(targetHz);
  });
}
