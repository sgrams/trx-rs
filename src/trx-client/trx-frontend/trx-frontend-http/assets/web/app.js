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
  const authGate = document.getElementById("auth-gate");
  authGate.style.display = "flex";
  authGate.style.flexDirection = "column";
  authGate.style.justifyContent = "center";
  authGate.style.alignItems = "stretch";
  const overviewStrip = document.querySelector(".overview-strip");
  if (overviewStrip) {
    overviewStrip.style.display = "none";
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

  document.querySelectorAll(".tab-bar .tab").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.tab === "main");
  });
  syncTopBarAccess();
}

function hideAuthGate() {
  const authGate = document.getElementById("auth-gate");
  authGate.style.display = "none";
  document.getElementById("loading").style.display = "block";
  const overviewStrip = document.querySelector(".overview-strip");
  if (overviewStrip) {
    overviewStrip.style.display = "";
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
  syncTopBarAccess();
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
    syncTopBarAccess();
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
  syncTopBarAccess();
}

function applyAuthRestrictions() {
  if (!authRole) return;

  // Disable TX/PTT/frequency/mode/VFO controls for rx role
  if (authRole === "rx") {
    const pttBtn = document.getElementById("ptt-btn");
    const powerBtn = document.getElementById("power-btn");
    const lockBtn = document.getElementById("lock-btn");
    const freqInput = document.getElementById("freq");
    const centerFreqInput = document.getElementById("center-freq");
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
    if (centerFreqInput) centerFreqInput.disabled = true;
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
  lastHasTx = !!caps.tx;

  // PTT / TX controls
  const pttBtn = document.getElementById("ptt-btn");
  const txPowerCol = document.getElementById("tx-power-col");
  const txMetersRow = document.getElementById("tx-meters");
  const txAudioBtn = document.getElementById("tx-audio-btn");
  const txVolSlider = document.getElementById("tx-vol");
  const txVolControl = txVolSlider ? txVolSlider.closest(".vol-label") : null;
  if (txPowerCol) txPowerCol.style.display = caps.tx ? "" : "none";
  if (pttBtn) pttBtn.style.display = caps.tx ? "" : "none";
  if (txMetersRow) txMetersRow.style.display = caps.tx ? "" : "none";
  if (txAudioBtn) txAudioBtn.style.display = caps.tx ? "" : "none";
  if (txVolControl) txVolControl.style.display = caps.tx ? "" : "none";
  if (!caps.tx && typeof stopTxAudio === "function" && txActive) {
    stopTxAudio();
  }

  // TX limit row
  const txLimitRow = document.getElementById("tx-limit-row");
  if (txLimitRow && !caps.tx_limit) txLimitRow.style.display = "none";

  // VFO row
  const vfoRow = document.getElementById("vfo-row");
  if (vfoRow) vfoRow.style.display = caps.vfo_switch ? "" : "none";

  // Signal meter row
  document.querySelectorAll(".full-row.label-below-row").forEach(row => {
    const label = row.querySelector(".label span");
    if (label && label.textContent === "Signal") {
      row.style.display = (caps.signal_meter && !caps.filter_controls) ? "" : "none";
    }
  });

  // Spectrum panel (SDR-only)
  const spectrumPanel = document.getElementById("spectrum-panel");
  const centerFreqField = document.getElementById("center-freq-field");
  if (spectrumPanel) {
    if (caps.filter_controls) {
      spectrumPanel.style.display = "";
      if (centerFreqField) centerFreqField.style.display = "";
      startSpectrumStreaming();
    } else {
      spectrumPanel.style.display = "none";
      if (centerFreqField) centerFreqField.style.display = "none";
      stopSpectrumStreaming();
    }
  }
}

const freqEl = document.getElementById("freq");
const centerFreqEl = document.getElementById("center-freq");
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
const overviewCanvas = document.getElementById("overview-canvas");
const overviewPeakHoldEl = document.getElementById("overview-peak-hold");
const themeToggleBtn = document.getElementById("theme-toggle");
const headerRigSwitchSelect = document.getElementById("header-rig-switch-select");
const headerStylePickSelect = document.getElementById("header-style-pick-select");
const rdsPsOverlay = document.getElementById("rds-ps-overlay");
let overviewPeakHoldMs = Number(loadSetting("overviewPeakHoldMs", 2000));

function syncTopBarAccess() {
  const loggedOut = authEnabled && !authRole;
  const tabBar = document.getElementById("tab-bar");
  const rigSwitch = document.querySelector(".header-rig-switch");
  if (tabBar) tabBar.style.display = "";

  document.querySelectorAll(".tab-bar .tab").forEach((btn) => {
    const isMain = btn.dataset.tab === "main";
    btn.style.display = !loggedOut || isMain ? "" : "none";
    btn.disabled = false;
  });

  if (rigSwitch) {
    rigSwitch.style.display = loggedOut ? "none" : "";
  }

  if (headerRigSwitchSelect) {
    headerRigSwitchSelect.disabled = loggedOut || authRole === "rx" || lastRigIds.length === 0;
  }
}

let overviewDrawPending = false;
let lastSpectrumData = null;
let rdsFrameCount = 0;
let lastControl;
let lastTxEn = null;
let lastHasTx = true;
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
let centerFreqDirty = false;
let jogUnit = loadSetting("jogUnit", 1000);   // base unit: 1, 1000, 1000000
let jogMult = loadSetting("jogMult", 1);      // divisor: 1, 10, 100
let jogStep = Math.max(Math.round(jogUnit / jogMult), 1);
let minFreqStepHz = 1;
let lastModeName = "";
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
let lastActiveRigId = null;
const originalTitle = document.title;
const savedTheme = loadSetting("theme", null);

function currentTheme() {
  return document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark";
}

function updateDocumentTitle(rds = null) {
  if (!Number.isFinite(lastFreqHz)) {
    document.title = originalTitle;
    return;
  }
  const parts = [formatFreq(lastFreqHz)];
  const ps = rds?.program_service;
  if (ps && ps.length > 0) {
    parts.push(ps);
  }
  parts.push(originalTitle);
  document.title = parts.join(" - ");
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

// ── Style / palette system ────────────────────────────────────────────────────
const CANVAS_PALETTE = {
  original: {
    dark: {
      bg: "#0a0f18",
      spectrumLine: "#00e676", spectrumFill: "rgba(0,230,118,0.10)",
      spectrumGrid: "rgba(255,255,255,0.06)", spectrumLabel: "rgba(180,200,220,0.45)",
      waveformLine: "rgba(94,234,212,0.92)", waveformPeak: "rgba(251,191,36,0.88)",
      waveformGrid: "rgba(148,163,184,0.12)", waveformLabel: "rgba(203,213,225,0.72)",
      waterfallHue: [225, 30], waterfallSat: 88, waterfallLight: [16, 68], waterfallAlpha: [0.28, 0.86],
    },
    light: {
      bg: "#eef3fb",
      spectrumLine: "#007a47", spectrumFill: "rgba(0,110,70,0.12)",
      spectrumGrid: "rgba(0,30,80,0.10)", spectrumLabel: "rgba(30,50,90,0.55)",
      waveformLine: "rgba(17,94,89,0.95)", waveformPeak: "rgba(217,119,6,0.9)",
      waveformGrid: "rgba(71,85,105,0.14)", waveformLabel: "rgba(51,65,85,0.72)",
      waterfallHue: [210, 35], waterfallSat: 82, waterfallLight: [92, 40], waterfallAlpha: [0.42, 0.80],
    },
  },
  arctic: {
    dark: {
      bg: "#1e2530",
      spectrumLine: "#88c0d0", spectrumFill: "rgba(136,192,208,0.12)",
      spectrumGrid: "rgba(216,222,233,0.08)", spectrumLabel: "rgba(216,222,233,0.55)",
      waveformLine: "rgba(136,192,208,0.92)", waveformPeak: "rgba(235,203,139,0.88)",
      waveformGrid: "rgba(216,222,233,0.10)", waveformLabel: "rgba(216,222,233,0.65)",
      waterfallHue: [212, 188], waterfallSat: 70, waterfallLight: [14, 58], waterfallAlpha: [0.28, 0.82],
    },
    light: {
      bg: "#dde1e9",
      spectrumLine: "#5e81ac", spectrumFill: "rgba(94,129,172,0.14)",
      spectrumGrid: "rgba(46,52,64,0.08)", spectrumLabel: "rgba(46,52,64,0.55)",
      waveformLine: "rgba(94,129,172,0.95)", waveformPeak: "rgba(208,135,112,0.9)",
      waveformGrid: "rgba(46,52,64,0.12)", waveformLabel: "rgba(46,52,64,0.65)",
      waterfallHue: [215, 195], waterfallSat: 65, waterfallLight: [88, 45], waterfallAlpha: [0.35, 0.78],
    },
  },
  lime: {
    dark: {
      bg: "#181815",
      spectrumLine: "#a6e22e", spectrumFill: "rgba(166,226,46,0.10)",
      spectrumGrid: "rgba(248,248,242,0.05)", spectrumLabel: "rgba(248,248,242,0.45)",
      waveformLine: "rgba(166,226,46,0.92)", waveformPeak: "rgba(230,219,116,0.88)",
      waveformGrid: "rgba(248,248,242,0.08)", waveformLabel: "rgba(248,248,242,0.65)",
      waterfallHue: [70, 38], waterfallSat: 80, waterfallLight: [12, 62], waterfallAlpha: [0.25, 0.88],
    },
    light: {
      bg: "#ede8d8",
      spectrumLine: "#5f8700", spectrumFill: "rgba(95,135,0,0.12)",
      spectrumGrid: "rgba(39,40,34,0.08)", spectrumLabel: "rgba(39,40,34,0.50)",
      waveformLine: "rgba(95,135,0,0.95)", waveformPeak: "rgba(176,120,0,0.9)",
      waveformGrid: "rgba(39,40,34,0.10)", waveformLabel: "rgba(39,40,34,0.60)",
      waterfallHue: [75, 42], waterfallSat: 75, waterfallLight: [90, 42], waterfallAlpha: [0.35, 0.78],
    },
  },
  contrast: {
    dark: {
      bg: "#000000",
      spectrumLine: "#00ff88", spectrumFill: "rgba(0,255,136,0.12)",
      spectrumGrid: "rgba(255,255,255,0.12)", spectrumLabel: "rgba(255,255,255,0.70)",
      waveformLine: "rgba(0,255,136,0.95)", waveformPeak: "rgba(255,204,0,0.92)",
      waveformGrid: "rgba(255,255,255,0.15)", waveformLabel: "rgba(255,255,255,0.80)",
      waterfallHue: [150, 60], waterfallSat: 100, waterfallLight: [8, 55], waterfallAlpha: [0.30, 0.95],
    },
    light: {
      bg: "#f4f4f4",
      spectrumLine: "#005cc5", spectrumFill: "rgba(0,92,197,0.12)",
      spectrumGrid: "rgba(0,0,0,0.12)", spectrumLabel: "rgba(0,0,0,0.65)",
      waveformLine: "rgba(0,92,197,0.95)", waveformPeak: "rgba(180,60,0,0.9)",
      waveformGrid: "rgba(0,0,0,0.14)", waveformLabel: "rgba(0,0,0,0.70)",
      waterfallHue: [220, 180], waterfallSat: 100, waterfallLight: [90, 42], waterfallAlpha: [0.35, 0.82],
    },
  },
};

function currentStyle() {
  return document.documentElement.getAttribute("data-style") || "original";
}

function canvasPalette() {
  const s = currentStyle();
  const t = currentTheme();
  return (CANVAS_PALETTE[s] ?? CANVAS_PALETTE.original)[t];
}

function setStyle(style) {
  const remapped = style === "nord" ? "arctic" : style === "monokai" ? "lime" : style;
  const valid = ["original", "arctic", "lime", "contrast"];
  const next = valid.includes(remapped) ? remapped : "original";
  if (next === "original") {
    document.documentElement.removeAttribute("data-style");
  } else {
    document.documentElement.setAttribute("data-style", next);
  }
  saveSetting("style", next);
  if (headerStylePickSelect) headerStylePickSelect.value = next;
  scheduleOverviewDraw();
  if (typeof scheduleSpectrumDraw === "function" && lastSpectrumData) scheduleSpectrumDraw();
}

if (overviewPeakHoldEl) {
  if (!Number.isFinite(overviewPeakHoldMs) || overviewPeakHoldMs <= 0) {
    overviewPeakHoldMs = 2000;
  }
  overviewPeakHoldEl.value = String(overviewPeakHoldMs);
  overviewPeakHoldEl.addEventListener("change", () => {
    overviewPeakHoldMs = Math.max(0, Number(overviewPeakHoldEl.value) || 0);
    saveSetting("overviewPeakHoldMs", overviewPeakHoldMs);
    scheduleOverviewDraw();
  });
}

if (savedTheme === "light" || savedTheme === "dark") {
  setTheme(savedTheme);
} else {
  const prefersLight = window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches;
  setTheme(prefersLight ? "light" : "dark");
}

const savedStyle = loadSetting("style", "original");
setStyle(savedStyle);

if (themeToggleBtn) {
  themeToggleBtn.addEventListener("click", () => {
    setTheme(currentTheme() === "dark" ? "light" : "dark");
    updateMapBaseLayerForTheme(currentTheme());
    scheduleOverviewDraw();
    if (typeof scheduleSpectrumDraw === "function" && lastSpectrumData) scheduleSpectrumDraw();
  });
}

if (headerStylePickSelect) {
  headerStylePickSelect.addEventListener("change", () => {
    setStyle(headerStylePickSelect.value);
    updateMapBaseLayerForTheme(currentTheme());
  });
}

function readyText() {
  return lastClientCount !== null ? `Ready \u00b7 ${lastClientCount} user${lastClientCount !== 1 ? "s" : ""}` : "Ready";
}

function rigBadgeColor(rigId) {
  const text = (rigId || "rx").toString();
  let hash = 0;
  for (let i = 0; i < text.length; i++) {
    hash = ((hash * 33) + text.charCodeAt(i)) >>> 0;
  }
  const hue = hash % 360;
  return `hsl(${hue}, 62%, 52%)`;
}

window.getDecodeRigMeta = function() {
  const rigId = lastActiveRigId || "local";
  return {
    rigId,
    label: lastRigDisplayNames[rigId] || rigId,
    color: rigBadgeColor(rigId),
  };
};

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
    lastActiveRigId = activeRigId;
    const aboutActive = document.getElementById("about-active-rig");
    if (aboutActive) aboutActive.textContent = activeRigId;
  }
  const disableSwitch = lastRigIds.length === 0 || !authRole || authRole === "rx";
  populateRigPicker(headerRigSwitchSelect, lastRigIds, activeRigId, disableSwitch);
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
let overviewSignalSamples = [];
let overviewSignalTimer = null;
let overviewWaterfallRows = [];
let overviewWaterfallPushCount = 0;   // monotonically increments on every push
const HEADER_SIG_WINDOW_MS = 10_000;

// Offscreen waterfall cache — reused across frames to avoid full redraws
let _wfOC = null;           // OffscreenCanvas
let _wfOCPalKey = "";       // palette signature when offscreen was last built
let _wfOCPushCount = 0;     // overviewWaterfallPushCount when offscreen was last updated

function _wfResetOffscreen() { _wfOC = null; _wfOCPushCount = 0; _wfOCPalKey = ""; }
function _wfPalKey(pal) {
  return `${pal.waterfallHue}|${pal.waterfallSat}|${pal.waterfallLight}|${pal.waterfallAlpha}`;
}

function resizeHeaderSignalCanvas() {
  if (!overviewCanvas) return;
  const cssW = Math.floor(overviewCanvas.clientWidth);
  const cssH = Math.floor(overviewCanvas.clientHeight);
  if (cssW <= 0 || cssH <= 0) return;
  const dpr = window.devicePixelRatio || 1;
  const nextW = Math.floor(cssW * dpr);
  const nextH = Math.floor(cssH * dpr);
  if (overviewCanvas.width !== nextW || overviewCanvas.height !== nextH) {
    overviewCanvas.width = nextW;
    overviewCanvas.height = nextH;
    _wfResetOffscreen();
    trimOverviewWaterfallRows();
  }
  positionRdsPsOverlay();
  drawHeaderSignalGraph();
}

function scheduleOverviewDraw() {
  if (!overviewCanvas || overviewDrawPending) return;
  overviewDrawPending = true;
  requestAnimationFrame(() => {
    overviewDrawPending = false;
    drawHeaderSignalGraph();
  });
}

function pushHeaderSignalSample(sUnits) {
  if (!overviewCanvas) return;
  const now = Date.now();
  const sample = Number.isFinite(sUnits) ? Math.max(0, Math.min(20, sUnits)) : 0;
  overviewSignalSamples.push({ t: now, v: sample });
  while (overviewSignalSamples.length && now - overviewSignalSamples[0].t > HEADER_SIG_WINDOW_MS) {
    overviewSignalSamples.shift();
  }
  scheduleOverviewDraw();
}

function trimOverviewWaterfallRows() {
  if (!overviewCanvas) return;
  const dpr = window.devicePixelRatio || 1;
  const maxRows = Math.max(1, Math.floor(overviewCanvas.height / dpr));
  while (overviewWaterfallRows.length > maxRows) {
    overviewWaterfallRows.shift();
  }
}

function overviewVisibleBinWindow(data, binCount) {
  if (!data || !Number.isFinite(data.sample_rate) || binCount <= 1) {
    return { startIdx: 0, endIdx: Math.max(0, binCount - 1) };
  }
  const range = spectrumVisibleRange(data);
  const fullLoHz = data.center_hz - data.sample_rate / 2;
  const startFrac = (range.visLoHz - fullLoHz) / data.sample_rate;
  const endFrac = (range.visHiHz - fullLoHz) / data.sample_rate;
  const maxIdx = binCount - 1;
  const startIdx = Math.max(0, Math.min(maxIdx, Math.floor(startFrac * maxIdx)));
  const endIdx = Math.max(startIdx, Math.min(maxIdx, Math.ceil(endFrac * maxIdx)));
  return { startIdx, endIdx };
}

function pushOverviewWaterfallFrame(data) {
  if (!overviewCanvas || !data || !Array.isArray(data.bins) || data.bins.length === 0) return;
  overviewWaterfallRows.push(data.bins.slice());
  overviewWaterfallPushCount++;
  trimOverviewWaterfallRows();
  scheduleOverviewDraw();
}

function startHeaderSignalSampling() {
  if (!overviewCanvas || overviewSignalTimer) return;
  overviewSignalTimer = setInterval(() => {
    pushHeaderSignalSample(Number.isFinite(sigLastSUnits) ? sigLastSUnits : 0);
  }, 120);
}

function drawHeaderSignalGraph() {
  if (!overviewCanvas) return;
  const ctx = overviewCanvas.getContext("2d");
  if (!ctx) return;
  const pal = canvasPalette();
  const dpr = window.devicePixelRatio || 1;
  const w = overviewCanvas.width / dpr;
  const h = overviewCanvas.height / dpr;
  if (w <= 0 || h <= 0) return;

  ctx.save();
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);
  if (lastSpectrumData && overviewWaterfallRows.length > 0) {
    drawOverviewWaterfall(ctx, w, h, pal);
  } else {
    drawOverviewSignalHistory(ctx, w, h, pal);
  }
  ctx.restore();
  positionRdsPsOverlay();
}

function _wfDrawRows(oct, rows, startRowIdx, endRowIdx, iW, iH, pal) {
  // Draw rows[startRowIdx..endRowIdx) into oct, positioned at the canvas bottom.
  // rowH is computed relative to the total row count (all of `rows`).
  const total = rows.length;
  const rowH = iH / total;
  const columnStep = Math.max(1, Math.ceil(iW / 320));
  for (let ri = startRowIdx; ri < endRowIdx; ri++) {
    const bins = rows[ri];
    if (!Array.isArray(bins) || bins.length === 0) continue;
    const { startIdx, endIdx } = overviewVisibleBinWindow(lastSpectrumData, bins.length);
    const spanBins = Math.max(1, endIdx - startIdx);
    const y = iH - (total - ri) * rowH;
    for (let x = 0; x < iW; x += columnStep) {
      const frac = x / Math.max(1, iW - 1);
      const binIdx = Math.min(endIdx, startIdx + Math.floor(frac * spanBins));
      oct.fillStyle = waterfallColor(bins[binIdx], pal);
      oct.fillRect(x, y, columnStep + 0.75, rowH + 1);
    }
  }
}

function drawOverviewWaterfall(ctx, w, h, pal) {
  const maxVisible = Math.max(1, Math.floor(h));
  const rows = overviewWaterfallRows.slice(-maxVisible);
  if (rows.length === 0) return;

  const iW = Math.ceil(w);
  const iH = Math.ceil(h);
  const palKey = _wfPalKey(pal);
  const steadyState = rows.length >= maxVisible;
  // How many rows were pushed since the offscreen was last updated
  const newPushes = overviewWaterfallPushCount - _wfOCPushCount;

  // Detect conditions that require a full redraw
  const sizeChanged = !_wfOC || _wfOC.width !== iW || _wfOC.height !== iH;
  const palChanged  = _wfOCPalKey !== palKey;
  const needsFull   = sizeChanged || palChanged || _wfOCPushCount === 0;

  if (sizeChanged || !_wfOC) {
    _wfOC = new OffscreenCanvas(iW, iH);
    _wfOCPushCount = 0;
  }
  const oct = _wfOC.getContext("2d");

  if (needsFull) {
    oct.clearRect(0, 0, iW, iH);
    _wfDrawRows(oct, rows, 0, rows.length, iW, iH, pal);
    _wfOCPushCount = overviewWaterfallPushCount;
    _wfOCPalKey    = palKey;
  } else if (steadyState && newPushes > 0) {
    // Steady state: scroll up and paint only the new rows at the bottom.
    // newPushes new rows are at the tail of `rows`; each replaces one old row.
    const newCount = Math.min(newPushes, rows.length);
    const rowH     = iH / rows.length;
    const scrollPx = Math.round(newCount * rowH);
    if (scrollPx > 0 && scrollPx < iH) {
      const img = oct.getImageData(0, scrollPx, iW, iH - scrollPx);
      oct.putImageData(img, 0, 0);
      oct.clearRect(0, iH - scrollPx, iW, scrollPx);
    }
    _wfDrawRows(oct, rows, rows.length - newCount, rows.length, iW, iH, pal);
    _wfOCPushCount = overviewWaterfallPushCount;
  }

  ctx.drawImage(_wfOC, 0, 0, w, h);
}

function drawOverviewSignalHistory(ctx, w, h, pal) {
  const now = Date.now();
  const samples = overviewSignalSamples.filter((sample) => now - sample.t <= HEADER_SIG_WINDOW_MS);
  if (samples.length === 0) return;

  const maxVal = 20;
  const windowStart = now - HEADER_SIG_WINDOW_MS;
  const toX = (t) => ((t - windowStart) / HEADER_SIG_WINDOW_MS) * w;
  const toY = (v) => h - (Math.max(0, Math.min(maxVal, v)) / maxVal) * (h - 3) - 1.5;

  const gridMarkers = [
    { val: 0, label: "S0" },
    { val: 9, label: "S9" },
    { val: 18, label: "S9+" },
  ];
  ctx.strokeStyle = pal.waveformGrid;
  ctx.lineWidth = 1;
  ctx.font = "11px sans-serif";
  ctx.fillStyle = pal.waveformLabel;
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  for (const marker of gridMarkers) {
    const y = toY(marker.val);
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
    ctx.fillText(marker.label, w - 6, Math.max(8, Math.min(h - 8, y + 6)));
  }

  ctx.beginPath();
  samples.forEach((sample, idx) => {
    const x = toX(sample.t);
    const y = toY(sample.v);
    if (idx === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });
  ctx.strokeStyle = pal.waveformLine;
  ctx.lineWidth = 1.6;
  ctx.stroke();

  const holdMs = Math.max(0, Number.isFinite(overviewPeakHoldMs) ? overviewPeakHoldMs : 0);
  if (holdMs > 0) {
    ctx.beginPath();
    for (let i = 0; i < samples.length; i++) {
      let peak = samples[i].v;
      for (let j = i; j >= 0; j--) {
        if (samples[i].t - samples[j].t > holdMs) break;
        if (samples[j].v > peak) peak = samples[j].v;
      }
      const x = toX(samples[i].t);
      const y = toY(peak);
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.strokeStyle = pal.waveformPeak;
    ctx.lineWidth = 1;
    ctx.stroke();
  }
}

function waterfallColor(db, pal) {
  const clamped = Math.max(-120, Math.min(-10, Number.isFinite(db) ? db : -120));
  const t = (clamped + 120) / 110;
  const hue = pal.waterfallHue[0] + t * (pal.waterfallHue[1] - pal.waterfallHue[0]);
  const light = pal.waterfallLight[0] + t * (pal.waterfallLight[1] - pal.waterfallLight[0]);
  const alpha = pal.waterfallAlpha[0] + t * (pal.waterfallAlpha[1] - pal.waterfallAlpha[0]);
  return `hsla(${hue}, ${pal.waterfallSat}%, ${light}%, ${alpha})`;
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
  freqEl.value = formatFreqForStep(lastFreqHz, jogUnit);
  refreshWavelengthDisplay(lastFreqHz);
}

function positionRdsPsOverlay() {
  if (!rdsPsOverlay || !lastSpectrumData || lastFreqHz == null || !overviewCanvas) return;
  const width = overviewCanvas.clientWidth || overviewCanvas.width || 0;
  if (width <= 0) {
    return;
  }
  const range = spectrumVisibleRange(lastSpectrumData);
  if (!Number.isFinite(range.visLoHz) || !Number.isFinite(range.visSpanHz) || range.visSpanHz <= 0) {
    return;
  }
  const rel = (lastFreqHz - range.visLoHz) / range.visSpanHz;
  const clamped = Math.max(0.06, Math.min(0.94, rel));
  rdsPsOverlay.style.left = `${clamped * width}px`;
}

function resetRdsDisplay() {
  rdsFrameCount = 0;
  updateRdsPsOverlay(null);
}

function resetWfmStereoIndicator() {
  if (!wfmStFlagEl) return;
  wfmStFlagEl.textContent = "MO";
  wfmStFlagEl.classList.remove("wfm-st-flag-stereo");
  wfmStFlagEl.classList.add("wfm-st-flag-mono");
}

function applyLocalTunedFrequency(hz, forceDisplay = false) {
  if (!Number.isFinite(hz)) return;
  const freqChanged = lastFreqHz !== hz;
  if (freqChanged) {
    resetRdsDisplay();
    resetWfmStereoIndicator();
  }
  lastFreqHz = hz;
  updateDocumentTitle(lastSpectrumData?.rds ?? null);
  refreshWavelengthDisplay(lastFreqHz);
  if (forceDisplay) {
    freqDirty = false;
  }
  if (forceDisplay || !freqDirty) {
    refreshFreqDisplay();
  }
  window.ft8BaseHz = lastFreqHz;
  if (window.updateFt8RfDisplay) {
    window.updateFt8RfDisplay();
  }
  if (lastSpectrumData) {
    scheduleSpectrumDraw();
  }
  positionRdsPsOverlay();
}

function refreshCenterFreqDisplay() {
  if (!centerFreqEl || !lastSpectrumData || centerFreqDirty) return;
  centerFreqEl.value = formatFreqForStep(lastSpectrumData.center_hz, jogUnit);
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

  const current = Number(jogUnit);
  const desired =
    Number.isFinite(current) && current >= minFreqStepHz ? current : Math.max(steps[0], minFreqStepHz);

  jogUnit = steps.reduce((best, s) => (Math.abs(s - desired) < Math.abs(best - desired) ? s : best), steps[0]);
  jogStep = Math.max(Math.round(jogUnit / jogMult), minFreqStepHz);
  saveSetting("jogUnit", jogUnit);
  saveSetting("jogStep", jogStep);

  buttons.forEach((btn) => {
    btn.classList.toggle("active", Number(btn.dataset.step) === jogUnit);
  });

  refreshFreqDisplay();
  refreshCenterFreqDisplay();
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
  [freqEl, centerFreqEl, modeEl, pttBtn, powerBtn, txLimitInput, txLimitBtn, lockBtn].forEach((el) => {
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
  const titleEl = document.getElementById("rig-title");
  if (titleEl) {
    titleEl.textContent = serverVersion ? `trx-rs v${serverVersion}` : "trx-rs";
  }
  updateDocumentTitle(lastSpectrumData?.rds ?? null);
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
    const fallbackRigName = originalTitle || "Rig";
    const manu = (update.info && update.info.manufacturer) || fallbackRigName;
    const model = (update.info && update.info.model) || fallbackRigName;
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
  if (update.filter && typeof update.filter.bandwidth_hz === "number") {
    currentBandwidthHz = update.filter.bandwidth_hz;
    syncBandwidthInput(currentBandwidthHz);
    if (
      sdrGainEl
      && typeof update.filter.sdr_gain_db === "number"
      && document.activeElement !== sdrGainEl
    ) {
      sdrGainEl.value = String(Math.round(update.filter.sdr_gain_db));
    }
    if (wfmDeemphasisEl && typeof update.filter.wfm_deemphasis_us === "number") {
      wfmDeemphasisEl.value = String(update.filter.wfm_deemphasis_us);
    }
    if (wfmAudioModeEl && typeof update.filter.wfm_stereo === "boolean") {
      const nextMode = update.filter.wfm_stereo ? "stereo" : "mono";
      if (wfmAudioModeEl.value !== nextMode) {
        wfmAudioModeEl.value = nextMode;
        saveSetting("wfmAudioMode", nextMode);
      }
    }
    if (wfmStFlagEl && typeof update.filter.wfm_stereo_detected === "boolean") {
      const detected = update.filter.wfm_stereo_detected;
      wfmStFlagEl.textContent = detected ? "ST" : "MO";
      wfmStFlagEl.classList.toggle("wfm-st-flag-stereo", detected);
      wfmStFlagEl.classList.toggle("wfm-st-flag-mono", !detected);
    }
  }
  if (update.status && update.status.freq && typeof update.status.freq.hz === "number") {
    applyLocalTunedFrequency(update.status.freq.hz, true);
  }
  if (update.status && update.status.mode) {
    const mode = normalizeMode(update.status.mode);
    const modeUpper = mode ? mode.toUpperCase() : "";
    modeEl.value = modeUpper;
    if (modeUpper === "WFM" && lastModeName !== "WFM") {
      setJogDivisor(10);
    }
    lastModeName = modeUpper;
    updateWfmControls();
    // When filter panel is active (SDR backend), update the BW slider range
    // to match the new mode — but only if the server hasn't already sent a
    // filter state that overrides it.
    // When SDR backend is active (spectrum visible), apply BW default for new
    // mode — but only if the server hasn't already pushed a filter_state.
    if (lastSpectrumData && !update.filter) {
      applyBwDefaultForMode(mode, false);
    }
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
  txMeters.style.display = lastHasTx ? "" : "none";
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
  stopSpectrumStreaming();
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
  selectEl.disabled = true;
  showHint("Switching rig…");
  try {
    await postPath(`/select_rig?rig_id=${encodeURIComponent(selectEl.value)}`);
    refreshRigList();
    showHint("Rig switch requested", 1500);
  } catch (err) {
    showHint("Rig switch failed", 2000);
    console.error(err);
  } finally {
    const disableSwitch = lastRigIds.length === 0 || !authRole || authRole === "rx";
    selectEl.disabled = disableSwitch;
  }
}

if (headerRigSwitchSelect) {
  headerRigSwitchSelect.addEventListener("change", () => { switchRigFromSelect(headerRigSwitchSelect); });
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
  const parsedRaw = parseFreqInput(freqEl.value, jogUnit);
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
    applyLocalTunedFrequency(parsed);
    showHint("Freq set", 1500);
  } catch (err) {
    showHint("Set freq failed", 2000);
    console.error(err);
  } finally {
    freqEl.disabled = false;
  }
}

async function applyCenterFreqFromInput() {
  if (!centerFreqEl) return;
  const parsedRaw = parseFreqInput(centerFreqEl.value, jogUnit);
  const parsed = alignFreqToRigStep(parsedRaw);
  if (parsed === null) {
    showHint("Central freq missing", 1500);
    return;
  }
  if (!freqAllowed(parsed)) {
    showHint("Out of supported bands", 1500);
    return;
  }
  centerFreqDirty = false;
  centerFreqEl.disabled = true;
  showHint("Setting central frequency…");
  try {
    await postPath(`/set_center_freq?hz=${parsed}`);
    showHint("Central freq set", 1500);
  } catch (err) {
    showHint("Set central freq failed", 2000);
    console.error(err);
  } finally {
    centerFreqEl.disabled = false;
  }
}

freqEl.addEventListener("keydown", (e) => {
  freqDirty = true;
  if (e.key === "Enter") {
    e.preventDefault();
    applyFreqFromInput();
  }
});
if (centerFreqEl) {
  centerFreqEl.addEventListener("keydown", (e) => {
    centerFreqDirty = true;
    if (e.key === "Enter") {
      e.preventDefault();
      applyCenterFreqFromInput();
    }
  });
}
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
const jogMultEl = document.getElementById("jog-mult");
const VALID_JOG_DIVISORS = new Set([1, 10]);

function applyJogStep() {
  jogStep = Math.max(Math.round(jogUnit / jogMult), minFreqStepHz);
  saveSetting("jogUnit", jogUnit);
  saveSetting("jogMult", jogMult);
  saveSetting("jogStep", jogStep);
  refreshFreqDisplay();
  refreshCenterFreqDisplay();
}

function setJogDivisor(divisor) {
  const next = VALID_JOG_DIVISORS.has(divisor) ? divisor : 1;
  jogMult = next;
  if (jogMultEl) {
    jogMultEl.querySelectorAll("button[data-mult]").forEach((b) => {
      b.classList.toggle("active", parseInt(b.dataset.mult, 10) === jogMult);
    });
  }
  applyJogStep();
}

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
    applyLocalTunedFrequency(newHz);
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

// Step unit selector
jogStepEl.addEventListener("click", (e) => {
  const btn = e.target.closest("button[data-step]");
  if (!btn) return;
  jogUnit = parseInt(btn.dataset.step, 10);
  jogStepEl.querySelectorAll("button").forEach((b) => b.classList.remove("active"));
  btn.classList.add("active");
  applyJogStep();
});

// Step multiplier selector
if (jogMultEl) {
  jogMultEl.querySelectorAll("button[data-mult]").forEach((btn) => {
    const divisor = parseInt(btn.dataset.mult, 10);
    if (!VALID_JOG_DIVISORS.has(divisor)) {
      btn.remove();
    }
  });
  jogMultEl.addEventListener("click", (e) => {
    const btn = e.target.closest("button[data-mult]");
    if (!btn) return;
    setJogDivisor(parseInt(btn.dataset.mult, 10));
  });
}

// Restore active jog step buttons from saved settings
{
  const unitBtns = Array.from(jogStepEl.querySelectorAll("button[data-step]"));
  const activeUnit =
    unitBtns.find((b) => parseInt(b.dataset.step, 10) === jogUnit) ||
    unitBtns.find((b) => parseInt(b.dataset.step, 10) === 1000) ||
    unitBtns[0];
  if (activeUnit) {
    jogUnit = parseInt(activeUnit.dataset.step, 10);
    unitBtns.forEach((b) => b.classList.toggle("active", b === activeUnit));
  }
  if (jogMultEl) {
    const multBtns = Array.from(jogMultEl.querySelectorAll("button[data-mult]"));
    const activeMult =
      multBtns.find((b) => parseInt(b.dataset.mult, 10) === jogMult && VALID_JOG_DIVISORS.has(jogMult)) ||
      multBtns.find((b) => parseInt(b.dataset.mult, 10) === 1) ||
      multBtns[0];
    if (activeMult) {
      jogMult = VALID_JOG_DIVISORS.has(parseInt(activeMult.dataset.mult, 10))
        ? parseInt(activeMult.dataset.mult, 10)
        : 1;
      multBtns.forEach((b) => b.classList.toggle("active", b === activeMult));
    } else {
      jogMult = 1;
    }
  }
  jogStep = Math.max(Math.round(jogUnit / jogMult), minFreqStepHz);
}

async function applyModeFromPicker() {
  const mode = modeEl.value || "";
  if (!mode) {
    showHint("Mode missing", 1500);
    return;
  }
  updateWfmControls();
  modeEl.disabled = true;
  showHint("Setting mode…");
  try {
    await postPath(`/set_mode?mode=${encodeURIComponent(mode)}`);
    showHint("Mode set", 1500);
    if (mode.toUpperCase() === "WFM") {
      setJogDivisor(10);
    }
    // Apply sensible default bandwidth for the new mode and push to server.
    await applyBwDefaultForMode(mode, true);
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

// Per-mode defaults: [default bandwidth Hz, min Hz, max Hz, step Hz]
const MODE_BW_DEFAULTS = {
  CW:     [500,    50,    2_000,  50],
  CWR:    [500,    50,    2_000,  50],
  LSB:    [2_700,  300,   6_000,  100],
  USB:    [2_700,  300,   6_000,  100],
  AM:     [6_000,  500,   15_000, 500],
  FM:     [12_500, 2_500, 25_000, 500],
  WFM:    [180_000, 50_000,300_000,5_000],
  DIG:    [3_000,  300,   6_000,  100],
  PKT:    [3_000,  300,  25_000,  100],
};
const MODE_BW_FALLBACK = [3_000, 300, 500_000, 100];

function mwDefaultsForMode(mode) {
  return MODE_BW_DEFAULTS[(mode || "").toUpperCase()] || MODE_BW_FALLBACK;
}

function formatBwLabel(hz) {
  if (hz >= 1000) return (hz / 1000).toFixed(hz % 1000 === 0 ? 0 : 1) + " kHz";
  return hz + " Hz";
}

// Current receive bandwidth (Hz) — updated by server sync and BW drag.
let currentBandwidthHz = 3_000;
const spectrumBwInput = document.getElementById("spectrum-bw-input");
const spectrumBwSetBtn = document.getElementById("spectrum-bw-set-btn");
const spectrumBwAutoBtn = document.getElementById("spectrum-bw-auto-btn");

function formatBandwidthInputKhz(hz) {
  const khz = hz / 1000;
  if (Math.abs(Math.round(khz) - khz) < 0.0001) return String(Math.round(khz));
  if (Math.abs(Math.round(khz * 10) - khz * 10) < 0.0001) return khz.toFixed(1);
  return khz.toFixed(2);
}

function syncBandwidthInput(hz) {
  if (!spectrumBwInput || !Number.isFinite(hz) || hz <= 0) return;
  const [, minBw, maxBw, stepBw] = mwDefaultsForMode(modeEl ? modeEl.value : "USB");
  spectrumBwInput.min = String(minBw / 1000);
  spectrumBwInput.max = String(maxBw / 1000);
  spectrumBwInput.step = String(stepBw / 1000);
  spectrumBwInput.value = formatBandwidthInputKhz(hz);
}

// Apply mode-specific BW default and optionally push to server.
async function applyBwDefaultForMode(mode, sendToServer) {
  const [def] = mwDefaultsForMode(mode);
  currentBandwidthHz = def;
  syncBandwidthInput(def);
  if (sendToServer) {
    try { await postPath(`/set_bandwidth?hz=${def}`); } catch (_) {}
  }
}

async function applyBandwidthFromInput() {
  if (!spectrumBwInput) return;
  const [, minBw, maxBw] = mwDefaultsForMode(modeEl ? modeEl.value : "USB");
  const nextKhz = Number(spectrumBwInput.value);
  const next = Math.round(nextKhz * 1000);
  if (!Number.isFinite(next) || next <= 0) {
    syncBandwidthInput(currentBandwidthHz);
    return;
  }
  const clamped = Math.max(minBw, Math.min(maxBw, next));
  currentBandwidthHz = clamped;
  syncBandwidthInput(clamped);
  if (lastSpectrumData) scheduleSpectrumDraw();
  try { await postPath(`/set_bandwidth?hz=${clamped}`); } catch (_) {}
}

function estimateBandwidthAroundPeak(data, centerHz) {
  if (!data || !Array.isArray(data.bins) || data.bins.length < 3 || !Number.isFinite(centerHz)) {
    return null;
  }

  const bins = data.bins;
  const maxIdx = bins.length - 1;
  const fullLoHz = data.center_hz - data.sample_rate / 2;
  const centerIdx = Math.max(
    1,
    Math.min(maxIdx - 1, Math.round(((centerHz - fullLoHz) / data.sample_rate) * maxIdx)),
  );
  const searchRadius = Math.max(6, Math.min(120, Math.round(maxIdx * 0.03)));
  const searchLo = Math.max(1, centerIdx - searchRadius);
  const searchHi = Math.min(maxIdx - 1, centerIdx + searchRadius);

  let peakIdx = centerIdx;
  for (let i = searchLo; i <= searchHi; i++) {
    if (bins[i] > bins[peakIdx]) peakIdx = i;
  }

  const sorted = [...bins].sort((a, b) => a - b);
  const noise = sorted[Math.floor(sorted.length * 0.2)];
  const peak = bins[peakIdx];
  const threshold = Math.max(noise + 4, peak - Math.max(8, (peak - noise) * 0.35));

  let left = peakIdx;
  let right = peakIdx;
  let belowCount = 0;
  for (let i = peakIdx; i > 1; i--) {
    if (bins[i] < threshold) belowCount += 1;
    else belowCount = 0;
    if (belowCount >= 2) break;
    left = i;
  }

  belowCount = 0;
  for (let i = peakIdx; i < maxIdx - 1; i++) {
    if (bins[i] < threshold) belowCount += 1;
    else belowCount = 0;
    if (belowCount >= 2) break;
    right = i;
  }

  const shoulderPad = Math.max(1, Math.round((right - left) * 0.08));
  left = Math.max(0, left - shoulderPad);
  right = Math.min(maxIdx, right + shoulderPad);

  const hzPerBin = data.sample_rate / maxIdx;
  const rawBw = Math.max(hzPerBin, (right - left) * hzPerBin);
  const [, minBw, maxBw, stepBw] = mwDefaultsForMode(modeEl ? modeEl.value : "USB");
  const clamped = Math.max(minBw, Math.min(maxBw, rawBw));
  return Math.max(stepBw, Math.round(clamped / stepBw) * stepBw);
}

async function applyAutoBandwidth() {
  if (!lastSpectrumData || lastFreqHz == null) return;
  const estimated = estimateBandwidthAroundPeak(lastSpectrumData, lastFreqHz);
  if (!Number.isFinite(estimated) || estimated <= 0) {
    syncBandwidthInput(currentBandwidthHz);
    return;
  }
  currentBandwidthHz = estimated;
  syncBandwidthInput(estimated);
  if (lastSpectrumData) scheduleSpectrumDraw();
  try {
    await postPath(`/set_bandwidth?hz=${estimated}`);
  } catch (_) {}
}

if (spectrumBwInput) {
  spectrumBwInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      applyBandwidthFromInput();
    }
  });
}
if (spectrumBwSetBtn) {
  spectrumBwSetBtn.addEventListener("click", () => { applyBandwidthFromInput(); });
}
if (spectrumBwAutoBtn) {
  spectrumBwAutoBtn.addEventListener("click", () => { applyAutoBandwidth(); });
}

// --- Tab navigation ---
document.querySelector(".tab-bar").addEventListener("click", (e) => {
  const btn = e.target.closest(".tab[data-tab]");
  if (!btn) return;
  if (authEnabled && !authRole && btn.dataset.tab !== "main") return;
  document.querySelectorAll(".tab-bar .tab").forEach((t) => t.classList.remove("active"));
  btn.classList.add("active");
  document.querySelectorAll(".tab-panel").forEach((p) => p.style.display = "none");
  document.getElementById(`tab-${btn.dataset.tab}`).style.display = "";
  if (btn.dataset.tab === "map") {
    initAprsMap();
    sizeAprsMapToViewport();
    if (aprsMap) setTimeout(() => aprsMap.invalidateSize(), 50);
  }
});

// --- Auth startup sequence ---
async function initializeApp() {
  showAuthGate(false);
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
  });
});

window.addEventListener("resize", () => {
  const mapTab = document.getElementById("tab-map");
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
const wfmControlsCol = document.getElementById("wfm-controls-col");
const wfmDeemphasisEl = document.getElementById("wfm-deemphasis");
const wfmAudioModeEl = document.getElementById("wfm-audio-mode");
const sdrGainEl = document.getElementById("sdr-gain-db");
const sdrGainSetBtn = document.getElementById("sdr-gain-set");
const wfmStFlagEl = document.getElementById("wfm-st-flag");

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
const MAX_RX_BUFFER_SECS = 0.25;
const TARGET_RX_BUFFER_SECS = 0.04;
const MIN_RX_JITTER_SAMPLES = 512;

if (wfmAudioModeEl) {
  wfmAudioModeEl.value = loadSetting("wfmAudioMode", "stereo");
  wfmAudioModeEl.addEventListener("change", () => {
    saveSetting("wfmAudioMode", wfmAudioModeEl.value);
    const enabled = wfmAudioModeEl.value !== "mono";
    postPath(`/set_wfm_stereo?enabled=${enabled ? "true" : "false"}`).catch(() => {});
  });
}
if (wfmDeemphasisEl) {
  wfmDeemphasisEl.addEventListener("change", () => {
    postPath(`/set_wfm_deemphasis?us=${encodeURIComponent(wfmDeemphasisEl.value)}`).catch(() => {});
  });
}
function submitSdrGain() {
  if (!sdrGainEl) return;
  const parsed = Number.parseFloat(sdrGainEl.value);
  if (!Number.isFinite(parsed) || parsed < 0) return;
  postPath(`/set_sdr_gain?db=${encodeURIComponent(parsed)}`).catch(() => {});
}
if (sdrGainSetBtn) {
  sdrGainSetBtn.addEventListener("click", submitSdrGain);
}
if (sdrGainEl) {
  sdrGainEl.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      submitSdrGain();
    }
  });
}
function updateWfmControls() {
  if (!wfmControlsCol) return;
  const mode = (modeEl && modeEl.value ? modeEl.value : "").toUpperCase();
  wfmControlsCol.style.display = mode === "WFM" ? "" : "none";
}

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

function resetRxDecoder() {
  if (opusDecoder) {
    try { opusDecoder.close(); } catch (e) {}
    opusDecoder = null;
  }
  nextPlayTime = 0;
}

function configureRxStream(nextInfo) {
  const nextSampleRate = (nextInfo && nextInfo.sample_rate) || 48000;
  const sampleRateChanged = !audioCtx || audioCtx.sampleRate !== nextSampleRate;
  streamInfo = nextInfo;
  updateWfmControls();
  resetRxDecoder();
  if (sampleRateChanged && audioCtx) {
    audioCtx.close().catch(() => {});
    audioCtx = null;
    rxGainNode = null;
  }
  if (!audioCtx) {
    audioCtx = new AudioContext({ sampleRate: nextSampleRate });
    audioCtx.resume().catch(() => {});
  }
  if (!rxGainNode) {
    rxGainNode = audioCtx.createGain();
    rxGainNode.connect(audioCtx.destination);
  }
  rxGainNode.gain.value = rxVolSlider.value / 100;
  rxActive = true;
  rxAudioBtn.style.borderColor = "#00d17f";
  rxAudioBtn.style.color = "#00d17f";
  audioStatus.textContent = "RX";
}

function extractAudioFrameChannels(frame) {
  const channels = Math.max(1, frame.numberOfChannels || 1);
  const frames = Math.max(0, frame.numberOfFrames || 0);
  const format = String(frame.format || "").toLowerCase();
  const isPlanar = format.includes("planar");

  if (!isPlanar) {
    const interleaved = new Float32Array(frames * channels);
    frame.copyTo(interleaved, { planeIndex: 0 });
    const out = Array.from({ length: channels }, () => new Float32Array(frames));
    for (let i = 0; i < frames; i++) {
      for (let ch = 0; ch < channels; ch++) {
        out[ch][i] = interleaved[i * channels + ch];
      }
    }
    return out;
  }

  const out = [];
  for (let ch = 0; ch < channels; ch++) {
    let len = frames;
    try {
      len = Math.max(frames, Math.floor(frame.allocationSize({ planeIndex: ch }) / 4));
    } catch (e) {}
    const plane = new Float32Array(len);
    frame.copyTo(plane, { planeIndex: ch });
    out.push(plane.length === frames ? plane : plane.subarray(0, frames));
  }
  return out;
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
        configureRxStream(JSON.parse(evt.data));
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
            const frameChannels = extractAudioFrameChannels(frame);
            const forceMono = frame.numberOfChannels >= 2
              && wfmAudioModeEl
              && wfmAudioModeEl.value === "mono"
              && modeEl
              && (modeEl.value || "").toUpperCase() === "WFM";
            const outChannels = forceMono ? 1 : frameChannels.length;
            const ab = audioCtx.createBuffer(outChannels, frame.numberOfFrames, frame.sampleRate);
            if (forceMono) {
              const monoData = new Float32Array(frame.numberOfFrames);
              for (let ch = 0; ch < frameChannels.length; ch++) {
                const plane = frameChannels[ch];
                for (let i = 0; i < frame.numberOfFrames; i++) monoData[i] += plane[i];
              }
              const inv = 1 / Math.max(1, frameChannels.length);
              for (let i = 0; i < frame.numberOfFrames; i++) monoData[i] *= inv;
              ab.copyToChannel(monoData, 0);
            } else {
              for (let ch = 0; ch < frameChannels.length; ch++) {
                ab.copyToChannel(frameChannels[ch], ch);
              }
            }
            const src = audioCtx.createBufferSource();
            src.buffer = ab;
            src.connect(rxGainNode);
            const now = audioCtx.currentTime;
            const sampleRate = (streamInfo && streamInfo.sample_rate) || frame.sampleRate || 48000;
            const minLeadSecs = Math.max(0, MIN_RX_JITTER_SAMPLES / Math.max(1, sampleRate));
            const targetLeadSecs = Math.max(TARGET_RX_BUFFER_SECS, minLeadSecs);
            if (nextPlayTime && nextPlayTime - now > MAX_RX_BUFFER_SECS) {
              nextPlayTime = now + targetLeadSecs;
            }
            if (!nextPlayTime || nextPlayTime < now + minLeadSecs) {
              nextPlayTime = now + targetLeadSecs;
            }
            const schedTime = nextPlayTime || (now + targetLeadSecs);
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
    streamInfo = null;
    updateWfmControls();
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
  streamInfo = null;
  if (audioWs) { audioWs.close(); audioWs = null; }
  if (audioCtx) { audioCtx.close(); audioCtx = null; }
  updateWfmControls();
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


// ── Spectrum display ─────────────────────────────────────────────────────────
const spectrumCanvas  = document.getElementById("spectrum-canvas");
const spectrumFreqAxis = document.getElementById("spectrum-freq-axis");
const spectrumTooltip = document.getElementById("spectrum-tooltip");
let spectrumSource = null;
let spectrumReconnectTimer = null;
let spectrumDrawPending = false;
let spectrumAxisKey = "";
let lastSpectrumRenderData = null;

// Zoom / pan state.  zoom >= 1; panFrac in [0,1] is the fraction of the full
// bandwidth at the centre of the visible window.
let spectrumZoom    = 1;
let spectrumPanFrac = 0.5;

// Y-axis level: floor = bottom dB value shown; range = total dB span.
let spectrumFloor = -115;
let spectrumRange = 80;
const SPECTRUM_SMOOTH_ALPHA = 0.42;

// BW-strip drag state.
let _bwDragEdge     = null; // "left" | "right" | null
let _bwDragStartX   = 0;
let _bwDragStartBwHz = 0;

function spectrumBgColor() {
  return canvasPalette().bg;
}

function buildSpectrumRenderData(frame) {
  if (!frame || !Array.isArray(frame.bins)) return frame;
  const prev = lastSpectrumRenderData;
  const canBlend =
    prev &&
    Array.isArray(prev.bins) &&
    prev.bins.length === frame.bins.length &&
    prev.sample_rate === frame.sample_rate &&
    prev.center_hz === frame.center_hz;
  const bins = frame.bins.map((value, idx) => {
    if (!canBlend) return value;
    const prevValue = prev.bins[idx];
    return prevValue + (value - prevValue) * SPECTRUM_SMOOTH_ALPHA;
  });
  return { ...frame, bins };
}

// Returns { loHz, hiHz, visLoHz, visHiHz, fullSpanHz, visSpanHz } and clamps
// panFrac so the view never scrolls past the edges.
function spectrumVisibleRange(data) {
  const fullSpanHz = data.sample_rate;
  const loHz       = data.center_hz - fullSpanHz / 2;
  const halfVis    = 0.5 / spectrumZoom;
  spectrumPanFrac  = Math.min(Math.max(spectrumPanFrac, halfVis), 1 - halfVis);
  const visCenterHz = loHz + spectrumPanFrac * fullSpanHz;
  const visSpanHz   = fullSpanHz / spectrumZoom;
  return {
    loHz,
    hiHz: loHz + fullSpanHz,
    visLoHz:   visCenterHz - visSpanHz / 2,
    visHiHz:   visCenterHz + visSpanHz / 2,
    fullSpanHz,
    visSpanHz,
  };
}

function canvasXToHz(cssX, cssW, range) {
  return range.visLoHz + (cssX / cssW) * range.visSpanHz;
}

function nearestSpectrumPeak(cssX, cssW, data) {
  if (!data || !Array.isArray(data.bins) || data.bins.length === 0 || cssW <= 0) {
    return null;
  }

  const bins = data.bins;
  const maxIdx = bins.length - 1;
  const range = spectrumVisibleRange(data);
  const fullLoHz = data.center_hz - data.sample_rate / 2;
  const targetHz = canvasXToHz(cssX, cssW, range);
  const targetIdx = Math.max(
    0,
    Math.min(maxIdx, Math.round(((targetHz - fullLoHz) / data.sample_rate) * maxIdx)),
  );

  const visStartIdx = Math.max(
    0,
    Math.min(maxIdx, Math.floor(((range.visLoHz - fullLoHz) / data.sample_rate) * maxIdx)),
  );
  const visEndIdx = Math.max(
    visStartIdx,
    Math.min(maxIdx, Math.ceil(((range.visHiHz - fullLoHz) / data.sample_rate) * maxIdx)),
  );
  const visSpanBins = Math.max(1, visEndIdx - visStartIdx);
  const searchRadius = Math.max(3, Math.min(80, Math.round((24 / cssW) * visSpanBins)));
  const searchLo = Math.max(1, targetIdx - searchRadius);
  const searchHi = Math.min(maxIdx - 1, targetIdx + searchRadius);

  let windowMax = -Infinity;
  const localPeaks = [];
  for (let i = searchLo; i <= searchHi; i++) {
    const val = bins[i];
    if (val > windowMax) windowMax = val;
    if (val >= bins[i - 1] && val >= bins[i + 1]) {
      localPeaks.push(i);
    }
  }

  const candidates = localPeaks.filter((i) => bins[i] >= windowMax - 6);
  const ranked = (candidates.length ? candidates : localPeaks).sort((a, b) => {
    const dist = Math.abs(a - targetIdx) - Math.abs(b - targetIdx);
    if (dist !== 0) return dist;
    return bins[b] - bins[a];
  });

  let snappedIdx = ranked[0];
  if (snappedIdx == null) {
    snappedIdx = targetIdx;
    for (let i = searchLo; i <= searchHi; i++) {
      if (bins[i] > bins[snappedIdx]) snappedIdx = i;
    }
  }

  return {
    index: snappedIdx,
    hz: Math.round(fullLoHz + (snappedIdx / maxIdx) * data.sample_rate),
    db: bins[snappedIdx],
  };
}

function nearestSpectrumPeakHz(cssX, cssW, data) {
  return nearestSpectrumPeak(cssX, cssW, data)?.hz ?? null;
}

function visibleSpectrumPeakIndices(data, limit = 24) {
  if (!data || !Array.isArray(data.bins) || data.bins.length < 3) {
    return [];
  }

  const bins = data.bins;
  const maxIdx = bins.length - 1;
  const range = spectrumVisibleRange(data);
  const fullLoHz = data.center_hz - data.sample_rate / 2;
  const visStartIdx = Math.max(
    1,
    Math.min(maxIdx - 1, Math.floor(((range.visLoHz - fullLoHz) / data.sample_rate) * maxIdx)),
  );
  const visEndIdx = Math.max(
    visStartIdx,
    Math.min(maxIdx - 1, Math.ceil(((range.visHiHz - fullLoHz) / data.sample_rate) * maxIdx)),
  );

  const peaks = [];
  for (let i = visStartIdx; i <= visEndIdx; i++) {
    const v = bins[i];
    if (v >= bins[i - 1] && v >= bins[i + 1]) {
      peaks.push(i);
    }
  }
  if (peaks.length === 0) {
    return [];
  }

  const peakValues = peaks.map((i) => bins[i]).sort((a, b) => a - b);
  const cutoff = peakValues[Math.max(0, Math.floor(peakValues.length * 0.7))];

  return peaks
    .filter((i) => bins[i] >= cutoff)
    .sort((a, b) => bins[b] - bins[a])
    .slice(0, limit)
    .sort((a, b) => a - b);
}

// Format a frequency according to the current jog-step unit.
function formatSpectrumFreq(hz) {
  if (jogUnit >= 1_000_000) return (hz / 1e6).toFixed(3) + " MHz";
  if (jogUnit >= 1_000)     return (hz / 1e3).toFixed(3) + " kHz";
  return hz.toFixed(0) + " Hz";
}

// ── Streaming ────────────────────────────────────────────────────────────────
function scheduleSpectrumReconnect() {
  if (spectrumReconnectTimer !== null) return;
  spectrumReconnectTimer = setTimeout(() => {
    spectrumReconnectTimer = null;
    startSpectrumStreaming();
  }, 1000);
}

function startSpectrumStreaming() {
  if (spectrumSource !== null) return;
  spectrumSource = new EventSource("/spectrum");
  spectrumSource.onmessage = (evt) => {
    if (evt.data === "null") {
      lastSpectrumData = null;
      lastSpectrumRenderData = null;
      overviewWaterfallRows = [];
      overviewWaterfallPushCount = 0;
      _wfResetOffscreen();
      scheduleOverviewDraw();
      clearSpectrumCanvas();
      updateRdsPsOverlay(null);
      return;
    }
    try {
      lastSpectrumData = JSON.parse(evt.data);
      lastSpectrumRenderData = buildSpectrumRenderData(lastSpectrumData);
      rdsFrameCount++;
      pushOverviewWaterfallFrame(lastSpectrumData);
      refreshCenterFreqDisplay();
      scheduleSpectrumDraw();
      updateRdsPsOverlay(lastSpectrumData.rds);
    } catch (_) {}
  };
  spectrumSource.onerror = () => {
    if (spectrumSource) {
      spectrumSource.close();
      spectrumSource = null;
    }
    scheduleSpectrumReconnect();
  };
}

function stopSpectrumStreaming() {
  if (spectrumSource !== null) {
    spectrumSource.close();
    spectrumSource = null;
  }
  if (spectrumReconnectTimer !== null) {
    clearTimeout(spectrumReconnectTimer);
    spectrumReconnectTimer = null;
  }
  spectrumDrawPending = false;
  lastSpectrumData = null;
  lastSpectrumRenderData = null;
  rdsFrameCount = 0;
  overviewWaterfallRows = [];
  overviewWaterfallPushCount = 0;
  _wfResetOffscreen();
  scheduleOverviewDraw();
  updateRdsPsOverlay(null);
  clearSpectrumCanvas();
}

// ── Rendering ────────────────────────────────────────────────────────────────
function clearSpectrumCanvas() {
  if (!spectrumCanvas) return;
  const ctx = spectrumCanvas.getContext("2d");
  ctx.fillStyle = spectrumBgColor();
  ctx.fillRect(0, 0, spectrumCanvas.width, spectrumCanvas.height);
}

function formatOverlayPs(ps) {
  return String(ps ?? "")
    .slice(0, 8)
    .padEnd(8, "_")
    .replaceAll(" ", "_");
}

function formatOverlayPi(pi) {
  return pi != null
    ? `PI 0x${pi.toString(16).toUpperCase().padStart(4, "0")}`
    : "PI --";
}

function formatOverlayPty(pty, ptyName) {
  if (ptyName) return ptyName;
  return pty != null ? String(pty) : "--";
}

function overlayTrafficFlagHtml(label, active) {
  const stateClass = active === true ? "rds-flag-active" : "rds-flag-inactive";
  return `<span class="rds-flag ${stateClass}">${label}</span>`;
}

function formatRdsFlag(value, yes = "Yes", no = "No") {
  if (value == null) return "--";
  return value ? yes : no;
}

function formatRdsAudio(value) {
  if (value == null) return "--";
  return value ? "Music" : "Speech";
}

function formatMinuteTimestamp(date = new Date()) {
  const yyyy = date.getFullYear();
  const mm = String(date.getMonth() + 1).padStart(2, "0");
  const dd = String(date.getDate()).padStart(2, "0");
  const hh = String(date.getHours()).padStart(2, "0");
  const min = String(date.getMinutes()).padStart(2, "0");
  return `${yyyy}-${mm}-${dd} ${hh}:${min}`;
}

function buildRdsRawPayload(rds) {
  return {
    time: formatMinuteTimestamp(),
    freq_hz: Number.isFinite(lastFreqHz) ? Math.round(lastFreqHz) : null,
    ...rds,
  };
}

function formatRdsAfMHz(hz) {
  return `${(hz / 1_000_000).toFixed(1)} MHz`;
}

async function tuneRdsAlternativeFrequency(hz) {
  if (!Number.isFinite(hz) || hz <= 0) return;
  const targetHz = Math.round(hz);
  try {
    await postPath(`/set_freq?hz=${targetHz}`);
    applyLocalTunedFrequency(targetHz);
    showHint(`Tuned ${formatRdsAfMHz(targetHz)}`, 1200);
  } catch (_) {
    showHint("Set freq failed", 1500);
  }
}

function renderRdsAlternativeFrequencies(list) {
  const afEl = document.getElementById("rds-af-list");
  if (!afEl) return;
  const afs = Array.isArray(list)
    ? list
      .filter((hz) => Number.isFinite(hz) && hz > 0)
      .map((hz) => Math.round(hz))
    : [];
  const afKey = afs.join(",");
  if (!afs.length) {
    if (afEl.dataset.afKey === "") return;
    afEl.dataset.afKey = "";
    afEl.textContent = "--";
    return;
  }
  if (afEl.dataset.afKey === afKey) return;
  afEl.dataset.afKey = afKey;
  afEl.innerHTML = "";
  for (const hz of afs) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "rds-af-btn";
    btn.dataset.hz = String(hz);
    btn.textContent = formatRdsAfMHz(hz);
    afEl.appendChild(btn);
  }
  if (!afEl.childElementCount) afEl.textContent = "--";
}

async function copyRdsPsToClipboard() {
  const rds = lastSpectrumData?.rds;
  const ps = rds?.program_service;
  if (!rds || !ps || ps.length === 0) {
    showHint("No RDS PS", 1200);
    return;
  }
  const freqMhz = Number.isFinite(lastFreqHz) ? (Math.round((lastFreqHz / 100_000)) / 10).toFixed(1) : "--.-";
  const piHex = rds.pi != null
    ? `0x${rds.pi.toString(16).toUpperCase().padStart(4, "0")}`
    : "--";
  const clipPs = formatOverlayPs(ps);
  const clipText = `${formatMinuteTimestamp()} - ${freqMhz} MHz - ${piHex} - ${clipPs}`;
  try {
    await navigator.clipboard.writeText(clipText);
    showHint("RDS copied", 1200);
  } catch (_) {
    showHint("Clipboard failed", 1500);
  }
}

async function copyRdsRawToClipboard() {
  const rawEl = document.getElementById("rds-raw");
  const rawText = rawEl?.textContent ?? "";
  if (!rawText || rawText === "--") {
    showHint("No RDS JSON", 1200);
    return;
  }
  try {
    await navigator.clipboard.writeText(rawText);
    showHint("RDS JSON copied", 1200);
  } catch (_) {
    showHint("Clipboard failed", 1500);
  }
}

if (rdsPsOverlay) {
  rdsPsOverlay.addEventListener("click", () => { copyRdsPsToClipboard(); });
}
const rdsPsValueEl = document.getElementById("rds-ps");
if (rdsPsValueEl) {
  rdsPsValueEl.addEventListener("click", () => { copyRdsPsToClipboard(); });
}
const rdsRawCopyBtn = document.getElementById("rds-raw-copy-btn");
if (rdsRawCopyBtn) {
  rdsRawCopyBtn.addEventListener("click", () => { copyRdsRawToClipboard(); });
}
const rdsAfListEl = document.getElementById("rds-af-list");
if (rdsAfListEl) {
  rdsAfListEl.addEventListener("click", (event) => {
    const btn = event.target instanceof HTMLElement ? event.target.closest(".rds-af-btn") : null;
    const hz = Number(btn?.dataset?.hz);
    if (btn && Number.isFinite(hz)) {
      tuneRdsAlternativeFrequency(hz);
    }
  });
}

function updateRdsPsOverlay(rds) {
  updateDocumentTitle(rds);
  // Overview strip overlay
  if (rdsPsOverlay) {
    const ps = rds?.program_service;
    const hasPs = !!(ps && ps.length > 0);
    const hasPi = rds?.pi != null;
    if (hasPs || hasPi) {
      const mainText = hasPs
        ? formatOverlayPs(ps)
        : formatOverlayPi(rds?.pi);
      const mainClass = hasPs ? "rds-ps-main" : "rds-ps-fallback";
      const metaText = hasPs
        ? `${formatOverlayPi(rds?.pi)} · ${formatOverlayPty(rds?.pty, rds?.pty_name)}`
        : (rds?.pty_name ?? (rds?.pty != null ? String(rds.pty) : ""));
      const trafficFlags =
        `<span class="rds-ps-flags">` +
        `${overlayTrafficFlagHtml("TP", rds?.traffic_program)}` +
        `${overlayTrafficFlagHtml("TA", rds?.traffic_announcement)}` +
        `</span>`;
      rdsPsOverlay.innerHTML =
        `<span class="${mainClass}">${escapeMapHtml(mainText)}</span>` +
        `<span class="rds-ps-meta">` +
        `<span class="rds-ps-meta-text">${escapeMapHtml(metaText)}</span>` +
        `${trafficFlags}` +
        `</span>`;
      positionRdsPsOverlay();
      rdsPsOverlay.style.display = "flex";
    } else {
      rdsPsOverlay.innerHTML = "";
      rdsPsOverlay.style.display = "none";
    }
  }

  // RDS debug panel
  const statusEl   = document.getElementById("rds-status");
  const modeEl     = document.getElementById("rds-mode");
  const piEl       = document.getElementById("rds-pi");
  const psEl       = document.getElementById("rds-ps");
  const ptyEl      = document.getElementById("rds-pty");
  const ptyNameEl  = document.getElementById("rds-pty-name");
  const ptynEl     = document.getElementById("rds-ptyn");
  const tpEl       = document.getElementById("rds-tp");
  const taEl       = document.getElementById("rds-ta");
  const musicEl    = document.getElementById("rds-music");
  const stereoEl   = document.getElementById("rds-stereo");
  const compEl     = document.getElementById("rds-compressed");
  const headEl     = document.getElementById("rds-artificial-head");
  const dynPtyEl   = document.getElementById("rds-dynamic-pty");
  const afEl       = document.getElementById("rds-af-list");
  const rtEl       = document.getElementById("rds-radio-text");
  const rawEl      = document.getElementById("rds-raw");
  if (!statusEl) return;

  // Always show the current mode, frame counter, and a sanitised spectrum snapshot
  if (modeEl) modeEl.textContent = document.getElementById("mode")?.value || "--";
  const framesEl = document.getElementById("rds-frames");
  if (framesEl) framesEl.textContent = String(rdsFrameCount);

  if (!rds) {
    statusEl.textContent = "No signal";
    statusEl.className = "rds-value rds-no-signal";
    piEl.textContent = "--";
    psEl.textContent = "--";
    ptyEl.textContent = "--";
    ptyNameEl.textContent = "--";
    if (ptynEl) ptynEl.textContent = "--";
    if (tpEl) tpEl.textContent = "--";
    if (taEl) taEl.textContent = "--";
    if (musicEl) musicEl.textContent = "--";
    if (stereoEl) stereoEl.textContent = "--";
    if (compEl) compEl.textContent = "--";
    if (headEl) headEl.textContent = "--";
    if (dynPtyEl) dynPtyEl.textContent = "--";
    if (afEl) afEl.textContent = "--";
    if (rtEl) rtEl.textContent = "--";
    if (rawEl && lastSpectrumData) {
      const { bins: _b, ...rest } = lastSpectrumData;
      rawEl.textContent = JSON.stringify({
        time: formatMinuteTimestamp(),
        freq_hz: Number.isFinite(lastFreqHz) ? Math.round(lastFreqHz) : null,
        ...rest,
      }, null, 2);
    }
    return;
  }

  statusEl.textContent = "Decoding";
  statusEl.className = "rds-value rds-decoding";
  piEl.textContent = rds.pi != null ? `0x${rds.pi.toString(16).toUpperCase().padStart(4, "0")}` : "--";
  psEl.textContent = rds.program_service ?? "--";
  ptyEl.textContent = rds.pty_name ?? (rds.pty != null ? String(rds.pty) : "--");
  ptyNameEl.textContent = rds.pty != null ? String(rds.pty) : "--";
  if (ptynEl) ptynEl.textContent = rds.program_type_name_long ?? "--";
  if (tpEl) tpEl.textContent = formatRdsFlag(rds.traffic_program);
  if (taEl) taEl.textContent = formatRdsFlag(rds.traffic_announcement);
  if (musicEl) musicEl.textContent = formatRdsAudio(rds.music);
  if (stereoEl) stereoEl.textContent = formatRdsFlag(rds.stereo);
  if (compEl) compEl.textContent = formatRdsFlag(rds.compressed);
  if (headEl) headEl.textContent = formatRdsFlag(rds.artificial_head);
  if (dynPtyEl) dynPtyEl.textContent = formatRdsFlag(rds.dynamic_pty);
  renderRdsAlternativeFrequencies(rds.alternative_frequencies_hz);
  if (rtEl) rtEl.textContent = rds.radio_text ?? "--";
  rawEl.textContent = JSON.stringify(buildRdsRawPayload(rds), null, 2);
}

function scheduleSpectrumDraw() {
  if (spectrumDrawPending) return;
  spectrumDrawPending = true;
  requestAnimationFrame(() => {
    spectrumDrawPending = false;
    if (lastSpectrumRenderData) {
      drawSpectrum(lastSpectrumRenderData);
      if (overviewWaterfallRows.length > 0) scheduleOverviewDraw();
    }
  });
}

function drawSpectrum(data) {
  if (!spectrumCanvas) return;

  // HiDPI sizing
  const dpr  = window.devicePixelRatio || 1;
  const cssW = spectrumCanvas.clientWidth  || 640;
  const cssH = spectrumCanvas.clientHeight || 160;
  const W = Math.round(cssW * dpr);
  const H = Math.round(cssH * dpr);
  if (spectrumCanvas.width !== W || spectrumCanvas.height !== H) {
    spectrumCanvas.width  = W;
    spectrumCanvas.height = H;
  }

  const ctx   = spectrumCanvas.getContext("2d");
  const pal   = canvasPalette();
  const range = spectrumVisibleRange(data);
  const bins  = data.bins;
  const n     = bins.length;

  // Background
  ctx.fillStyle = pal.bg;
  ctx.fillRect(0, 0, W, H);

  if (!n) return;

  const DB_MIN  = spectrumFloor;
  const DB_MAX  = spectrumFloor + spectrumRange;
  const dbRange = DB_MAX - DB_MIN;
  const fullSpanHz = data.sample_rate;
  const loHz       = data.center_hz - fullSpanHz / 2;

  // Horizontal dB grid lines
  ctx.strokeStyle = pal.spectrumGrid;
  ctx.lineWidth = 1;
  const gridStep = spectrumRange > 100 ? 20 : 10;
  for (let db = Math.ceil(DB_MIN / gridStep) * gridStep; db <= DB_MAX; db += gridStep) {
    const y = Math.round(H * (1 - (db - DB_MIN) / dbRange));
    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(W, y); ctx.stroke();
  }

  // Y-axis dB labels (left side)
  ctx.save();
  ctx.font = `${Math.round(9 * dpr)}px monospace`;
  ctx.fillStyle = pal.spectrumLabel;
  ctx.textAlign = "left";
  for (let db = Math.ceil(DB_MIN / gridStep) * gridStep; db <= DB_MAX; db += gridStep) {
    const y = Math.round(H * (1 - (db - DB_MIN) / dbRange));
    if (y > 8 * dpr && y < H - 2 * dpr) {
      ctx.fillText(`${db}`, 4 * dpr, y - 2 * dpr);
    }
  }
  ctx.restore();

  // Coordinate helpers
  function hzToX(hz) {
    return ((hz - range.visLoHz) / range.visSpanHz) * W;
  }
  function binX(i) {
    return hzToX(loHz + (i / (n - 1)) * fullSpanHz);
  }
  function binY(i) {
    const db = Math.max(DB_MIN, Math.min(DB_MAX, bins[i]));
    return H * (1 - (db - DB_MIN) / dbRange);
  }

  // ── BW strip (drawn before spectrum so traces appear on top) ──────────────
  if (lastFreqHz != null && currentBandwidthHz > 0) {
    const halfBw = currentBandwidthHz / 2;
    const xL = hzToX(lastFreqHz - halfBw);
    const xR = hzToX(lastFreqHz + halfBw);
    const stripW = xR - xL;

    if (stripW > 1) {
      // Warm amber gradient fill
      const grd = ctx.createLinearGradient(xL, 0, xR, 0);
      grd.addColorStop(0,   "rgba(240,173,78,0.05)");
      grd.addColorStop(0.2, "rgba(240,173,78,0.14)");
      grd.addColorStop(0.5, "rgba(240,173,78,0.19)");
      grd.addColorStop(0.8, "rgba(240,173,78,0.14)");
      grd.addColorStop(1,   "rgba(240,173,78,0.05)");
      ctx.fillStyle = grd;
      ctx.fillRect(xL, 0, stripW, H);

      // Edge handle bars
      const EDGE = 5 * dpr;
      ctx.fillStyle = "rgba(240,173,78,0.30)";
      ctx.fillRect(xL, 0, EDGE, H);
      ctx.fillRect(xR - EDGE, 0, EDGE, H);

      // Edge border lines
      ctx.strokeStyle = "rgba(240,173,78,0.70)";
      ctx.lineWidth = 1.5 * dpr;
      ctx.beginPath(); ctx.moveTo(xL, 0); ctx.lineTo(xL, H); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(xR, 0); ctx.lineTo(xR, H); ctx.stroke();

      // Bottom bookmark tab centered on the dial frequency
      const xMid = hzToX(lastFreqHz);
      const bwText = formatBwLabel(currentBandwidthHz);
      ctx.save();
      ctx.font = `bold ${Math.round(10 * dpr)}px sans-serif`;
      const tw = ctx.measureText(bwText).width;
      const PAD  = 6 * dpr;
      const TAB_H = 16 * dpr;
      const tabX = Math.max(0, Math.min(W - tw - PAD * 2, xMid - (tw + PAD * 2) / 2));
      const tabY = H - TAB_H;
      const r = 3 * dpr;
      // Rounded-bottom tab shape (flat top)
      ctx.fillStyle = "rgba(240,173,78,0.85)";
      ctx.beginPath();
      ctx.moveTo(tabX, tabY);
      ctx.lineTo(tabX + tw + PAD * 2, tabY);
      ctx.lineTo(tabX + tw + PAD * 2, H - r);
      ctx.arcTo(tabX + tw + PAD * 2, H, tabX + tw + PAD * 2 - r, H, r);
      ctx.lineTo(tabX + r, H);
      ctx.arcTo(tabX, H, tabX, H - r, r);
      ctx.lineTo(tabX, tabY);
      ctx.closePath();
      ctx.fill();
      // Tab text
      ctx.fillStyle = spectrumBgColor();
      ctx.textAlign = "left";
      ctx.fillText(bwText, tabX + PAD, H - 4 * dpr);
      ctx.restore();
    }
  }

  // ── Spectrum fill ─────────────────────────────────────────────────────────
  ctx.save();
  ctx.beginPath();
  ctx.moveTo(binX(0), H);
  for (let i = 0; i < n; i++) ctx.lineTo(binX(i), binY(i));
  ctx.lineTo(binX(n - 1), H);
  ctx.closePath();
  ctx.fillStyle = pal.spectrumFill;
  ctx.fill();
  ctx.restore();

  // ── Spectrum line ─────────────────────────────────────────────────────────
  ctx.save();
  ctx.beginPath();
  ctx.strokeStyle = pal.spectrumLine;
  ctx.lineWidth   = Math.max(1, dpr);
  for (let i = 0; i < n; i++) {
    const x = binX(i), y = binY(i);
    i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
  }
  ctx.stroke();
  ctx.restore();

  // ── Peak markers for easier snap-tune targeting ──────────────────────────
  const markerPeaks = visibleSpectrumPeakIndices(data);
  if (markerPeaks.length > 0) {
    ctx.save();
    ctx.fillStyle = pal.waveformPeak;
    ctx.strokeStyle = pal.bg;
    ctx.lineWidth = Math.max(1, dpr * 0.75);
    const radius = Math.max(2, dpr * 1.6);
    for (const idx of markerPeaks) {
      const x = binX(idx);
      const y = binY(idx);
      ctx.beginPath();
      ctx.arc(x, y - radius * 0.35, radius, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();
    }
    ctx.restore();
  }

  // ── Tuned-frequency marker ────────────────────────────────────────────────
  if (lastFreqHz != null) {
    const xf = hzToX(lastFreqHz);
    if (xf >= 0 && xf <= W) {
      ctx.save();
      ctx.setLineDash([4 * dpr, 4 * dpr]);
      ctx.strokeStyle = "#ff1744";
      ctx.lineWidth   = Math.max(1, dpr);
      ctx.beginPath(); ctx.moveTo(xf, 0); ctx.lineTo(xf, H); ctx.stroke();
      ctx.restore();
    }
  }

  updateSpectrumFreqAxis(range);
}

function updateSpectrumFreqAxis(range) {
  if (!spectrumFreqAxis) return;
  const spanHz = range.visSpanHz;
  // Pick a step that gives ~5 labels
  const targets = [100, 200, 500, 1e3, 2e3, 5e3, 10e3, 20e3, 50e3,
                   100e3, 200e3, 500e3, 1e6, 2e6, 5e6, 10e6];
  const ideal = spanHz / 5;
  const stepHz = targets.reduce((best, s) =>
    Math.abs(s - ideal) < Math.abs(best - ideal) ? s : best, targets[0]);
  const axisKey = [
    Math.round(range.visLoHz),
    Math.round(range.visHiHz),
    Math.round(stepHz),
    spectrumFreqAxis.clientWidth || 0,
  ].join(":");
  if (axisKey === spectrumAxisKey) return;
  spectrumAxisKey = axisKey;

  const firstHz = Math.ceil(range.visLoHz / stepHz) * stepHz;
  spectrumFreqAxis.innerHTML = "";
  for (let hz = firstHz; hz <= range.visHiHz + stepHz * 0.01; hz += stepHz) {
    const frac = (hz - range.visLoHz) / range.visSpanHz;
    if (frac < 0 || frac > 1) continue;
    const label = hz >= 1e6
      ? (hz / 1e6).toFixed(stepHz < 1e6 ? (stepHz < 100e3 ? 3 : 1) : 0) + " M"
      : hz >= 1e3
        ? (hz / 1e3).toFixed(stepHz < 1e3 ? 1 : 0) + " k"
        : hz.toFixed(0);
    const span = document.createElement("span");
    span.textContent = label;
    span.style.left  = (frac * 100).toFixed(2) + "%";
    spectrumFreqAxis.appendChild(span);
  }
}

// ── Zoom helpers ──────────────────────────────────────────────────────────────
function spectrumZoomAt(cssX, cssW, data, factor) {
  const range   = spectrumVisibleRange(data);
  const hzAtCursor = canvasXToHz(cssX, cssW, range);
  const frac    = cssX / cssW;
  spectrumZoom  = Math.max(1, Math.min(64, spectrumZoom * factor));
  // Recompute so the pixel under the cursor keeps the same frequency
  const newVisSpan    = data.sample_rate / spectrumZoom;
  const newVisCenter  = hzAtCursor + (0.5 - frac) * newVisSpan;
  const loHz          = data.center_hz - data.sample_rate / 2;
  spectrumPanFrac     = (newVisCenter - loHz) / data.sample_rate;
}

// ── Scroll to zoom ────────────────────────────────────────────────────────────
if (spectrumCanvas) {
  spectrumCanvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    if (!lastSpectrumData) return;
    if (e.ctrlKey) {
      const direction = e.deltaY < 0 ? 1 : -1;
      jogFreq(direction);
      return;
    }
    const rect   = spectrumCanvas.getBoundingClientRect();
    const cssX   = e.clientX - rect.left;
    const factor = e.deltaY < 0 ? 1.25 : 1 / 1.25;
    spectrumZoomAt(cssX, rect.width, lastSpectrumData, factor);
    scheduleSpectrumDraw();
  }, { passive: false });
}

// ── BW strip edge hit-test (CSS pixels) ──────────────────────────────────────
function getBwEdgeHit(cssX, cssW, range) {
  if (!lastFreqHz || !currentBandwidthHz || !lastSpectrumData) return null;
  const halfBw = currentBandwidthHz / 2;
  const xL = ((lastFreqHz - halfBw - range.visLoHz) / range.visSpanHz) * cssW;
  const xR = ((lastFreqHz + halfBw - range.visLoHz) / range.visSpanHz) * cssW;
  const HIT = 8;
  if (Math.abs(cssX - xL) < HIT) return "left";
  if (Math.abs(cssX - xR) < HIT) return "right";
  return null;
}

// ── Mouse drag to pan / BW resize ─────────────────────────────────────────────
let _sDragStart = null;  // { clientX, panFrac }
let _sDragMoved = false;

if (spectrumCanvas) {
  spectrumCanvas.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    if (lastSpectrumData) {
      const rect  = spectrumCanvas.getBoundingClientRect();
      const cssX  = e.clientX - rect.left;
      const range = spectrumVisibleRange(lastSpectrumData);
      const edge  = getBwEdgeHit(cssX, rect.width, range);
      if (edge) {
        _bwDragEdge      = edge;
        _bwDragStartX    = cssX;
        _bwDragStartBwHz = currentBandwidthHz;
        _sDragStart      = null;
        _sDragMoved      = true; // suppress click-to-tune
        return;
      }
    }
    _sDragStart = { clientX: e.clientX, panFrac: spectrumPanFrac };
    _sDragMoved = false;
  });

  window.addEventListener("mousemove", (e) => {
    if (_bwDragEdge && lastSpectrumData) {
      const rect  = spectrumCanvas.getBoundingClientRect();
      const cssX  = e.clientX - rect.left;
      const range = spectrumVisibleRange(lastSpectrumData);
      const dxHz  = ((cssX - _bwDragStartX) / rect.width) * range.visSpanHz;
      let newBw   = _bwDragEdge === "right"
        ? _bwDragStartBwHz + dxHz * 2
        : _bwDragStartBwHz - dxHz * 2;
      const [, minBw, maxBw] = mwDefaultsForMode(modeEl ? modeEl.value : "USB");
      newBw = Math.round(Math.max(minBw, Math.min(maxBw, newBw)));
      currentBandwidthHz = newBw;
      syncBandwidthInput(newBw);
      scheduleSpectrumDraw();
      return;
    }
    if (!_sDragStart || !lastSpectrumData) return;
    const rect  = spectrumCanvas.getBoundingClientRect();
    const dx    = e.clientX - _sDragStart.clientX;
    if (Math.abs(dx) > 3) _sDragMoved = true;
    spectrumPanFrac = _sDragStart.panFrac - (dx / rect.width) / spectrumZoom;
    scheduleSpectrumDraw();
  });

  window.addEventListener("mouseup", async () => {
    if (_bwDragEdge) {
      try { await postPath(`/set_bandwidth?hz=${Math.round(currentBandwidthHz)}`); } catch (_) {}
      _bwDragEdge = null;
      return;
    }
    _sDragStart = null;
  });
}

// ── Touch: pinch-to-zoom + single-finger pan ──────────────────────────────────
let _sTouch = null;

if (spectrumCanvas) {
  spectrumCanvas.addEventListener("touchstart", (e) => {
    e.preventDefault();
    if (e.touches.length === 2) {
      const t0 = e.touches[0], t1 = e.touches[1];
      _sTouch = {
        type:    "pinch",
        dist:    Math.hypot(t1.clientX - t0.clientX, t1.clientY - t0.clientY),
        midX:    (t0.clientX + t1.clientX) / 2,
        zoom:    spectrumZoom,
        panFrac: spectrumPanFrac,
      };
    } else if (e.touches.length === 1) {
      _sTouch = { type: "pan", clientX: e.touches[0].clientX, panFrac: spectrumPanFrac };
    }
  }, { passive: false });

  spectrumCanvas.addEventListener("touchmove", (e) => {
    e.preventDefault();
    if (!_sTouch || !lastSpectrumData) return;
    const rect = spectrumCanvas.getBoundingClientRect();
    if (_sTouch.type === "pinch" && e.touches.length === 2) {
      const t0 = e.touches[0], t1 = e.touches[1];
      const newDist = Math.hypot(t1.clientX - t0.clientX, t1.clientY - t0.clientY);
      const newMidX = (t0.clientX + t1.clientX) / 2;
      const scale   = newDist / _sTouch.dist;
      const newZoom = Math.max(1, Math.min(64, _sTouch.zoom * scale));
      const loHz    = lastSpectrumData.center_hz - lastSpectrumData.sample_rate / 2;
      // Compute Hz under original midpoint in original view
      const oldVisSpan   = lastSpectrumData.sample_rate / _sTouch.zoom;
      const oldVisLo     = loHz + _sTouch.panFrac * lastSpectrumData.sample_rate - oldVisSpan / 2;
      const midFrac      = (_sTouch.midX - rect.left) / rect.width;
      const midHz        = oldVisLo + midFrac * oldVisSpan;
      const newVisSpan   = lastSpectrumData.sample_rate / newZoom;
      const newVisCenter = midHz + (0.5 - midFrac) * newVisSpan;
      spectrumZoom    = newZoom;
      spectrumPanFrac = (newVisCenter - loHz) / lastSpectrumData.sample_rate;
      // Pan contribution from mid shift
      const dxMid = newMidX - _sTouch.midX;
      spectrumPanFrac -= (dxMid / rect.width) / spectrumZoom;
      scheduleSpectrumDraw();
    } else if (_sTouch.type === "pan" && e.touches.length === 1) {
      const dx = e.touches[0].clientX - _sTouch.clientX;
      spectrumPanFrac = _sTouch.panFrac - (dx / rect.width) / spectrumZoom;
      scheduleSpectrumDraw();
    }
  }, { passive: false });

  spectrumCanvas.addEventListener("touchend", () => { _sTouch = null; });
}

// ── Hover tooltip + cursor ────────────────────────────────────────────────────
if (spectrumCanvas) {
  spectrumCanvas.addEventListener("mousemove", (e) => {
    if (!lastSpectrumData || !spectrumTooltip) return;
    const rect  = spectrumCanvas.getBoundingClientRect();
    const cssX  = e.clientX - rect.left;
    const range = spectrumVisibleRange(lastSpectrumData);
    // Change cursor when hovering near BW strip edges
    const edge = getBwEdgeHit(cssX, rect.width, range);
    spectrumCanvas.style.cursor = edge ? "ew-resize" : "crosshair";
    const hz = canvasXToHz(cssX, rect.width, range);
    const peak = edge ? null : nearestSpectrumPeak(cssX, rect.width, lastSpectrumData);
    const peakHz = peak?.hz ?? null;
    const peakDb = peak && Number.isFinite(peak.db) ? `${peak.db.toFixed(1)} dB` : null;
    if (peakHz != null && Math.abs(peakHz - hz) >= Math.max(minFreqStepHz, 10)) {
      spectrumTooltip.textContent = peakDb
        ? `Peak ${formatSpectrumFreq(peakHz)} · ${peakDb}`
        : `Peak ${formatSpectrumFreq(peakHz)}`;
    } else {
      const baseText = formatSpectrumFreq(peakHz ?? hz);
      spectrumTooltip.textContent = peakDb ? `${baseText} · ${peakDb}` : baseText;
    }
    spectrumTooltip.style.display = "block";
    const tw = spectrumTooltip.offsetWidth;
    let tx = cssX + 10;
    if (tx + tw > rect.width) tx = cssX - tw - 10;
    spectrumTooltip.style.left = tx + "px";
    spectrumTooltip.style.top  = Math.max(0, e.clientY - rect.top - 28) + "px";
  });
  spectrumCanvas.addEventListener("mouseleave", () => {
    if (spectrumTooltip) spectrumTooltip.style.display = "none";
    spectrumCanvas.style.cursor = "crosshair";
  });
}

// ── Click to tune (only when not dragging) ────────────────────────────────────
if (spectrumCanvas) {
  spectrumCanvas.addEventListener("click", (e) => {
    if (_sDragMoved) { _sDragMoved = false; return; }
    if (!lastSpectrumData) return;
    const rect  = spectrumCanvas.getBoundingClientRect();
    const cssX = e.clientX - rect.left;
    const range = spectrumVisibleRange(lastSpectrumData);
    const targetHz = nearestSpectrumPeakHz(cssX, rect.width, lastSpectrumData)
      ?? Math.round(canvasXToHz(cssX, rect.width, range));
    postPath(`/set_freq?hz=${targetHz}`)
      .then(() => { applyLocalTunedFrequency(targetHz); })
      .catch(() => {});
  });
}

// ── Spectrum floor input + Auto level ────────────────────────────────────────
(function () {
  const floorInput = document.getElementById("spectrum-floor-input");
  const autoBtn    = document.getElementById("spectrum-auto-btn");

  if (floorInput) {
    floorInput.addEventListener("change", () => {
      const v = Number(floorInput.value);
      if (!isNaN(v)) {
        spectrumFloor = v;
        if (lastSpectrumData) scheduleSpectrumDraw();
      }
    });
  }

  if (autoBtn) {
    autoBtn.addEventListener("click", () => {
      if (!lastSpectrumData) return;
      const sorted = [...lastSpectrumData.bins].sort((a, b) => a - b);
      // Use 15th-percentile as noise floor, peak for top
      const noise = sorted[Math.floor(sorted.length * 0.15)];
      const peak  = sorted[sorted.length - 1];
      spectrumFloor = Math.floor(noise / 10) * 10 - 10;
      spectrumRange = Math.max(60, Math.ceil((peak - spectrumFloor) / 10) * 10 + 10);
      if (floorInput) floorInput.value = spectrumFloor;
      scheduleSpectrumDraw();
    });
  }
})();
