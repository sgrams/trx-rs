// --- Decoder registry (fetched from /decoders on load) ---
/** @type {Array<{id:string,label:string,activation:string,active_modes:string[],background_decode:boolean,bookmark_selectable:boolean}>} */
let decoderRegistry = [];
window.decoderRegistry = decoderRegistry;

/** Callbacks invoked once the decoder registry is fetched. */
const _decoderRegistryReadyCallbacks = [];
window.onDecoderRegistryReady = function (fn) {
  if (decoderRegistry.length > 0) fn();
  else _decoderRegistryReadyCallbacks.push(fn);
};

(async function fetchDecoderRegistry() {
  try {
    const resp = await fetch("/decoders");
    if (resp.ok) {
      decoderRegistry = await resp.json();
      window.decoderRegistry = decoderRegistry;
      for (const fn of _decoderRegistryReadyCallbacks) fn();
      _decoderRegistryReadyCallbacks.length = 0;
    }
  } catch (e) {
    console.error("Failed to fetch decoder registry:", e);
  }
})();

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
function escapeMapHtml(input) {
  return String(input)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("\"", "&quot;");
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
    setDecodeHistoryOverlayVisible(false);
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
  setDecodeHistoryOverlayVisible(false);
  document.getElementById("loading").style.display = "none";
  document.getElementById("content").style.display = "none";
  const authGate = document.getElementById("auth-gate");
  authGate.style.display = "flex";
  authGate.style.flexDirection = "column";
  authGate.style.justifyContent = "center";
  authGate.style.alignItems = "stretch";
  const signalVisualBlock = document.querySelector(".signal-visual-block");
  if (signalVisualBlock) {
    signalVisualBlock.style.display = "none";
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
  const signalVisualBlock = document.querySelector(".signal-visual-block");
  if (signalVisualBlock) {
    signalVisualBlock.style.display = "";
  }

  // Show the tab that matches the current route.
  document.querySelectorAll(".tab-panel").forEach(panel => {
    panel.style.display = "none";
  });
  document.querySelectorAll(".tab-bar .tab").forEach(btn => {
    btn.classList.remove("active");
  });
  navigateToTab(tabFromPath(), { updateHistory: false, replaceHistory: true });
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
    if (badge) badge.style.display = "block";
    if (badgeRole) badgeRole.textContent = authRole === "control" ? "Control (full access)" : "RX (read-only)";
    if (headerAuthBtn) {
      headerAuthBtn.textContent = "Logout";
      headerAuthBtn.style.display = "block";
    }
  } else {
    if (badge) badge.style.display = "none";
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
      "ft4-decode-toggle-btn",
      "ft2-decode-toggle-btn",
      "wspr-decode-toggle-btn",
      "lrpt-decode-toggle-btn",
      "hf-aprs-decode-toggle-btn",
      "cw-auto",
      "settings-clear-ais-history",
      "settings-clear-vdes-history",
      "settings-clear-aprs-history",
      "settings-clear-hf-aprs-history",
      "settings-clear-cw-history",
      "settings-clear-ft8-history",
      "settings-clear-ft4-history",
      "settings-clear-ft2-history",
      "settings-clear-wspr-history",
      "settings-clear-sat-history",
      "header-rec-btn",
      "recorder-start-btn",
      "recorder-stop-btn"
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
  if (signalVisualBlockEl) signalVisualBlockEl.style.display = "";

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
      setSignalSplitControlVisible(true);
      if (centerFreqField) centerFreqField.style.display = "";
      startSpectrumStreaming();
    } else {
      spectrumPanel.style.display = "none";
      setSignalSplitControlVisible(false);
      if (centerFreqField) centerFreqField.style.display = "none";
      stopSpectrumStreaming();
      resizeHeaderSignalCanvas();
      scheduleOverviewDraw();
    }
    scheduleSpectrumLayout();
  }
  if (!caps.filter_controls) {
    sdrSquelchSupported = false;
  }
  updateSdrSquelchControlVisibility();
  if (typeof vchanApplyCapabilities === "function") vchanApplyCapabilities(caps);
}

const freqEl = document.getElementById("freq");
const centerFreqEl = document.getElementById("center-freq");
const wavelengthEl = document.getElementById("wavelength");
const sigStrengthEl = document.getElementById("sig-strength");
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
const locationSubtitle = document.getElementById("location-subtitle");
const loadingTitle = document.getElementById("loading-title");
const loadingSub = document.getElementById("loading-sub");
const decodeHistoryOverlayEl = document.getElementById("decode-history-overlay");
const decodeHistoryOverlayTitleEl = document.getElementById("decode-history-overlay-title");
const decodeHistoryOverlaySubEl = document.getElementById("decode-history-overlay-sub");
const connLostOverlayEl = document.getElementById("conn-lost-overlay");
const connLostOverlayTitleEl = document.getElementById("conn-lost-overlay-title");
const connLostOverlaySubEl = document.getElementById("conn-lost-overlay-sub");
const overviewCanvas = document.getElementById("overview-canvas");
const signalOverlayCanvas = document.getElementById("signal-overlay-canvas");
// Screenshots composite these live WebGL canvases into a PNG.
const spectrumSnapshotGlOptions = { alpha: true, preserveDrawingBuffer: true };
const overviewGl = typeof createTrxWebGlRenderer === "function"
  ? createTrxWebGlRenderer(overviewCanvas, spectrumSnapshotGlOptions)
  : null;
const signalOverlayGl = typeof createTrxWebGlRenderer === "function"
  ? createTrxWebGlRenderer(signalOverlayCanvas, spectrumSnapshotGlOptions)
  : null;
const signalVisualBlockEl = document.querySelector(".signal-visual-block");
const signalSplitControlEl = document.getElementById("signal-split-control");
const signalSplitSliderEl = document.getElementById("signal-split-slider");
const signalSplitValueEl = document.getElementById("signal-split-value");
const overviewPeakHoldEl = document.getElementById("overview-peak-hold");
const themeToggleBtn = document.getElementById("theme-toggle");
const headerRigSwitchSelect = document.getElementById("header-rig-switch-select");
const headerStylePickSelect = document.getElementById("header-style-pick-select");
const rdsPsOverlay = document.getElementById("rds-ps-overlay");
const tabMainEl = document.getElementById("tab-main");
// Cached About-tab elements (avoid getElementById on every SSE render)
const aboutServerVerEl = document.getElementById("about-server-ver");
const aboutServerBuildDateEl = document.getElementById("about-server-build-date");
const aboutServerAddrEl = document.getElementById("about-server-addr");
const aboutServerCallEl = document.getElementById("about-server-call");
const aboutServerLocationEl = document.getElementById("about-server-location");
const aboutRigInfoEl = document.getElementById("about-rig-info");
const aboutRigAccessEl = document.getElementById("about-rig-access");
const aboutModesEl = document.getElementById("about-modes");
const aboutVfosEl = document.getElementById("about-vfos");
const aboutActiveRigEl = document.getElementById("about-active-rig");
const aboutAudioCodecEl = document.getElementById("about-audio-codec");
const aboutAudioSamplerateEl = document.getElementById("about-audio-samplerate");
const aboutAudioChannelsEl = document.getElementById("about-audio-channels");
const aboutAudioBitrateEl = document.getElementById("about-audio-bitrate");
const aboutAudioFrameEl = document.getElementById("about-audio-frame");
const aboutAudioRxEl = document.getElementById("about-audio-rx");
const aboutAudioStreamsEl = document.getElementById("about-audio-streams");
const aboutPskreporterEl = document.getElementById("about-pskreporter");
const aboutAprsIsEl = document.getElementById("about-aprs-is");
const aboutRigctlClientsEl = document.getElementById("about-rigctl-clients");
const aboutRigctlEndpointEl = document.getElementById("about-rigctl-endpoint");
const aboutClientsEl = document.getElementById("about-clients");
// Cached CW elements (avoid getElementById on every SSE render)
const cwAutoEl = document.getElementById("cw-auto");
const cwWpmEl = document.getElementById("cw-wpm");
const cwToneEl = document.getElementById("cw-tone");
let overviewPeakHoldMs = Number(loadSetting("overviewPeakHoldMs", 2000));
let decodeHistoryRetentionMin = 24 * 60;

// Cached decoder toggle buttons — built from the registry, keyed by status
// field name (e.g. "ft8_decode_enabled").  Lazily populated on first SSE.
const _decoderToggles = {};
function _ensureDecoderToggles() {
  if (Object.keys(_decoderToggles).length > 0) return;
  for (const d of decoderRegistry) {
    if (d.activation !== "toggle") continue;
    const key = d.id.replace(/-/g, "_") + "_decode_enabled";
    const el = document.getElementById(d.id + "-decode-toggle-btn");
    if (el) _decoderToggles[key] = { el, last: null, label: d.label };
  }
}

function syncDecoderToggle(entry, enabled, label) {
  if (!entry.el || entry.last === enabled) return;
  entry.last = enabled;
  entry.el.dataset.enabled = enabled ? "true" : "false";
  entry.el.textContent = enabled ? `Disable ${label}` : `Enable ${label}`;
  entry.el.style.borderColor = enabled ? "#00d17f" : "";
  entry.el.style.color = enabled ? "#00d17f" : "";
}

// Cached About-tab decoder status elements — avoids 8× getElementById per render().
const _aboutDecEls = [
  "about-dec-ft8", "about-dec-ft4", "about-dec-ft2", "about-dec-wspr",
  "about-dec-cw", "about-dec-aprs", "about-dec-lrpt",
].map((id) => ({ el: document.getElementById(id), last: null }));

function syncAboutDecoder(idx, enabled) {
  const entry = _aboutDecEls[idx];
  if (!entry || !entry.el || entry.last === enabled) return;
  entry.last = enabled;
  entry.el.textContent = enabled ? "Active" : "Off";
  entry.el.className = enabled ? "about-status-on" : "about-status-off";
}

let primaryRds = null;
let vchanRdsById = new Map();
let vchanSignalDbById = new Map();
let rdsOverlayEntries = [];

function currentDecodeHistoryRetentionMs() {
  const minutes = Math.max(1, Math.round(Number(decodeHistoryRetentionMin) || (24 * 60)));
  return minutes * 60 * 1000;
}

window.getDecodeHistoryRetentionMs = currentDecodeHistoryRetentionMs;

window.applyDecodeHistoryRetention = function() {
  if (typeof window.pruneAprsHistoryView === "function") window.pruneAprsHistoryView();
  if (typeof window.pruneHfAprsHistoryView === "function") window.pruneHfAprsHistoryView();
  if (typeof window.pruneAisHistoryView === "function") window.pruneAisHistoryView();
  if (typeof window.pruneVdesHistoryView === "function") window.pruneVdesHistoryView();
  if (typeof window.pruneFt8HistoryView === "function") window.pruneFt8HistoryView();
  if (typeof window.pruneWsprHistoryView === "function") window.pruneWsprHistoryView();
};

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
function setDecodeHistoryOverlayVisible(visible, title = "", sub = "") {
  if (!decodeHistoryOverlayEl) return;
  if (title && decodeHistoryOverlayTitleEl) decodeHistoryOverlayTitleEl.textContent = title;
  if (decodeHistoryOverlaySubEl) decodeHistoryOverlaySubEl.textContent = sub || "";
  decodeHistoryOverlayEl.classList.toggle("is-hidden", !visible);
}

function setConnLostOverlay(visible, title = "Connection lost", sub = "Retrying\u2026", fullscreen = false) {
  if (!connLostOverlayEl) return;
  if (connLostOverlayTitleEl) connLostOverlayTitleEl.textContent = title;
  if (connLostOverlaySubEl) connLostOverlaySubEl.textContent = sub;
  connLostOverlayEl.classList.toggle("conn-lost-fullscreen", fullscreen);
  connLostOverlayEl.classList.toggle("is-hidden", !visible);
}
const decodeHistoryTextDecoder = typeof TextDecoder === "function" ? new TextDecoder() : null;
let decodeHistoryReplayActive = false;
let decodeMapSyncPending = false;

function markDecodeMapSyncPending() {
  decodeMapSyncPending = true;
}

function flushDeferredDecodeMapSync() {
  if (!decodeMapSyncPending || decodeHistoryReplayActive || !window.trx?.map?.aprsMap) return;
  decodeMapSyncPending = false;
  scheduleUiFrameJob("decode-map-maintenance", () => {
    window.trx.map?.pruneMapHistory();
  });
}

function setDecodeHistoryReplayActive(active) {
  decodeHistoryReplayActive = !!active;
  if (!decodeHistoryReplayActive) {
    flushDeferredDecodeMapSync();
  }
}

function decodeHistoryMapRenderingDeferred() {
  return decodeHistoryReplayActive || !window.trx?.map?.aprsMap;
}

function decodeCborUint(view, bytes, state, additional) {
  const offset = state.offset;
  if (additional < 24) return additional;
  if (additional === 24) {
    if (offset + 1 > bytes.length) throw new Error("CBOR payload truncated");
    state.offset += 1;
    return bytes[offset];
  }
  if (additional === 25) {
    if (offset + 2 > bytes.length) throw new Error("CBOR payload truncated");
    state.offset += 2;
    return view.getUint16(offset);
  }
  if (additional === 26) {
    if (offset + 4 > bytes.length) throw new Error("CBOR payload truncated");
    state.offset += 4;
    return view.getUint32(offset);
  }
  if (additional === 27) {
    if (offset + 8 > bytes.length) throw new Error("CBOR payload truncated");
    const value = view.getBigUint64(offset);
    state.offset += 8;
    const numeric = Number(value);
    if (!Number.isSafeInteger(numeric)) throw new Error("CBOR integer exceeds JS safe range");
    return numeric;
  }
  throw new Error("Unsupported CBOR additional info");
}

function decodeCborFloat16(bits) {
  const sign = (bits & 0x8000) ? -1 : 1;
  const exponent = (bits >> 10) & 0x1f;
  const fraction = bits & 0x03ff;
  if (exponent === 0) {
    return fraction === 0 ? sign * 0 : sign * Math.pow(2, -14) * (fraction / 1024);
  }
  if (exponent === 0x1f) {
    return fraction === 0 ? sign * Infinity : Number.NaN;
  }
  return sign * Math.pow(2, exponent - 15) * (1 + (fraction / 1024));
}

function decodeCborItem(view, bytes, state) {
  if (state.offset >= bytes.length) throw new Error("CBOR payload truncated");
  const initial = bytes[state.offset++];
  const major = initial >> 5;
  const additional = initial & 0x1f;
  if (major === 0) return decodeCborUint(view, bytes, state, additional);
  if (major === 1) return -1 - decodeCborUint(view, bytes, state, additional);
  if (major === 2) {
    const length = decodeCborUint(view, bytes, state, additional);
    if (state.offset + length > bytes.length) throw new Error("CBOR payload truncated");
    const chunk = bytes.slice(state.offset, state.offset + length);
    state.offset += length;
    return Array.from(chunk);
  }
  if (major === 3) {
    const length = decodeCborUint(view, bytes, state, additional);
    if (state.offset + length > bytes.length) throw new Error("CBOR payload truncated");
    const chunk = bytes.subarray(state.offset, state.offset + length);
    state.offset += length;
    return decodeHistoryTextDecoder ? decodeHistoryTextDecoder.decode(chunk) : String.fromCharCode(...chunk);
  }
  if (major === 4) {
    const length = decodeCborUint(view, bytes, state, additional);
    const items = new Array(length);
    for (let i = 0; i < length; i += 1) {
      items[i] = decodeCborItem(view, bytes, state);
    }
    return items;
  }
  if (major === 5) {
    const length = decodeCborUint(view, bytes, state, additional);
    const value = {};
    for (let i = 0; i < length; i += 1) {
      const key = decodeCborItem(view, bytes, state);
      value[String(key)] = decodeCborItem(view, bytes, state);
    }
    return value;
  }
  if (major === 6) {
    decodeCborUint(view, bytes, state, additional);
    return decodeCborItem(view, bytes, state);
  }
  if (major === 7) {
    if (additional === 20) return false;
    if (additional === 21) return true;
    if (additional === 22) return null;
    if (additional === 23) return undefined;
    if (additional === 25) {
      if (state.offset + 2 > bytes.length) throw new Error("CBOR payload truncated");
      const bits = view.getUint16(state.offset);
      state.offset += 2;
      return decodeCborFloat16(bits);
    }
    if (additional === 26) {
      if (state.offset + 4 > bytes.length) throw new Error("CBOR payload truncated");
      const value = view.getFloat32(state.offset);
      state.offset += 4;
      return value;
    }
    if (additional === 27) {
      if (state.offset + 8 > bytes.length) throw new Error("CBOR payload truncated");
      const value = view.getFloat64(state.offset);
      state.offset += 8;
      return value;
    }
  }
  throw new Error("Unsupported CBOR major type");
}

function decodeCborPayload(buffer) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const state = { offset: 0 };
  const value = decodeCborItem(view, bytes, state);
  if (state.offset !== bytes.length) {
    throw new Error("Unexpected trailing bytes in CBOR payload");
  }
  return value;
}

let lastSpectrumData = null;
window.lastSpectrumData = null;
let lastControl;
let lastTxEn = null;
let lastHasTx = true;
let lastRendered = null;
let prevRenderData = {};
let hintTimer = null;
let sigMeasuring = false;
let sigLastSUnits = null;
let sigLastDbm = null;
const SIG_STRENGTH_UNITS = ["dBFS", "dBf", "dBm", "S"];
let sigStrengthUnitIdx = loadSetting("sigStrengthUnit", 0);

function sigUnit(u) { return `<span class="sig-unit">${u}</span>`; }

function formatSigStrength(dbm) {
  if (!Number.isFinite(dbm)) return "--";
  const unit = SIG_STRENGTH_UNITS[sigStrengthUnitIdx] || "dBFS";
  if (unit === "S") return formatSignal(dbmToSUnits(dbm));
  if (unit === "dBm") return `${dbm.toFixed(1)} ${sigUnit("dBm")}`;
  if (unit === "dBf") {
    // dBf = dBm + 107 (referenced to 1 femtowatt across 50 Ω)
    const dbf = dbm + 107;
    return `${dbf.toFixed(1)} ${sigUnit("dBf")}`;
  }
  // dBFS: map receiver range to a full-scale reference
  // Typical receiver: -140 dBm (noise floor) to 0 dBm (full scale)
  const dbfs = Math.max(-140, Math.min(0, dbm));
  return `${dbfs.toFixed(1)} ${sigUnit("dBFS")}`;
}

function refreshSigStrengthDisplay() {
  if (!sigStrengthEl) return;
  sigStrengthEl.innerHTML = formatSigStrength(sigLastDbm);
}

if (sigStrengthEl) {
  sigStrengthEl.addEventListener("click", () => {
    sigStrengthUnitIdx = (sigStrengthUnitIdx + 1) % SIG_STRENGTH_UNITS.length;
    saveSetting("sigStrengthUnit", sigStrengthUnitIdx);
    refreshSigStrengthDisplay();
  });
}

let sigMeasureTimer = null;
let sigMeasureLastTickMs = 0;
let sigMeasureAccumMs = 0;
let sigMeasureWeighted = 0;
let sigMeasurePeak = null;
let lastFreqHz = null;
window.lastFreqHz = null;
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
let sdrSquelchSupported = false;
// ── Previous-state tracking for "B" hotkey ────────────────────────────────────
let previousTuneState = null; // { freqHz, bandwidthHz, mode, centerHz }

function savePreviousTuneState() {
  previousTuneState = {
    freqHz: lastFreqHz,
    bandwidthHz: currentBandwidthHz,
    mode: modeEl ? modeEl.value : "",
    centerHz: lastSpectrumData ? Number(lastSpectrumData.center_hz) : null,
  };
}

async function restorePreviousTuneState() {
  if (!previousTuneState) {
    showHint("No previous state", 1500);
    return;
  }
  const saved = previousTuneState;
  savePreviousTuneState(); // save current as previous so B toggles back
  if (saved.mode && modeEl && modeEl.value !== saved.mode) {
    modeEl.value = saved.mode;
    await postPath(`/set_mode?mode=${encodeURIComponent(saved.mode)}`);
    updateWfmControls();
  }
  if (Number.isFinite(saved.bandwidthHz) && saved.bandwidthHz !== currentBandwidthHz) {
    currentBandwidthHz = saved.bandwidthHz;
    window.currentBandwidthHz = currentBandwidthHz;
    syncBandwidthInput(currentBandwidthHz);
    await postPath(`/set_bandwidth?hz=${saved.bandwidthHz}`);
  }
  if (Number.isFinite(saved.freqHz)) {
    setRigFrequency(saved.freqHz);
  }
  if (Number.isFinite(saved.centerHz)) {
    await postPath(`/set_center_freq?hz=${saved.centerHz}`);
  }
  showHint("Restored previous", 1500);
}

let lastRigIds = [];
let lastRigDisplayNames = {};
let lastActiveRigId = null;
let lastCityLabel = "";
let sseSessionId = null;
const originalTitle = document.title;
const savedTheme = loadSetting("theme", null);

function currentTheme() {
  return document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark";
}

function updateDocumentTitle(rds = null) {
  const freqHz = activeChannelFreqHz();
  if (!Number.isFinite(freqHz)) {
    document.title = originalTitle;
    return;
  }
  const parts = [formatFreq(freqHz)];
  const ps = rds?.program_service;
  if (ps && ps.length > 0) {
    parts.push(ps);
  }
  const rigName = (lastActiveRigId && lastRigDisplayNames[lastActiveRigId]) || lastActiveRigId || "";
  if (rigName) parts.push(rigName);
  if (lastCityLabel) parts.push(lastCityLabel);
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
  if (typeof trxClearCssColorCache === 'function') trxClearCssColorCache();
  invalidateBookmarkColors();
}

// Recolour bookmark chips after any palette/theme change (setTheme or setStyle).
function invalidateBookmarkColors() {
  if (typeof bmOverlayRevision === "undefined") return;
  bmOverlayRevision++;
  // Force the browser to recalculate styles so getComputedStyle reads new values.
  void getComputedStyle(document.documentElement).getPropertyValue("--bg");
  const colorMap = bmCategoryColorMap();
  const ref = typeof bmOverlayList !== "undefined" ? bmOverlayList : [];
  document.querySelectorAll(".spectrum-bookmark-chip").forEach((chip) => {
    const bm = ref.find((b) => b.id === chip.dataset.bmId);
    if (!bm) return;
    const col = colorMap[bm.category || ""] || "#66d9ef";
    chip.style.setProperty("--bm-cat-bg", col);
    chip.style.setProperty("--bm-cat-fg", bmContrastFg(col));
  });
  // Clear cached DOM keys so the next spectrum draw rebuilds chips fresh.
  for (const id of ["spectrum-bookmark-axis", "spectrum-bookmark-side-left", "spectrum-bookmark-side-right"]) {
    const el = document.getElementById(id);
    if (el) el.dataset.bmKey = "";
  }
  try { if (typeof scheduleSpectrumDraw === "function") scheduleSpectrumDraw(); } catch (_) {}
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
  "neon-disco": {
    dark: {
      bg: "#090010",
      spectrumLine: "#ff10e0", spectrumFill: "rgba(255,16,224,0.12)",
      spectrumGrid: "rgba(255,16,224,0.10)", spectrumLabel: "rgba(240,200,255,0.55)",
      waveformLine: "rgba(57,255,20,0.92)", waveformPeak: "rgba(255,16,224,0.88)",
      waveformGrid: "rgba(255,16,224,0.10)", waveformLabel: "rgba(240,200,255,0.65)",
      waterfallHue: [300, 120], waterfallSat: 100, waterfallLight: [8, 55], waterfallAlpha: [0.30, 0.92],
    },
    light: {
      bg: "#f0d8ff",
      spectrumLine: "#cc00a8", spectrumFill: "rgba(204,0,168,0.12)",
      spectrumGrid: "rgba(100,0,150,0.10)", spectrumLabel: "rgba(50,0,80,0.55)",
      waveformLine: "rgba(31,136,0,0.95)", waveformPeak: "rgba(180,0,120,0.9)",
      waveformGrid: "rgba(50,0,80,0.10)", waveformLabel: "rgba(50,0,80,0.65)",
      waterfallHue: [300, 120], waterfallSat: 90, waterfallLight: [90, 45], waterfallAlpha: [0.35, 0.80],
    },
  },
  "golden-rain": {
    dark: {
      bg: "#120d07",
      spectrumLine: "#e4b24d", spectrumFill: "rgba(228,178,77,0.11)",
      spectrumGrid: "rgba(255,229,172,0.07)", spectrumLabel: "rgba(230,205,152,0.54)",
      waveformLine: "rgba(236,199,108,0.92)", waveformPeak: "rgba(214,134,44,0.90)",
      waveformGrid: "rgba(255,210,120,0.09)", waveformLabel: "rgba(232,214,174,0.66)",
      waterfallHue: [40, 18], waterfallSat: 88, waterfallLight: [8, 58], waterfallAlpha: [0.26, 0.84],
    },
    light: {
      bg: "#f5ecd9",
      spectrumLine: "#9e6700", spectrumFill: "rgba(158,103,0,0.12)",
      spectrumGrid: "rgba(82,55,14,0.09)", spectrumLabel: "rgba(82,55,14,0.55)",
      waveformLine: "rgba(140,92,0,0.94)", waveformPeak: "rgba(191,86,0,0.90)",
      waveformGrid: "rgba(82,55,14,0.11)", waveformLabel: "rgba(82,55,14,0.66)",
      waterfallHue: [45, 18], waterfallSat: 86, waterfallLight: [92, 42], waterfallAlpha: [0.34, 0.82],
    },
  },
  amber: {
    dark: {
      bg: "#130706",
      spectrumLine: "#ff7a1f", spectrumFill: "rgba(255,122,31,0.14)",
      spectrumGrid: "rgba(255,110,40,0.09)", spectrumLabel: "rgba(255,202,164,0.54)",
      waveformLine: "rgba(255,134,54,0.94)", waveformPeak: "rgba(255,220,96,0.92)",
      waveformGrid: "rgba(255,120,36,0.11)", waveformLabel: "rgba(255,214,176,0.66)",
      waterfallHue: [8, 42], waterfallSat: 96, waterfallLight: [8, 58], waterfallAlpha: [0.26, 0.88],
    },
    light: {
      bg: "#fff2e7",
      spectrumLine: "#c24500", spectrumFill: "rgba(194,69,0,0.14)",
      spectrumGrid: "rgba(125,52,0,0.09)", spectrumLabel: "rgba(90,38,0,0.56)",
      waveformLine: "rgba(176,62,0,0.95)", waveformPeak: "rgba(224,132,0,0.90)",
      waveformGrid: "rgba(125,52,0,0.10)", waveformLabel: "rgba(90,38,0,0.68)",
      waterfallHue: [18, 48], waterfallSat: 90, waterfallLight: [92, 42], waterfallAlpha: [0.34, 0.84],
    },
  },
  fire: {
    dark: {
      bg: "#140406",
      spectrumLine: "#cf1b22", spectrumFill: "rgba(207,27,34,0.14)",
      spectrumGrid: "rgba(255,84,60,0.08)", spectrumLabel: "rgba(255,214,202,0.54)",
      waveformLine: "rgba(222,46,34,0.94)", waveformPeak: "rgba(255,112,48,0.90)",
      waveformGrid: "rgba(255,84,60,0.10)", waveformLabel: "rgba(255,226,214,0.66)",
      waterfallHue: [2, 18], waterfallSat: 96, waterfallLight: [8, 52], waterfallAlpha: [0.26, 0.88],
    },
    light: {
      bg: "#ffede5",
      spectrumLine: "#a91511", spectrumFill: "rgba(169,21,17,0.14)",
      spectrumGrid: "rgba(125,36,12,0.09)", spectrumLabel: "rgba(92,24,10,0.56)",
      waveformLine: "rgba(164,28,16,0.95)", waveformPeak: "rgba(214,88,20,0.90)",
      waveformGrid: "rgba(125,36,12,0.10)", waveformLabel: "rgba(92,24,10,0.68)",
      waterfallHue: [4, 24], waterfallSat: 82, waterfallLight: [92, 40], waterfallAlpha: [0.34, 0.84],
    },
  },
  phosphor: {
    dark: {
      bg: "#010501",
      spectrumLine: "#39ff14", spectrumFill: "rgba(57,255,20,0.13)",
      spectrumGrid: "rgba(57,255,20,0.07)", spectrumLabel: "rgba(168,230,168,0.55)",
      waveformLine: "rgba(57,255,20,0.92)", waveformPeak: "rgba(184,240,96,0.88)",
      waveformGrid: "rgba(57,255,20,0.08)", waveformLabel: "rgba(168,230,168,0.65)",
      waterfallHue: [115, 90], waterfallSat: 100, waterfallLight: [5, 52], waterfallAlpha: [0.28, 0.92],
    },
    light: {
      bg: "#e0f0e0",
      spectrumLine: "#1a7a1a", spectrumFill: "rgba(26,122,26,0.13)",
      spectrumGrid: "rgba(10,42,10,0.08)", spectrumLabel: "rgba(10,42,10,0.52)",
      waveformLine: "rgba(20,110,20,0.95)", waveformPeak: "rgba(74,138,0,0.90)",
      waveformGrid: "rgba(10,42,10,0.10)", waveformLabel: "rgba(10,42,10,0.65)",
      waterfallHue: [115, 90], waterfallSat: 90, waterfallLight: [92, 40], waterfallAlpha: [0.34, 0.82],
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
  const remapped =
    style === "nord" ? "arctic"
    : style === "monokai" ? "lime"
    : style === "blood" ? "fire"
    : style;
  const valid = ["original", "arctic", "lime", "contrast", "neon-disco", "golden-rain", "amber", "fire", "phosphor"];
  const next = valid.includes(remapped) ? remapped : "original";
  if (next === "original") {
    document.documentElement.removeAttribute("data-style");
  } else {
    document.documentElement.setAttribute("data-style", next);
  }
  saveSetting("style", next);
  if (headerStylePickSelect) headerStylePickSelect.value = next;
  if (typeof trxClearCssColorCache === 'function') trxClearCssColorCache();
  invalidateBookmarkColors();
  scheduleOverviewDraw();
}

if (overviewPeakHoldEl) {
  if (!Number.isFinite(overviewPeakHoldMs) || overviewPeakHoldMs < 0) {
    overviewPeakHoldMs = 2000;
  }
  overviewPeakHoldEl.value = String(overviewPeakHoldMs);
  overviewPeakHoldEl.addEventListener("change", () => {
    overviewPeakHoldMs = Math.max(0, Number(overviewPeakHoldEl.value) || 0);
    saveSetting("overviewPeakHoldMs", overviewPeakHoldMs);
    pruneSpectrumPeakHoldFrames();
    if (lastSpectrumData) scheduleSpectrumDraw();
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
    syncLocatorMarkerStyles();
    refreshAisMarkerColors();
    scheduleOverviewDraw();
    if (typeof scheduleSpectrumDraw === "function" && lastSpectrumData) scheduleSpectrumDraw();
  });
}

if (headerStylePickSelect) {
  headerStylePickSelect.addEventListener("change", () => {
    setStyle(headerStylePickSelect.value);
    updateMapBaseLayerForTheme(currentTheme());
    syncLocatorMarkerStyles();
    refreshAisMarkerColors();
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
  selectEl.replaceChildren();
  rigIds.forEach((id) => {
    const opt = document.createElement("option");
    opt.value = id;
    opt.textContent = lastRigDisplayNames[id] || id;
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
  updateDocumentTitle(activeChannelRds());
}

function applyRigList(activeRigId, rigIds, displayNames) {
  if (!Array.isArray(rigIds)) return;
  const nextIds = rigIds.filter((id) => typeof id === "string" && id.length > 0);
  // Detect whether the rig list or active rig actually changed so we can
  // skip expensive bookmark re-fetches on every SSE state update.
  const prevKey = lastRigIds.join("\0") + "|" + (lastActiveRigId || "");
  lastRigIds = nextIds;
  if (displayNames && typeof displayNames === "object") {
    lastRigDisplayNames = { ...displayNames };
  }
  const aboutList = document.getElementById("about-rig-list");
  if (aboutList) {
    aboutList.textContent = lastRigIds.length ? lastRigIds.join(", ") : "--";
  }
  if (typeof activeRigId === "string" && activeRigId.length > 0) {
    // Only adopt the server's active rig when this tab has no selection yet
    // (first load). Otherwise keep the per-tab choice so other tabs' switches
    // do not override ours.
    if (!lastActiveRigId) {
      lastActiveRigId = activeRigId;
    }
    const aboutActive = document.getElementById("about-active-rig");
    if (aboutActive) aboutActive.textContent = lastActiveRigId;
  }
  const nextKey = lastRigIds.join("\0") + "|" + (lastActiveRigId || "");
  const rigListChanged = prevKey !== nextKey;
  const disableSwitch = lastRigIds.length === 0 || !authRole || authRole === "rx";
  populateRigPicker(headerRigSwitchSelect, lastRigIds, lastActiveRigId, disableSwitch);
  updateRigSubtitle(lastActiveRigId);
  if (rigListChanged) {
    if (typeof setSchedulerRig === "function") setSchedulerRig(lastActiveRigId);
    if (typeof setBackgroundDecodeRig === "function") setBackgroundDecodeRig(lastActiveRigId);
    if (typeof bmPopulateScopePicker === "function") bmPopulateScopePicker();
    if (typeof bmFetch === "function") bmFetch(document.getElementById("bm-category-filter")?.value || "");
  }
  window.trx.map?.updateMapRigFilter();
}


async function refreshRigList() {
  try {
    const resp = await fetch("/rigs", { cache: "no-store" });
    if (!resp.ok) return;
    const data = await resp.json();
    const rigs = Array.isArray(data.rigs) ? data.rigs : [];
    const rigIds = rigs.map((r) => r && r.remote).filter(Boolean);
    const displayNames = {};
    rigs.forEach((r) => {
      if (!r || !r.remote) return;
      if (typeof r.display_name === "string" && r.display_name.length > 0) {
        displayNames[r.remote] = r.display_name;
      } else {
        const mfg = (r.manufacturer || "").trim();
        const mdl = (r.model || "").trim();
        const hw = [mfg, mdl].filter(Boolean).join(" ");
        displayNames[r.remote] = hw || r.remote;
      }
    });
    serverRigs = rigs;
    serverActiveRigId = data.active_remote || null;
    applyRigList(data.active_remote, rigIds, displayNames);
    window.trx.map?.syncAprsReceiverMarker();
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
let lastUnsupportedFreqPopupAt = 0;
let freqDirty = false;
let initialized = false;
let lastEventAt = Date.now();
let aboutUptimeStart = null;
let es;
let esHeartbeat;

function formatUptime(ms) {
  const s = Math.floor(ms / 1000);
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const parts = [];
  if (d > 0) parts.push(`${d}d`);
  if (h > 0 || d > 0) parts.push(`${h}h`);
  parts.push(`${m}m`);
  parts.push(`${sec}s`);
  return parts.join(" ");
}
setInterval(() => {
  if (!aboutUptimeStart) return;
  const el = document.getElementById("about-uptime");
  if (el) el.textContent = formatUptime(Date.now() - aboutUptimeStart);
}, 1000);
let reconnectTimer = null;
let overviewSignalSamples = [];
let overviewSignalTimer = null;
let overviewWaterfallRows = [];
let overviewWaterfallPushCount = 0;   // monotonically increments on every push
const HEADER_SIG_WINDOW_MS = 10_000;
const OVERVIEW_WF_TEX_MAX_W = 512;
let overviewWfTexData = null;
let overviewWfTexWidth = 0;
let overviewWfTexHeight = 0;
let overviewWfTexPushCount = 0;
let overviewWfTexPalKey = "";
let overviewWfTexReady = false;

function cssColorToRgba(color, alphaMul = 1) {
  const parser = typeof window.trxParseCssColor === "function" ? window.trxParseCssColor : null;
  const parsed = parser ? parser(color) : [0, 0, 0, 1];
  return [
    parsed[0],
    parsed[1],
    parsed[2],
    Math.max(0, Math.min(1, parsed[3] * alphaMul)),
  ];
}

function rgbaWithAlpha(color, alphaMul = 1) {
  return cssColorToRgba(color, alphaMul);
}

const BW_OVERLAY_COLORS = {
  soft: [240 / 255, 173 / 255, 78 / 255, 0.05],
  mid: [240 / 255, 173 / 255, 78 / 255, 0.19],
  edge: [240 / 255, 173 / 255, 78 / 255, 0.30],
  stroke: [240 / 255, 173 / 255, 78 / 255, 0.70],
  hard: [240 / 255, 173 / 255, 78 / 255, 0.38],
};

const BOOKMARK_MARKER_FALLBACK = "#66d9ef";

function overviewWfResetTextureCache() {
  overviewWfTexData = null;
  overviewWfTexWidth = 0;
  overviewWfTexHeight = 0;
  overviewWfTexPushCount = 0;
  overviewWfTexPalKey = "";
  overviewWfTexReady = false;
}

function overviewWfPaletteKey(pal, viewKey = "") {
  return `${pal.waterfallHue}|${pal.waterfallSat}|${pal.waterfallLight}|${pal.waterfallAlpha}|${spectrumFloor}|${spectrumRange}|${waterfallGamma}|${viewKey}`;
}

function resizeHeaderSignalCanvas() {
  if (!ensureOverviewCanvasBackingStore()) return;
  positionRdsPsOverlay();
  drawHeaderSignalGraph();
}

function ensureOverviewCanvasBackingStore() {
  if (!overviewCanvas || !overviewGl || !overviewGl.ready) return false;
  const cssW = Math.floor(overviewCanvas.clientWidth);
  const cssH = Math.floor(overviewCanvas.clientHeight);
  if (cssW <= 0 || cssH <= 0) return false;
  const dpr = window.devicePixelRatio || 1;
  const resized = overviewGl.ensureSize(cssW, cssH, dpr);
  if (resized) {
    overviewWfResetTextureCache();
    trimOverviewWaterfallRows();
  }
  return true;
}

function signalOverlayHeight() {
  if (!overviewCanvas) return 0;
  let height = overviewCanvas.clientHeight || 0;
  // Include the bandplan strip height when it sits above the overview.
  if (bandplanStripEl && bandplanStripEl.classList.contains("bp-visible")) {
    height += bandplanStripEl.clientHeight || 0;
  }
  const spectrumCanvasEl = document.getElementById("spectrum-canvas");
  const spectrumPanelEl = document.getElementById("spectrum-panel");
  const spectrumVisible =
    spectrumCanvasEl &&
    spectrumCanvasEl.clientHeight > 0 &&
    spectrumPanelEl &&
    getComputedStyle(spectrumPanelEl).display !== "none";
  if (spectrumVisible) {
    height += spectrumCanvasEl.clientHeight || 0;
    const wfCanvas = document.getElementById("spectrum-waterfall-canvas");
    if (wfCanvas && wfCanvas.clientHeight > 0) {
      height += wfCanvas.clientHeight;
    }
  }
  return Math.floor(height);
}

function drawSignalOverlay() {
  if (!signalOverlayCanvas || !signalVisualBlockEl || !signalOverlayGl || !signalOverlayGl.ready) return;
  if (!lastSpectrumData) {
    signalOverlayCanvas.style.height = "0";
    signalOverlayCanvas.width = 0;
    signalOverlayCanvas.height = 0;
    return;
  }
  const cssW = Math.floor(signalVisualBlockEl.clientWidth);
  const cssH = signalOverlayHeight();
  signalOverlayCanvas.style.height = cssH > 0 ? `${cssH}px` : "0";
  if (cssW <= 0 || cssH <= 0) {
    signalOverlayCanvas.width = 0;
    signalOverlayCanvas.height = 0;
    return;
  }

  const dpr = window.devicePixelRatio || 1;
  signalOverlayGl.ensureSize(cssW, cssH, dpr);
  const W = signalOverlayCanvas.width;
  const H = signalOverlayCanvas.height;
  if (W <= 0 || H <= 0) return;
  signalOverlayGl.clear([0, 0, 0, 0]);

  const range = spectrumVisibleRange(lastSpectrumData);
  const hzToX = (hz) => ((hz - range.visLoHz) / range.visSpanHz) * W;
  const bwSoft = BW_OVERLAY_COLORS.soft;
  const bwMid = BW_OVERLAY_COLORS.mid;
  const bwEdge = BW_OVERLAY_COLORS.edge;
  const bwStroke = BW_OVERLAY_COLORS.stroke;
  const bwHard = BW_OVERLAY_COLORS.hard;
  const bmRef = typeof bmOverlayList !== "undefined" ? bmOverlayList : null;
  if (Array.isArray(bmRef) && bmRef.length > 0) {
    const colorMap = bmCategoryColorMap();
    const grouped = new Map();
    for (const bm of bmRef) {
      const f = Number(bm?.freq_hz);
      if (!Number.isFinite(f) || f < range.visLoHz || f > range.visHiHz) continue;
      if (Number.isFinite(lastFreqHz) && Math.abs(f - lastFreqHz) <= Math.max(minFreqStepHz, 5)) continue;
      const x = hzToX(f);
      if (!Number.isFinite(x) || x < 0 || x > W) continue;
      const color = colorMap[bm?.category || ""] || BOOKMARK_MARKER_FALLBACK;
      if (!grouped.has(color)) grouped.set(color, []);
      grouped.get(color).push(x, 0, x, H);
    }
    for (const [color, segments] of grouped.entries()) {
      if (!Array.isArray(segments) || segments.length === 0) continue;
      signalOverlayGl.drawSegments(segments, rgbaWithAlpha(color, 0.72), Math.max(1, dpr * 0.9));
    }
  }

  const _bwCenterHz = activeBandwidthCenterHz();
  if (_bwCenterHz != null && currentBandwidthHz > 0) {
    for (const spec of visibleBandwidthSpecs(_bwCenterHz)) {
      const span = displaySpanForBandwidthSpec(spec);
      const xL = hzToX(span.loHz);
      const xR = hzToX(span.hiHz);
      const stripW = xR - xL;
      if (stripW <= 1) continue;
      if (span.side < 0) {
        signalOverlayGl.fillGradientRect(xL, 0, stripW, H, bwSoft, bwMid, bwMid, bwSoft);
      } else if (span.side > 0) {
        signalOverlayGl.fillGradientRect(xL, 0, stripW, H, bwMid, bwSoft, bwSoft, bwMid);
      } else {
        const half = stripW / 2;
        signalOverlayGl.fillGradientRect(xL, 0, half, H, bwSoft, bwMid, bwMid, bwSoft);
        signalOverlayGl.fillGradientRect(xL + half, 0, half, H, bwMid, bwSoft, bwSoft, bwMid);
      }

      const edgeW = Math.max(1, Math.round(5 * dpr));
      if (span.side <= 0) {
        signalOverlayGl.fillRect(xL, 0, edgeW, H, bwEdge);
      }
      if (span.side >= 0) {
        signalOverlayGl.fillRect(xR - edgeW, 0, edgeW, H, bwEdge);
      }

      if (span.side <= 0) {
        signalOverlayGl.drawSegments([xL, 0, xL, H], bwStroke, Math.max(1, dpr * 1.5));
      }
      if (span.side >= 0) {
        signalOverlayGl.drawSegments([xR, 0, xR, H], bwStroke, Math.max(1, dpr * 1.5));
      }
      if (span.side !== 0) {
        const hardX = span.side < 0 ? xR : xL;
        signalOverlayGl.drawSegments([hardX, 0, hardX, H], bwHard, Math.max(1, dpr));
      }
    }
  }

  // Virtual channel markers (sky-blue dashed lines, active one is solid).
  if (typeof vchanChannels !== "undefined" && Array.isArray(vchanChannels)) {
    vchanChannels.forEach(ch => {
      if (!Number.isFinite(ch.freq_hz) || ch.freq_hz <= 0) return;
      const xc = hzToX(ch.freq_hz);
      if (xc < 0 || xc > W) return;
      const isActive = ch.id === vchanActiveId;
      const color = cssColorToRgba("#38bdf8");
      if (isActive) {
        signalOverlayGl.drawSegments([xc, 0, xc, H], color, Math.max(1.5, dpr * 1.5));
      } else {
        signalOverlayGl.drawDashedVerticalLine(
          xc, 0, H,
          Math.max(2, Math.round(4 * dpr)),
          Math.max(3, Math.round(6 * dpr)),
          color,
          Math.max(1, dpr),
        );
      }
    });
  }

  if (lastFreqHz != null) {
    const xf = hzToX(lastFreqHz);
    if (xf >= 0 && xf <= W) {
      signalOverlayGl.drawDashedVerticalLine(
        xf,
        0,
        H,
        Math.max(2, Math.round(4 * dpr)),
        Math.max(2, Math.round(4 * dpr)),
        cssColorToRgba("#ff1744"),
        Math.max(1, dpr),
      );
    }
  }
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
  const maxRows = Math.max(1, Math.floor(overviewCanvas.height / _cachedDpr));
  if (overviewWaterfallRows.length > maxRows) {
    overviewWaterfallRows.splice(0, overviewWaterfallRows.length - maxRows);
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
  if (!overviewCanvas || !data || !isBinsArray(data.bins) || data.bins.length === 0) return;
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
  if (!ensureOverviewCanvasBackingStore()) return;
  if (!overviewGl || !overviewGl.ready) return;
  const pal = canvasPalette();
  const W = overviewCanvas.width;
  const H = overviewCanvas.height;
  if (W <= 0 || H <= 0) return;

  overviewGl.clear(cssColorToRgba(pal.bg));
  if (lastSpectrumData && overviewWaterfallRows.length > 0) {
    drawOverviewWaterfall(W, H, pal);
  } else {
    drawOverviewSignalHistory(W, H, pal);
  }
  positionRdsPsOverlay();
  drawSignalOverlay();
  updateBandplanStrip(bandplanComputeRange());
}

function drawOverviewWaterfall(W, H, pal) {
  if (!overviewGl || !overviewGl.ready) return;
  const maxVisible = Math.max(1, Math.floor(H));
  const rows = overviewWaterfallRows.slice(-maxVisible);
  if (rows.length === 0) return;

  const iW = Math.max(96, Math.min(OVERVIEW_WF_TEX_MAX_W, Math.ceil(W / 2)));
  const iH = Math.max(1, rows.length);
  const minDb = Number.isFinite(spectrumFloor) ? spectrumFloor : -115;
  const maxDb = minDb + Math.max(20, Number.isFinite(spectrumRange) ? spectrumRange : 90);
  const view = lastSpectrumData ? spectrumVisibleRange(lastSpectrumData) : null;
  const viewKey = view ? `${Math.round(view.visLoHz)}:${Math.round(view.visHiHz)}` : "na";
  const palKey = overviewWfPaletteKey(pal, viewKey);
  const rowStride = iW * 4;
  const expectedSize = iW * iH * 4;
  const newPushes = overviewWaterfallPushCount - overviewWfTexPushCount;
  const sizeChanged = overviewWfTexWidth !== iW || overviewWfTexHeight !== iH;
  const palChanged = overviewWfTexPalKey !== palKey;
  const needsFull = !overviewWfTexData || sizeChanged || palChanged || overviewWfTexPushCount === 0;
  let texUpdated = false;

  if (!overviewWfTexData || overviewWfTexData.length !== expectedSize) {
    overviewWfTexData = new Uint8Array(expectedSize);
  }
  overviewWfTexWidth = iW;
  overviewWfTexHeight = iH;

  ensureWaterfallLut(pal, minDb, maxDb);

  function renderRow(dstY, srcBins) {
    if (!isBinsArray(srcBins) || srcBins.length === 0) return;
    const { startIdx, endIdx } = overviewVisibleBinWindow(lastSpectrumData, srcBins.length);
    const spanBins = Math.max(1, endIdx - startIdx);
    const rowBase = dstY * rowStride;
    const iwM1 = Math.max(1, iW - 1);
    for (let x = 0; x < iW; x++) {
      const binIdx = Math.min(endIdx, startIdx + ((x * spanBins / iwM1) | 0));
      waterfallLutWrite(overviewWfTexData, rowBase + x * 4, srcBins[binIdx]);
    }
  }

  if (needsFull) {
    for (let y = 0; y < iH; y++) {
      renderRow(y, rows[y]);
    }
    overviewWfTexPushCount = overviewWaterfallPushCount;
    overviewWfTexPalKey = palKey;
    texUpdated = true;
  } else if (newPushes > 0) {
    const newCount = Math.min(newPushes, iH);
    if (newCount >= iH) {
      for (let y = 0; y < iH; y++) renderRow(y, rows[y]);
    } else {
      const shiftBytes = newCount * rowStride;
      overviewWfTexData.copyWithin(0, shiftBytes);
      const startRow = iH - newCount;
      for (let y = startRow; y < iH; y++) {
        renderRow(y, rows[y]);
      }
    }
    overviewWfTexPushCount = overviewWaterfallPushCount;
    overviewWfTexPalKey = palKey;
    texUpdated = true;
  }

  if (texUpdated || !overviewWfTexReady) {
    overviewGl.uploadRgbaTexture("overview-waterfall", iW, iH, overviewWfTexData, "linear");
    overviewWfTexReady = true;
  }
  overviewGl.drawTexture("overview-waterfall", 0, 0, W, H, 1, true);
}

function drawOverviewSignalHistory(W, H, pal) {
  if (!overviewGl || !overviewGl.ready) return;
  const now = Date.now();
  const samples = overviewSignalSamples.filter((sample) => now - sample.t <= HEADER_SIG_WINDOW_MS);
  if (samples.length === 0) return;

  const maxVal = 20;
  const windowStart = now - HEADER_SIG_WINDOW_MS;
  const toX = (t) => ((t - windowStart) / HEADER_SIG_WINDOW_MS) * W;
  const toY = (v) => H - (Math.max(0, Math.min(maxVal, v)) / maxVal) * (H - 3) - 1.5;

  const gridMarkers = [
    { val: 0 },
    { val: 9 },
    { val: 18 },
  ];
  const gridSegments = [];
  for (const marker of gridMarkers) {
    const y = toY(marker.val);
    gridSegments.push(0, y, W, y);
  }
  overviewGl.drawSegments(gridSegments, cssColorToRgba(pal.waveformGrid), 1);

  const linePoints = [];
  samples.forEach((sample, idx) => {
    const x = toX(sample.t);
    const y = toY(sample.v);
    if (idx === 0 || x >= linePoints[linePoints.length - 2]) {
      linePoints.push(x, y);
    }
  });
  overviewGl.drawPolyline(linePoints, cssColorToRgba(pal.waveformLine), 1.6);

  const holdMs = Math.max(0, Number.isFinite(overviewPeakHoldMs) ? overviewPeakHoldMs : 0);
  if (holdMs > 0) {
    const holdPoints = [];
    for (let i = 0; i < samples.length; i++) {
      let peak = samples[i].v;
      for (let j = i; j >= 0; j--) {
        if (samples[i].t - samples[j].t > holdMs) break;
        if (samples[j].v > peak) peak = samples[j].v;
      }
      const x = toX(samples[i].t);
      const y = toY(peak);
      if (i === 0 || x >= holdPoints[holdPoints.length - 2]) {
        holdPoints.push(x, y);
      }
    }
    overviewGl.drawPolyline(holdPoints, cssColorToRgba(pal.waveformPeak), 1);
  }
}

function waterfallColorRgba(db, pal, minDb, maxDb) {
  const lo = Number.isFinite(minDb) ? minDb : (Number.isFinite(spectrumFloor) ? spectrumFloor : -115);
  const hi = Number.isFinite(maxDb) ? maxDb : (lo + Math.max(20, Number.isFinite(spectrumRange) ? spectrumRange : 90));
  const safeDb = Number.isFinite(db) ? db : lo;
  const clamped = Math.max(lo, Math.min(hi, safeDb));
  const span = Math.max(1, hi - lo);
  const tLinear = (clamped - lo) / span;
  const t = waterfallGamma === 1.0 ? tLinear : Math.pow(tLinear, waterfallGamma);
  const hue = pal.waterfallHue[0] + t * (pal.waterfallHue[1] - pal.waterfallHue[0]);
  const light = pal.waterfallLight[0] + t * (pal.waterfallLight[1] - pal.waterfallLight[0]);
  const alpha = pal.waterfallAlpha[0] + t * (pal.waterfallAlpha[1] - pal.waterfallAlpha[0]);
  if (typeof window.trxHslToRgba === "function") {
    return window.trxHslToRgba(hue, pal.waterfallSat, light, alpha);
  }
  return cssColorToRgba(`hsla(${hue}, ${pal.waterfallSat}%, ${light}%, ${alpha})`);
}

// 256-entry waterfall color lookup table (bins are i8 = 256 possible values).
// Eliminates per-pixel HSL→RGBA computation in the waterfall rendering hot path.
let _wfLutKey = "";
const _wfLut = new Uint8Array(256 * 4); // [r,g,b,a] × 256 entries, 0-255 range

function ensureWaterfallLut(pal, minDb, maxDb) {
  const key = `${pal.waterfallHue}|${pal.waterfallSat}|${pal.waterfallLight}|${pal.waterfallAlpha}|${minDb}|${maxDb}|${waterfallGamma}`;
  if (key === _wfLutKey) return;
  _wfLutKey = key;
  for (let i = 0; i < 256; i++) {
    // i8 range: -128 to 127 (dB values in the spectrum)
    const db = i < 128 ? i : i - 256;
    const c = waterfallColorRgba(db, pal, minDb, maxDb);
    const p = i * 4;
    _wfLut[p + 0] = (c[0] * 255 + 0.5) | 0;
    _wfLut[p + 1] = (c[1] * 255 + 0.5) | 0;
    _wfLut[p + 2] = (c[2] * 255 + 0.5) | 0;
    _wfLut[p + 3] = (c[3] * 255 + 0.5) | 0;
  }
}

// Fast waterfall pixel write using LUT. `db` is the raw i8 bin value.
function waterfallLutWrite(texData, offset, db) {
  // Convert signed i8 to 0-255 LUT index
  const idx = ((db | 0) + 256) & 0xFF;
  const p = idx * 4;
  texData[offset]     = _wfLut[p];
  texData[offset + 1] = _wfLut[p + 1];
  texData[offset + 2] = _wfLut[p + 2];
  texData[offset + 3] = _wfLut[p + 3];
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

function activeRdsChannelId() {
  if (typeof vchanActiveId !== "undefined" && vchanActiveId) return vchanActiveId;
  return null;
}

function activeChannelRds() {
  if (!activeChannelIsWfm()) return null;
  const activeId = activeRdsChannelId();
  if (activeId) {
    const rds = vchanRdsById.get(activeId);
    if (rds) return rds;
    if (typeof vchanChannels !== "undefined" && Array.isArray(vchanChannels) && vchanChannels.length > 0) {
      if (vchanChannels[0].id === activeId) return primaryRds;
    }
  }
  return primaryRds;
}

function activeChannelIsWfm() {
  if (typeof vchanChannels !== "undefined" && Array.isArray(vchanChannels) && vchanChannels.length > 0) {
    const activeId = activeRdsChannelId();
    const active = vchanChannels.find((ch) => ch.id === activeId) || vchanChannels[0];
    return String(active?.mode || "").toUpperCase() === "WFM";
  }
  return lastModeName === "WFM";
}

function activeChannelFreqHz() {
  if (typeof vchanActiveChannel === "function") {
    const ch = vchanActiveChannel();
    if (Number.isFinite(ch?.freq_hz)) return ch.freq_hz;
  }
  return lastFreqHz;
}

function activeBandwidthCenterHz() {
  const freqHz = activeChannelFreqHz();
  return Number.isFinite(freqHz) ? freqHz : lastFreqHz;
}

function buildRdsOverlayHtml(rds) {
  const ps = rds?.program_service;
  const hasPs = !!(ps && ps.length > 0);
  const hasPi = rds?.pi != null;
  if (!hasPs && !hasPi) return "";
  const mainText = hasPs ? formatOverlayPs(ps) : formatOverlayPi(rds?.pi);
  const mainClass = hasPs ? "rds-ps-main" : "rds-ps-fallback";
  const metaText = hasPs
    ? `${formatOverlayPi(rds?.pi)} · ${formatOverlayPty(rds?.pty, rds?.pty_name)}`
    : (rds?.pty_name ?? (rds?.pty != null ? String(rds.pty) : ""));
  const trafficFlags =
    `<span class="rds-ps-flags">` +
    `${overlayTrafficFlagHtml("TP", rds?.traffic_program)}` +
    `${overlayTrafficFlagHtml("TA", rds?.traffic_announcement)}` +
    `</span>`;
  return (
    `<span class="${mainClass}">${hasPs ? formatPsHtml(ps) : escapeMapHtml(mainText)}</span>` +
    `<span class="rds-ps-meta">` +
    `<span class="rds-ps-meta-text">${escapeMapHtml(metaText)}</span>` +
    `${trafficFlags}` +
    `</span>`
  );
}

function collectRdsOverlayEntries() {
  const entries = [];
  if (typeof vchanChannels !== "undefined" && Array.isArray(vchanChannels) && vchanChannels.length > 0) {
    for (const ch of vchanChannels) {
      if (String(ch?.mode || "").toUpperCase() !== "WFM") continue;
      if (!Number.isFinite(ch?.freq_hz)) continue;
      const rds = vchanRdsById.get(ch.id) || (vchanChannels[0].id === ch.id ? primaryRds : null);
      if (!rds) continue;
      entries.push({ id: ch.id, freq_hz: ch.freq_hz, rds });
    }
  } else if (lastModeName === "WFM" && primaryRds && Number.isFinite(lastFreqHz)) {
    entries.push({ id: "primary", freq_hz: lastFreqHz, rds: primaryRds });
  }
  return entries;
}

function renderRdsOverlays() {
  if (!rdsPsOverlay) return;
  if (!lastSpectrumData || !overviewCanvas) {
    rdsOverlayEntries = [];
    rdsPsOverlay.style.display = "none";
    return;
  }
  const entries = collectRdsOverlayEntries();
  rdsOverlayEntries = [];
  rdsPsOverlay.replaceChildren();
  if (entries.length === 0) {
    rdsPsOverlay.style.display = "none";
    return;
  }
  entries.forEach((entry) => {
    const html = buildRdsOverlayHtml(entry.rds);
    if (!html) return;
    const el = document.createElement("div");
    el.className = "rds-ps-overlay-item";
    el.dataset.freqHz = String(entry.freq_hz);
    el.innerHTML = html;
    el.addEventListener("click", (evt) => {
      evt.stopPropagation();
      copyRdsPsToClipboard(entry.rds, entry.freq_hz);
    });
    el.addEventListener("mouseenter", () => {
      el.style.zIndex = String(entries.length + 10);
    });
    el.addEventListener("mouseleave", () => {
      if (el.dataset.defaultZ) el.style.zIndex = el.dataset.defaultZ;
    });
    rdsPsOverlay.appendChild(el);
    rdsOverlayEntries.push({ ...entry, el });
  });
  if (rdsOverlayEntries.length === 0) {
    rdsPsOverlay.style.display = "none";
    return;
  }
  rdsPsOverlay.style.display = "block";
  positionRdsOverlays();
}

window.renderRdsOverlays = renderRdsOverlays;

function positionRdsOverlays() {
  if (!rdsPsOverlay || !lastSpectrumData || !overviewCanvas || rdsOverlayEntries.length === 0) return;
  const width = overviewCanvas.clientWidth || overviewCanvas.width || 0;
  if (width <= 0) return;
  const range = spectrumVisibleRange(lastSpectrumData);
  if (!Number.isFinite(range.visLoHz) || !Number.isFinite(range.visSpanHz) || range.visSpanHz <= 0) return;
  // Assign z-indices: sort by frequency ascending so higher-frequency layers
  // sit on top of lower-frequency ones in the default (non-hover) state.
  const sortedByFreq = [...rdsOverlayEntries].sort((a, b) => a.freq_hz - b.freq_hz);
  const freqZMap = new Map(sortedByFreq.map((e, i) => [e.id, i + 1]));
  rdsOverlayEntries.forEach((entry, idx) => {
    const el = entry.el;
    if (!el) return;
    if (!Number.isFinite(entry.freq_hz)) {
      el.style.display = "none";
      return;
    }
    el.style.display = "";
    const rel = (entry.freq_hz - range.visLoHz) / range.visSpanHz;
    const clamped = Math.max(0.06, Math.min(0.94, rel));
    el.style.left = `${clamped * width}px`;
    el.style.top = "50%";
    const z = String(freqZMap.get(entry.id) ?? (idx + 1));
    el.style.zIndex = z;
    el.dataset.defaultZ = z;
  });
}

function positionRdsPsOverlay() {
  positionRdsOverlays();
}

function resetRdsDisplay() {
  updateRdsPsOverlay(primaryRds);
}

function resetDecoderStateOnRigSwitch() {
  // RDS
  primaryRds = null;
  vchanRdsById = new Map();
  resetRdsDisplay();
  resetWfmStereoIndicator();
  resetIntfBars();

  // Spectrum — clear stale data from previous rig's SDR
  lastSpectrumData = null;
  window.lastSpectrumData = null;
  lastSpectrumRenderData = null;

  // Decoder status indicators
  const decoderIds = ["ais-status", "vdes-status", "aprs-status", "cw-status", "ft8-status", "wspr-status"];
  decoderIds.forEach((id) => {
    const el = document.getElementById(id);
    if (el) el.textContent = "--";
  });

  // FT8/FT4/WSPR history tables
  if (typeof window.ft8ClearHistory === "function") window.ft8ClearHistory();
  if (typeof window.ft4ClearHistory === "function") window.ft4ClearHistory();
  if (typeof window.ft2ClearHistory === "function") window.ft2ClearHistory();
  if (typeof window.wsprClearHistory === "function") window.wsprClearHistory();
}

function resetWfmStereoIndicator() {
  if (!wfmStFlagEl) return;
  wfmStFlagEl.textContent = "MO";
  wfmStFlagEl.classList.remove("wfm-st-flag-stereo");
  wfmStFlagEl.classList.add("wfm-st-flag-mono");
}

function updateIntfBar(fillEl, valEl, level) {
  if (!fillEl || !valEl) return;
  const v = Math.round(Math.min(Math.max(level, 0), 100));
  valEl.textContent = String(v);
  fillEl.style.width = v + "%";
  fillEl.classList.toggle("wfm-intf-warn", v >= 35 && v < 65);
  fillEl.classList.toggle("wfm-intf-high", v >= 65);
  if (v < 35) {
    fillEl.classList.remove("wfm-intf-warn", "wfm-intf-high");
  }
}

function resetIntfBars() {
  updateIntfBar(wfmCciFillEl, wfmCciValEl, 0);
  updateIntfBar(wfmAciFillEl, wfmAciValEl, 0);
}

// ── Fast CSS-based frequency/BW marker positioning ──────────────────────────
// These lightweight DOM elements reposition via `transform: translateX()`
// which is GPU-composited — zero layout/paint cost.  The full WebGL overlay
// (drawSignalOverlay) catches up on the next rAF.
const _fastFreqMarker = document.getElementById("fast-freq-marker");
const _fastBwLeft     = document.getElementById("fast-bw-left");
const _fastBwRight    = document.getElementById("fast-bw-right");

function positionFastOverlay(freqHz, bwHz) {
  if (!lastSpectrumData || !signalVisualBlockEl) {
    if (_fastFreqMarker) _fastFreqMarker.style.display = "none";
    if (_fastBwLeft) _fastBwLeft.style.display = "none";
    if (_fastBwRight) _fastBwRight.style.display = "none";
    return;
  }
  const cssW = signalVisualBlockEl.clientWidth;
  if (cssW <= 0) return;
  const range = spectrumVisibleRange(lastSpectrumData);
  const hzToFrac = (hz) => (hz - range.visLoHz) / range.visSpanHz;

  if (_fastFreqMarker && Number.isFinite(freqHz)) {
    const frac = hzToFrac(freqHz);
    if (frac >= 0 && frac <= 1) {
      _fastFreqMarker.style.display = "";
      _fastFreqMarker.style.transform = `translateX(${frac * cssW}px)`;
    } else {
      _fastFreqMarker.style.display = "none";
    }
  }
  if (_fastBwLeft && _fastBwRight && Number.isFinite(freqHz) && Number.isFinite(bwHz) && bwHz > 0) {
    const side = sidebandDirectionForMode(modeEl ? modeEl.value : "USB");
    let loHz, hiHz;
    if (side < 0) {
      loHz = freqHz - bwHz; hiHz = freqHz;
    } else if (side > 0) {
      loHz = freqHz; hiHz = freqHz + bwHz;
    } else {
      loHz = freqHz - bwHz / 2; hiHz = freqHz + bwHz / 2;
    }
    const lFrac = hzToFrac(loHz);
    const rFrac = hzToFrac(hiHz);
    const cFrac = hzToFrac(freqHz);
    // Left side of BW
    if (lFrac < cFrac && cFrac >= 0 && lFrac <= 1) {
      const x = Math.max(0, lFrac) * cssW;
      const w = (Math.min(1, cFrac) - Math.max(0, lFrac)) * cssW;
      _fastBwLeft.style.display = "";
      _fastBwLeft.style.transform = `translateX(${x}px)`;
      _fastBwLeft.style.width = `${w}px`;
    } else {
      _fastBwLeft.style.display = "none";
    }
    // Right side of BW
    if (rFrac > cFrac && rFrac >= 0 && cFrac <= 1) {
      const x = Math.max(0, cFrac) * cssW;
      const w = (Math.min(1, rFrac) - Math.max(0, cFrac)) * cssW;
      _fastBwRight.style.display = "";
      _fastBwRight.style.transform = `translateX(${x}px)`;
      _fastBwRight.style.width = `${w}px`;
    } else {
      _fastBwRight.style.display = "none";
    }
  }
}

function applyLocalTunedFrequency(hz, forceDisplay = false) {
  if (!Number.isFinite(hz)) return;
  const freqChanged = lastFreqHz !== hz;
  if (!freqChanged && !forceDisplay) return;
  if (freqChanged) {
    if (lastFreqHz != null) savePreviousTuneState();
    primaryRds = null;
    resetRdsDisplay();
    resetWfmStereoIndicator();
    resetIntfBars();
  }
  lastFreqHz = hz;
  window.lastFreqHz = lastFreqHz;
  updateDocumentTitle(activeChannelRds());
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
  if (window.refreshCwTonePicker) {
    window.refreshCwTonePicker();
  }
  // Instant CSS marker repositioning (GPU-composited, no WebGL).
  positionFastOverlay(lastFreqHz, currentBandwidthHz);
  if (freqChanged && lastSpectrumData) {
    scheduleSpectrumDraw();
  }
  if (freqChanged && !lastSpectrumData) {
    updateBandplanStrip(bandplanComputeRange());
  }
  positionRdsPsOverlay();
}

function coverageGuardBandwidthHz(mode = modeEl ? modeEl.value : "") {
  const [, , maxBw] = mwDefaultsForMode(mode);
  return Math.max(0, Number.isFinite(maxBw) ? maxBw : currentBandwidthHz);
}

function isAisMode(mode = modeEl ? modeEl.value : "") {
  return String(mode || "").toUpperCase() === "AIS";
}

function isVdesMode(mode = modeEl ? modeEl.value : "") {
  return String(mode || "").toUpperCase() === "VDES";
}

function visibleBandwidthSpecs(freqHz = lastFreqHz, mode = modeEl ? modeEl.value : "") {
  if (!Number.isFinite(freqHz)) return [];
  const modeUpper = String(mode || "").toUpperCase();
  if (modeUpper === "AIS") {
    return [
      { centerHz: freqHz, widthHz: currentBandwidthHz },
      { centerHz: freqHz + 50_000, widthHz: currentBandwidthHz },
    ];
  }
  return [{ centerHz: freqHz, widthHz: currentBandwidthHz }];
}

function sidebandDirectionForMode(mode = modeEl ? modeEl.value : "") {
  const modeUpper = String(mode || "").toUpperCase();
  if (modeUpper === "LSB" || modeUpper === "CWR") return -1;
  if (modeUpper === "USB" || modeUpper === "CW" || modeUpper === "DIG") return 1;
  return 0;
}

function displaySpanForBandwidthSpec(spec, mode = modeEl ? modeEl.value : "") {
  const centerHz = Number(spec?.centerHz);
  const widthHz = Math.max(0, Number.isFinite(spec?.widthHz) ? Number(spec.widthHz) : 0);
  const side = sidebandDirectionForMode(mode);
  if (side < 0) {
    return { loHz: centerHz - widthHz, hiHz: centerHz, side };
  }
  if (side > 0) {
    return { loHz: centerHz, hiHz: centerHz + widthHz, side };
  }
  const halfBw = widthHz / 2;
  return { loHz: centerHz - halfBw, hiHz: centerHz + halfBw, side };
}

function coverageSpanForMode(freqHz, bandwidthHz = coverageGuardBandwidthHz(), mode = modeEl ? modeEl.value : "") {
  if (!Number.isFinite(freqHz)) return null;
  const specs = visibleBandwidthSpecs(freqHz, mode).map((spec) => {
    const widthHz = Math.max(
      0,
      Number.isFinite(spec.widthHz) ? spec.widthHz : Math.max(0, Number.isFinite(bandwidthHz) ? bandwidthHz : 0),
    );
    return displaySpanForBandwidthSpec({ centerHz: spec.centerHz, widthHz }, mode);
  });
  if (specs.length === 0) return null;
  let loHz = specs[0].loHz;
  let hiHz = specs[0].hiHz;
  for (const spec of specs.slice(1)) {
    loHz = Math.min(loHz, spec.loHz);
    hiHz = Math.max(hiHz, spec.hiHz);
  }
  return { loHz, hiHz };
}

function visibleBandwidthCenters(freqHz = lastFreqHz, mode = modeEl ? modeEl.value : "") {
  return visibleBandwidthSpecs(freqHz, mode).map((spec) => spec.centerHz);
}

function effectiveSpectrumCoverageSpanHz(sampleRateHz) {
  const sampleRate = Number(sampleRateHz);
  if (!Number.isFinite(sampleRate) || sampleRate <= 0) return 0;
  // Keep a guard band at the spectrum edges; practical usable span is slightly smaller.
  const ratio = Number.isFinite(spectrumUsableSpanRatio) ? spectrumUsableSpanRatio : 0.92;
  return sampleRate * Math.max(0.01, Math.min(1.0, ratio));
}

function sweetSpotMinimumOffsetHz(bandwidthHz) {
  if (!Number.isFinite(bandwidthHz) || bandwidthHz <= 0) return 0;
  return bandwidthHz / 2;
}

function sweetSpotCenterHasRequiredOffset(centerHz, freqHz, bandwidthHz) {
  if (!Number.isFinite(centerHz) || !Number.isFinite(freqHz)) return false;
  const minOffsetHz = sweetSpotMinimumOffsetHz(bandwidthHz);
  if (!Number.isFinite(minOffsetHz) || minOffsetHz <= 0) return true;
  return Math.abs(centerHz - freqHz) >= minOffsetHz - 1;
}

function chooseSweetSpotCenterOutsideOffsetRange(freqHz, bandwidthHz, minCenterHz, maxCenterHz, preferredCenterHz = null) {
  if (!Number.isFinite(freqHz) || !Number.isFinite(minCenterHz) || !Number.isFinite(maxCenterHz) || minCenterHz > maxCenterHz) {
    return null;
  }

  const minOffsetHz = sweetSpotMinimumOffsetHz(bandwidthHz);
  if (!Number.isFinite(minOffsetHz) || minOffsetHz <= 0) {
    const fallbackCenterHz = Number.isFinite(preferredCenterHz) ? preferredCenterHz : freqHz;
    return alignFreqToRigStep(Math.round(Math.max(minCenterHz, Math.min(maxCenterHz, fallbackCenterHz))));
  }

  const targetCentersHz = [];
  const lowerTargetHz = alignFreqToRigStep(Math.round(freqHz - minOffsetHz));
  const upperTargetHz = alignFreqToRigStep(Math.round(freqHz + minOffsetHz));
  if (lowerTargetHz >= minCenterHz && lowerTargetHz <= maxCenterHz) targetCentersHz.push(lowerTargetHz);
  if (upperTargetHz >= minCenterHz && upperTargetHz <= maxCenterHz && !targetCentersHz.some((value) => Math.abs(value - upperTargetHz) < 1)) {
    targetCentersHz.push(upperTargetHz);
  }
  if (!targetCentersHz.length) return null;

  if (Number.isFinite(preferredCenterHz)) {
    let bestCenterHz = targetCentersHz[0];
    let bestDistance = Math.abs(bestCenterHz - preferredCenterHz);
    for (const targetCenterHz of targetCentersHz.slice(1)) {
      const distance = Math.abs(targetCenterHz - preferredCenterHz);
      if (distance < bestDistance) {
        bestDistance = distance;
        bestCenterHz = targetCenterHz;
      }
    }
    return bestCenterHz;
  }

  return targetCentersHz[0];
}

function requiredCenterFreqForCoverageInFrame(data, freqHz, bandwidthHz = coverageGuardBandwidthHz()) {
  if (!data || !Number.isFinite(freqHz)) return null;
  const sampleRate = effectiveSpectrumCoverageSpanHz(data.sample_rate);
  const currentCenterHz = Number(data.center_hz);
  if (!Number.isFinite(sampleRate) || sampleRate <= 0 || !Number.isFinite(currentCenterHz)) {
    return null;
  }

  const halfSpanHz = sampleRate / 2;
  const span = coverageSpanForMode(freqHz, bandwidthHz);
  if (!span) return null;
  const requiredLoHz = span.loHz - spectrumCoverageMarginHz;
  const requiredHiHz = span.hiHz + spectrumCoverageMarginHz;
  if (requiredHiHz - requiredLoHz >= sampleRate) {
    return alignFreqToRigStep(Math.round(freqHz));
  }

  const currentLoHz = currentCenterHz - halfSpanHz;
  const currentHiHz = currentCenterHz + halfSpanHz;
  if (requiredLoHz >= currentLoHz && requiredHiHz <= currentHiHz) {
    return null;
  }

  let nextCenterHz = currentCenterHz;
  if (requiredLoHz < currentLoHz) {
    nextCenterHz = requiredLoHz + halfSpanHz;
  }
  if (requiredHiHz > currentHiHz) {
    nextCenterHz = requiredHiHz - halfSpanHz;
  }
  return alignFreqToRigStep(Math.round(nextCenterHz));
}

function requiredCenterFreqForCoverage(freqHz, bandwidthHz = coverageGuardBandwidthHz()) {
  return requiredCenterFreqForCoverageInFrame(lastSpectrumData, freqHz, bandwidthHz);
}

async function ensureTunedBandwidthCoverage(freqHz, bandwidthHz = coverageGuardBandwidthHz()) {
  const nextCenterHz = requiredCenterFreqForCoverage(freqHz, bandwidthHz);
  if (!Number.isFinite(nextCenterHz)) return;
  if (lastSpectrumData && Math.abs(nextCenterHz - Number(lastSpectrumData.center_hz)) < 1) return;
  await postPath(`/set_center_freq?hz=${nextCenterHz}`);
  if (centerFreqEl && !centerFreqDirty) {
    centerFreqEl.value = formatFreqForStep(nextCenterHz, jogUnit);
  }
}

// Guard: while a set_freq is in flight, SSE state updates must not overwrite
// the optimistic local frequency with the stale server value.
let _freqOptimisticHz = null;
let _freqOptimisticSeq = 0;

function setRigFrequency(freqHz) {
  const targetHz = Math.round(freqHz);
  if (!freqAllowed(targetHz)) {
    showUnsupportedFreqPopup(targetHz);
    throw new Error(`Unsupported frequency: ${targetHz}`);
  }
  // Optimistic local update — visual is instant via CSS overlay + guard.
  const prevFreqHz = lastFreqHz;
  const seq = ++_freqOptimisticSeq;
  _freqOptimisticHz = targetHz;
  applyLocalTunedFrequency(targetHz);
  // Fire-and-forget: network calls run in background. The SSE stream will
  // push the confirmed frequency; the optimistic guard prevents snap-back.
  Promise.all([
    postPath(`/set_freq?hz=${targetHz}`),
    ensureTunedBandwidthCoverage(targetHz),
  ]).catch((err) => {
    // Roll back only if no newer optimistic call has superseded this one.
    if (_freqOptimisticSeq === seq && prevFreqHz != null) {
      _freqOptimisticHz = null;
      applyLocalTunedFrequency(prevFreqHz, true);
    }
    console.warn("setRigFrequency failed:", err);
  }).finally(() => {
    if (_freqOptimisticSeq === seq) _freqOptimisticHz = null;
  });
}

function spectrumBinIndexForHz(data, hz) {
  if (!data || !isBinsArray(data.bins) || data.bins.length < 2 || !Number.isFinite(hz)) {
    return null;
  }
  const maxIdx = data.bins.length - 1;
  const fullLoHz = Number(data.center_hz) - Number(data.sample_rate) / 2;
  const idx = Math.round(((hz - fullLoHz) / Number(data.sample_rate)) * maxIdx);
  return Math.max(0, Math.min(maxIdx, idx));
}

function spectrumPowerScore(db) {
  const value = Number.isFinite(db) ? db : -160;
  const clamped = Math.max(-160, Math.min(40, value));
  return 10 ** (clamped / 10);
}

function sweetSpotCandidateForFrame(data, freqHz, bandwidthHz) {
  if (!data || !isBinsArray(data.bins) || data.bins.length < 16) {
    return null;
  }
  if (!Number.isFinite(freqHz) || !Number.isFinite(bandwidthHz) || bandwidthHz <= 0) {
    return null;
  }

  const bins = data.bins;
  const sampleRate = Number(data.sample_rate);
  const usableSpanHz = effectiveSpectrumCoverageSpanHz(sampleRate);
  const currentCenterHz = Number(data.center_hz);
  if (!Number.isFinite(sampleRate) || sampleRate <= 0 || !Number.isFinite(usableSpanHz) || usableSpanHz <= 0 || !Number.isFinite(currentCenterHz)) {
    return null;
  }

  const halfUsableSpanHz = usableSpanHz / 2;
  const fullHalfSpanHz = sampleRate / 2;
  const span = coverageSpanForMode(freqHz, bandwidthHz);
  if (!span) return null;
  const requiredLoHz = span.loHz - spectrumCoverageMarginHz;
  const requiredHiHz = span.hiHz + spectrumCoverageMarginHz;
  if (requiredHiHz - requiredLoHz >= usableSpanHz) {
    const fallbackCenterHz = chooseSweetSpotCenterOutsideOffsetRange(
      freqHz,
      bandwidthHz,
      currentCenterHz - halfUsableSpanHz,
      currentCenterHz + halfUsableSpanHz,
      requiredCenterFreqForCoverageInFrame(data, freqHz, bandwidthHz),
    );
    if (!Number.isFinite(fallbackCenterHz)) return null;
    return { centerHz: fallbackCenterHz, score: Number.POSITIVE_INFINITY };
  }

  const evalHalfSpanHz = Math.max(0, (sampleRate - usableSpanHz) / 2);
  const evalMinCenterHz = currentCenterHz - evalHalfSpanHz;
  const evalMaxCenterHz = currentCenterHz + evalHalfSpanHz;
  const fitMinCenterHz = requiredHiHz - halfUsableSpanHz;
  const fitMaxCenterHz = requiredLoHz + halfUsableSpanHz;
  const minCenterHz = Math.max(evalMinCenterHz, fitMinCenterHz);
  const maxCenterHz = Math.min(evalMaxCenterHz, fitMaxCenterHz);
  if (!Number.isFinite(minCenterHz) || !Number.isFinite(maxCenterHz) || minCenterHz > maxCenterHz) {
    const fallbackCenterHz = chooseSweetSpotCenterOutsideOffsetRange(
      freqHz,
      bandwidthHz,
      evalMinCenterHz,
      evalMaxCenterHz,
      requiredCenterFreqForCoverageInFrame(data, freqHz, bandwidthHz),
    );
    if (!Number.isFinite(fallbackCenterHz)) return null;
    return { centerHz: fallbackCenterHz, score: Number.POSITIVE_INFINITY };
  }

  const maxIdx = bins.length - 1;
  const usableBins = Math.max(4, Math.min(maxIdx, Math.round((usableSpanHz / sampleRate) * maxIdx)));
  const fullLoHz = currentCenterHz - fullHalfSpanHz;
  const startMinIdx = Math.max(
    0,
    Math.min(maxIdx - usableBins, Math.round((((minCenterHz - halfUsableSpanHz) - fullLoHz) / sampleRate) * maxIdx)),
  );
  const startMaxIdx = Math.max(
    startMinIdx,
    Math.min(maxIdx - usableBins, Math.round((((maxCenterHz - halfUsableSpanHz) - fullLoHz) / sampleRate) * maxIdx)),
  );

  let bestStartIdx = null;
  let bestScore = Number.POSITIVE_INFINITY;
  const signalLoHz = span.loHz;
  const signalHiHz = span.hiHz;

  for (let startIdx = startMinIdx; startIdx <= startMaxIdx; startIdx += 1) {
    const endIdx = Math.min(maxIdx, startIdx + usableBins);
    const windowLoHz = fullLoHz + (startIdx / maxIdx) * sampleRate;
    const candidateCenterHz = windowLoHz + halfUsableSpanHz;
    if (!sweetSpotCenterHasRequiredOffset(candidateCenterHz, freqHz, bandwidthHz)) {
      continue;
    }
    const signalLoIdx = Math.max(startIdx, Math.min(endIdx, spectrumBinIndexForHz(data, signalLoHz)));
    const signalHiIdx = Math.max(startIdx, Math.min(endIdx, spectrumBinIndexForHz(data, signalHiHz)));

    let score = 0;
    for (let i = startIdx; i <= endIdx; i++) {
      if (i >= signalLoIdx && i <= signalHiIdx) continue;
      score += spectrumPowerScore(bins[i]);
    }

    // Keep a very small bias toward a reasonably centered passband when scores are close.
    const spanMidHz = (span.loHz + span.hiHz) / 2;
    const centeredOffsetHz = Math.abs(candidateCenterHz - spanMidHz);
    score *= 1 + centeredOffsetHz / Math.max(usableSpanHz, 1) * 0.08;
    if (score < bestScore) {
      bestScore = score;
      bestStartIdx = startIdx;
    }
  }

  if (!Number.isFinite(bestScore) || bestStartIdx == null) {
    const fallbackCenterHz = chooseSweetSpotCenterOutsideOffsetRange(
      freqHz,
      bandwidthHz,
      minCenterHz,
      maxCenterHz,
      requiredCenterFreqForCoverageInFrame(data, freqHz, bandwidthHz),
    );
    if (!Number.isFinite(fallbackCenterHz)) return null;
    return { centerHz: fallbackCenterHz, score: Number.POSITIVE_INFINITY };
  }

  const bestLoHz = fullLoHz + (bestStartIdx / maxIdx) * sampleRate;
  const bestCenterHz = bestLoHz + halfUsableSpanHz;
  return {
    centerHz: alignFreqToRigStep(Math.round(bestCenterHz)),
    score: bestScore,
  };
}

function sweetSpotCenterFreq(freqHz = lastFreqHz, bandwidthHz = currentBandwidthHz) {
  const candidate = sweetSpotCandidateForFrame(lastSpectrumData, freqHz, bandwidthHz);
  return candidate && Number.isFinite(candidate.centerHz) ? candidate.centerHz : null;
}

function sweetSpotProbeCenters(data, freqHz, bandwidthHz) {
  if (!data || !Number.isFinite(freqHz) || !Number.isFinite(bandwidthHz) || bandwidthHz <= 0) {
    return [];
  }

  const sampleRate = Number(data.sample_rate);
  const usableSpanHz = effectiveSpectrumCoverageSpanHz(sampleRate);
  if (!Number.isFinite(usableSpanHz) || usableSpanHz <= 0) return [];

  const halfUsableSpanHz = usableSpanHz / 2;
  const span = coverageSpanForMode(freqHz, bandwidthHz);
  if (!span) return [];
  const requiredLoHz = span.loHz - spectrumCoverageMarginHz;
  const requiredHiHz = span.hiHz + spectrumCoverageMarginHz;
  if (requiredHiHz - requiredLoHz >= usableSpanHz) {
    const probeCenters = [];
    const minOffsetHz = sweetSpotMinimumOffsetHz(bandwidthHz);
    for (const centerHz of [freqHz - minOffsetHz, freqHz + minOffsetHz]) {
      const alignedHz = alignFreqToRigStep(Math.round(centerHz));
      if (sweetSpotCenterHasRequiredOffset(alignedHz, freqHz, bandwidthHz)
        && !probeCenters.some((value) => Math.abs(value - alignedHz) < 1)) {
        probeCenters.push(alignedHz);
      }
    }
    return probeCenters;
  }

  const minCenterHz = requiredHiHz - halfUsableSpanHz;
  const maxCenterHz = requiredLoHz + halfUsableSpanHz;
  if (!Number.isFinite(minCenterHz) || !Number.isFinite(maxCenterHz) || minCenterHz > maxCenterHz) {
    return [];
  }

  const points = 5;
  const centers = [];
  for (let i = 0; i < points; i++) {
    const frac = points === 1 ? 0.5 : i / (points - 1);
    const centerHz = alignFreqToRigStep(Math.round(minCenterHz + (maxCenterHz - minCenterHz) * frac));
    if (sweetSpotCenterHasRequiredOffset(centerHz, freqHz, bandwidthHz)
      && !centers.some((value) => Math.abs(value - centerHz) < 1)) {
      centers.push(centerHz);
    }
  }

  const currentCenterHz = alignFreqToRigStep(Math.round(Number(data.center_hz)));
  if (Number.isFinite(currentCenterHz)
    && sweetSpotCenterHasRequiredOffset(currentCenterHz, freqHz, bandwidthHz)
    && !centers.some((value) => Math.abs(value - currentCenterHz) < 1)) {
    centers.push(currentCenterHz);
    centers.sort((a, b) => a - b);
  }
  return centers;
}

async function applySweetSpotCenter() {
  if (sweetSpotScanInFlight) {
    showHint("Sweet-spot already scanning", 900);
    return;
  }
  if (!Number.isFinite(lastFreqHz) || !lastSpectrumData) return;

  const originalCenterHz = Number(lastSpectrumData.center_hz);
  const probeCentersHz = sweetSpotProbeCenters(lastSpectrumData, lastFreqHz, currentBandwidthHz);
  let bestCandidate = sweetSpotCandidateForFrame(lastSpectrumData, lastFreqHz, currentBandwidthHz);
  if (!probeCentersHz.length && (!bestCandidate || !Number.isFinite(bestCandidate.centerHz))) {
    showHint("Sweet-spot unavailable", 1100);
    return;
  }

  sweetSpotScanInFlight = true;
  try {
    showHint("Scanning sweet spot...", 1400);

    for (const probeCenterHz of probeCentersHz) {
      if (!Number.isFinite(probeCenterHz)) continue;
      let probeFrame = lastSpectrumData;
      if (!probeFrame || Math.abs(Number(probeFrame.center_hz) - probeCenterHz) >= 1) {
        await postPath(`/set_center_freq?hz=${probeCenterHz}`);
        try {
          probeFrame = await waitForSpectrumFrame(probeCenterHz, 1400);
        } catch (_) {
          continue;
        }
      }

      const candidate = sweetSpotCandidateForFrame(probeFrame, lastFreqHz, currentBandwidthHz);
      if (!candidate || !Number.isFinite(candidate.centerHz)) continue;
      if (!bestCandidate || candidate.score < bestCandidate.score) {
        bestCandidate = candidate;
      }
    }

    const targetCenterHz = bestCandidate && Number.isFinite(bestCandidate.centerHz)
      ? bestCandidate.centerHz
      : sweetSpotCenterFreq(lastFreqHz, currentBandwidthHz);
    if (!Number.isFinite(targetCenterHz)) {
      if (Number.isFinite(originalCenterHz) && (!lastSpectrumData || Math.abs(Number(lastSpectrumData.center_hz) - originalCenterHz) >= 1)) {
        await postPath(`/set_center_freq?hz=${alignFreqToRigStep(Math.round(originalCenterHz))}`);
      }
      showHint("Sweet-spot unavailable", 1100);
      return;
    }
    if (!lastSpectrumData || Math.abs(targetCenterHz - Number(lastSpectrumData.center_hz)) >= 1) {
      await postPath(`/set_center_freq?hz=${targetCenterHz}`);
    }
    if (centerFreqEl && !centerFreqDirty) {
      centerFreqEl.value = formatFreqForStep(targetCenterHz, jogUnit);
    }
    if (Number.isFinite(originalCenterHz) && Math.abs(targetCenterHz - originalCenterHz) < 1) {
      showHint("Already at sweet spot", 900);
    } else {
      showHint("Sweet-spot set", 1200);
    }
  } finally {
    sweetSpotScanInFlight = false;
  }
}

function tunedFrequencyForCenterCoverage(centerHz, freqHz = lastFreqHz, bandwidthHz = coverageGuardBandwidthHz()) {
  if (!Number.isFinite(centerHz) || !Number.isFinite(freqHz) || !lastSpectrumData) return null;
  const sampleRate = effectiveSpectrumCoverageSpanHz(lastSpectrumData.sample_rate);
  if (!Number.isFinite(sampleRate) || sampleRate <= 0) return null;

  const span = coverageSpanForMode(freqHz, bandwidthHz);
  if (!span) return null;
  const halfSpanHz = sampleRate / 2;
  const requiredLoOffset = freqHz - (span.loHz - spectrumCoverageMarginHz);
  const requiredHiOffset = (span.hiHz + spectrumCoverageMarginHz) - freqHz;
  if (requiredLoOffset + requiredHiOffset >= sampleRate) {
    return alignFreqToRigStep(Math.round(centerHz));
  }

  const minFreqHz = centerHz - halfSpanHz + requiredLoOffset;
  const maxFreqHz = centerHz + halfSpanHz - requiredHiOffset;
  if (freqHz >= minFreqHz && freqHz <= maxFreqHz) {
    return null;
  }
  const clampedHz = Math.max(minFreqHz, Math.min(maxFreqHz, freqHz));
  return alignFreqToRigStep(Math.round(clampedHz));
}

// Optimistic center freq: updated immediately on each arrow click so that
// rapid clicks accumulate rather than all starting from the same stale frame.
let spectrumCenterPendingHz = null;

async function shiftSpectrumCenter(direction) {
  if (!lastSpectrumData || !Number.isFinite(direction) || direction === 0) return;
  const sampleRate = effectiveSpectrumCoverageSpanHz(lastSpectrumData.sample_rate);
  const currentCenterHz = spectrumCenterPendingHz ?? Number(lastSpectrumData.center_hz);
  if (!Number.isFinite(sampleRate) || sampleRate <= 0 || !Number.isFinite(currentCenterHz)) return;

  const stepHz = Math.max(50_000, Math.round(sampleRate * 0.35));
  const nextCenterHz = alignFreqToRigStep(Math.round(currentCenterHz + direction * stepHz));
  spectrumCenterPendingHz = nextCenterHz;
  showHint("Shifting spectrum…", 900);
  await postPath(`/set_center_freq?hz=${nextCenterHz}`);
  if (centerFreqEl && !centerFreqDirty) {
    centerFreqEl.value = formatFreqForStep(nextCenterHz, jogUnit);
  }

  const nextFreqHz = tunedFrequencyForCenterCoverage(nextCenterHz);
  if (Number.isFinite(nextFreqHz) && Math.abs(nextFreqHz - Number(lastFreqHz)) >= 1) {
    await postPath(`/set_freq?hz=${nextFreqHz}`);
    applyLocalTunedFrequency(nextFreqHz);
  }
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
  const rawNumber = match[1];
  let num = parseFloat(rawNumber.replace(",", "."));
  const unit = match[2] || "";
  if (Number.isNaN(num)) return null;
  if (unit.startsWith("gh") || unit === "g") {
    num *= 1_000_000_000;
  } else if (unit.startsWith("mh") || unit === "m") {
    num *= 1_000_000;
  } else if (unit.startsWith("kh") || unit === "k") {
    num *= 1_000;
  } else if (!unit) {
    const mode = (modeEl?.value || "").toUpperCase();
    const hasDecimalSeparator = rawNumber.includes(".") || rawNumber.includes(",");
    if (mode === "WFM") {
      if (hasDecimalSeparator && num >= 50 && num < 200) {
        num *= 1_000_000;
        return Math.round(num);
      }
      if (!hasDecimalSeparator && num >= 875 && num <= 1080) {
        num = (num / 10) * 1_000_000;
        return Math.round(num);
      }
    }
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
      .filter((b) => typeof b.low_hz === "number" && typeof b.high_hz === "number")
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

function unsupportedBandSummary() {
  if (supportedBands.length === 0) return "No supported frequency ranges were reported by the rig.";
  const ranges = supportedBands
    .slice()
    .sort((a, b) => a.low - b.low)
    .map((b) => `${formatFreqForHumans(b.low)} to ${formatFreqForHumans(b.high)}`);
  return `Supported ranges: ${ranges.join(", ")}`;
}

function formatFreqForHumans(hz) {
  if (!Number.isFinite(hz)) return "--";
  if (hz >= 1_000_000_000) return `${(hz / 1_000_000_000).toFixed(3)} GHz`;
  if (hz >= 1_000_000) return `${(hz / 1_000_000).toFixed(3)} MHz`;
  if (hz >= 1_000) return `${(hz / 1_000).toFixed(3)} kHz`;
  return `${Math.round(hz)} Hz`;
}

function showUnsupportedFreqPopup(hz) {
  const message = `Unsupported frequency: ${formatFreqForHumans(hz)}.\n\n${unsupportedBandSummary()}`;
  showHint("Out of supported range", 1800);
  const now = Date.now();
  if (now - lastUnsupportedFreqPopupAt < 1200) return;
  lastUnsupportedFreqPopupAt = now;
  window.alert(message);
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
  if (!Number.isFinite(sUnits) || sUnits <= 0) return `${sigUnit("S")}0`;
  if (sUnits <= 9) return `${sigUnit("S")}${Math.round(sUnits)}`;
  // S9+xdB: round to nearest 10 dB step, cap at +60.
  const overDb = Math.min(60, Math.round((sUnits - 9) * 10 / 10) * 10);
  return overDb === 0 ? `${sigUnit("S")}9` : `${sigUnit("S")}9+${overDb}${sigUnit("dB")}`;
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
let ownerWebsiteUrl = null;
let ownerWebsiteName = null;
let aisVesselUrlBase = null;
let serverRigs = [];
let serverActiveRigId = null;
let serverLat = null;
let serverLon = null;
let initialMapZoom = 10;
let spectrumCoverageMarginHz = 50_000;
let spectrumUsableSpanRatio = 0.92;
const DEFAULT_OVERVIEW_PLOT_HEIGHT_PX = 160;
const DEFAULT_SPECTRUM_PLOT_HEIGHT_PX = 160;
const MIN_OVERVIEW_PLOT_HEIGHT_PX = 90;
const MIN_SPECTRUM_PLOT_HEIGHT_PX = 130;
const DEFAULT_SIGNAL_SPLIT_PERCENT = 50;
const MIN_SIGNAL_SPLIT_PERCENT = 20;
const MAX_SIGNAL_SPLIT_PERCENT = 80;
let spectrumLayoutPending = false;
let spectrumManualTotalPlotHeightPx = null;
let spectrumResizeState = null;
let signalSplitPercent = clampSignalSplitPercent(
  Number(loadSetting("signalSplitPercent", DEFAULT_SIGNAL_SPLIT_PERCENT)),
);


function scheduleSpectrumLayout() {
  if (spectrumLayoutPending) return;
  spectrumLayoutPending = true;
  requestAnimationFrame(() => {
    spectrumLayoutPending = false;
    updateSpectrumAutoHeight();
  });
}

function clampSignalSplitPercent(value) {
  const numeric = Number.isFinite(value) ? value : DEFAULT_SIGNAL_SPLIT_PERCENT;
  return Math.max(
    MIN_SIGNAL_SPLIT_PERCENT,
    Math.min(MAX_SIGNAL_SPLIT_PERCENT, Math.round(numeric)),
  );
}

function updateSignalSplitControlText() {
  if (!signalSplitValueEl) return;
  signalSplitValueEl.textContent = `${signalSplitPercent}/${100 - signalSplitPercent}`;
}

function setSignalSplitControlVisible(visible) {
  if (!signalSplitControlEl) return;
  signalSplitControlEl.style.display = visible ? "flex" : "none";
}

function currentOverviewHeightPx(overviewCanvasEl) {
  return Math.max(
    MIN_OVERVIEW_PLOT_HEIGHT_PX,
    Math.round(overviewCanvasEl?.clientHeight || DEFAULT_OVERVIEW_PLOT_HEIGHT_PX),
  );
}

function currentSpectrumHeightPx(spectrumCanvasEl) {
  return Math.max(
    MIN_SPECTRUM_PLOT_HEIGHT_PX,
    Math.round(spectrumCanvasEl?.clientHeight || DEFAULT_SPECTRUM_PLOT_HEIGHT_PX),
  );
}

function spectrumHeightBoundsPx(tabMainEl, contentEl, overviewCanvasEl, spectrumCanvasEl) {
  const currentOverviewHeight = currentOverviewHeightPx(overviewCanvasEl);
  const currentSpectrumHeight = currentSpectrumHeightPx(spectrumCanvasEl);
  const currentTotalHeight = currentOverviewHeight + currentSpectrumHeight;
  const tabBottom = tabMainEl.getBoundingClientRect().bottom;
  const contentBottom = contentEl.getBoundingClientRect().bottom;
  const slackPx = Math.floor(tabBottom - contentBottom);
  const minTotalHeight = MIN_OVERVIEW_PLOT_HEIGHT_PX + MIN_SPECTRUM_PLOT_HEIGHT_PX;
  const maxAutoTotalHeight = Math.max(
    minTotalHeight,
    currentTotalHeight + slackPx - 2,
  );
  return {
    minTotal: minTotalHeight,
    autoMaxTotal: maxAutoTotalHeight,
  };
}

function updateSpectrumAutoHeight() {
  const root = document.documentElement;
  const overviewCanvasEl = document.getElementById("overview-canvas");
  const spectrumPanelEl = document.getElementById("spectrum-panel");
  const spectrumCanvasEl = document.getElementById("spectrum-canvas");
  if (!root || !tabMainEl || !contentEl || !overviewCanvasEl || !spectrumPanelEl || !spectrumCanvasEl) return;

  const mainVisible = getComputedStyle(tabMainEl).display !== "none";
  const contentVisible = getComputedStyle(contentEl).display !== "none";
  const spectrumVisible = getComputedStyle(spectrumPanelEl).display !== "none";
  const currentOverviewHeight = currentOverviewHeightPx(overviewCanvasEl);
  const currentSpectrumHeight = currentSpectrumHeightPx(spectrumCanvasEl);

  if (!mainVisible || !contentVisible || !spectrumVisible) {
    setSignalSplitControlVisible(false);
    const dimensionsChanged =
      currentOverviewHeight !== DEFAULT_OVERVIEW_PLOT_HEIGHT_PX
      || currentSpectrumHeight !== DEFAULT_SPECTRUM_PLOT_HEIGHT_PX;
    root.style.setProperty("--overview-plot-height", `${DEFAULT_OVERVIEW_PLOT_HEIGHT_PX}px`);
    root.style.setProperty("--spectrum-plot-height", `${DEFAULT_SPECTRUM_PLOT_HEIGHT_PX}px`);
    if (dimensionsChanged) {
      resizeHeaderSignalCanvas();
      scheduleOverviewDraw();
      if (lastSpectrumData) scheduleSpectrumDraw();
    }
    return;
  }

  setSignalSplitControlVisible(true);
  const bounds = spectrumHeightBoundsPx(tabMainEl, contentEl, overviewCanvasEl, spectrumCanvasEl);
  const nextTotalHeight = spectrumManualTotalPlotHeightPx == null
    ? bounds.autoMaxTotal
    : Math.max(bounds.minTotal, Math.round(spectrumManualTotalPlotHeightPx));
  if (spectrumManualTotalPlotHeightPx != null) {
    spectrumManualTotalPlotHeightPx = nextTotalHeight;
  }
  const requestedOverviewHeight = Math.round((nextTotalHeight * signalSplitPercent) / 100);
  const nextOverviewHeight = Math.max(
    MIN_OVERVIEW_PLOT_HEIGHT_PX,
    Math.min(nextTotalHeight - MIN_SPECTRUM_PLOT_HEIGHT_PX, requestedOverviewHeight),
  );
  const nextSpectrumHeight = Math.max(
    MIN_SPECTRUM_PLOT_HEIGHT_PX,
    nextTotalHeight - nextOverviewHeight,
  );
  if (
    Math.abs(nextOverviewHeight - currentOverviewHeight) < 2
    && Math.abs(nextSpectrumHeight - currentSpectrumHeight) < 2
  ) return;

  root.style.setProperty("--overview-plot-height", `${nextOverviewHeight}px`);
  root.style.setProperty("--spectrum-plot-height", `${nextSpectrumHeight}px`);
  // Refresh cached canvas sizes after layout change.
  if (typeof _updateCachedCanvasSizes === "function") _updateCachedCanvasSizes();
  if (lastSpectrumData) {
    scheduleSpectrumDraw();
    scheduleOverviewDraw();
    scheduleSpectrumWaterfallDraw();
  }
}

function beginSpectrumResize(clientY) {
  const overviewCanvasEl = document.getElementById("overview-canvas");
  const spectrumCanvasEl = document.getElementById("spectrum-canvas");
  const spectrumPanelEl = document.getElementById("spectrum-panel");
  if (!tabMainEl || !contentEl || !overviewCanvasEl || !spectrumCanvasEl || !spectrumPanelEl) return false;
  if (getComputedStyle(spectrumPanelEl).display === "none") return false;
  const bounds = spectrumHeightBoundsPx(tabMainEl, contentEl, overviewCanvasEl, spectrumCanvasEl);
  const startTotalHeight = Math.max(
    bounds.minTotal,
    currentOverviewHeightPx(overviewCanvasEl) + currentSpectrumHeightPx(spectrumCanvasEl),
  );
  spectrumResizeState = {
    startY: clientY,
    startTotalHeight,
    minTotalHeight: bounds.minTotal,
  };
  document.body.classList.add("spectrum-resizing");
  return true;
}

function updateSpectrumResize(clientY) {
  if (!spectrumResizeState) return;
  const deltaY = clientY - spectrumResizeState.startY;
  spectrumManualTotalPlotHeightPx = Math.max(
    spectrumResizeState.minTotalHeight,
    Math.round(spectrumResizeState.startTotalHeight + deltaY),
  );
  updateSpectrumAutoHeight();
}

function endSpectrumResize() {
  spectrumResizeState = null;
  document.body.classList.remove("spectrum-resizing");
}

const spectrumSizeGrip = document.getElementById("spectrum-size-grip");
if (spectrumSizeGrip) {
  spectrumSizeGrip.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    if (!beginSpectrumResize(event.clientY)) return;
    event.preventDefault();
    if (typeof spectrumSizeGrip.setPointerCapture === "function") {
      spectrumSizeGrip.setPointerCapture(event.pointerId);
    }
  });
  spectrumSizeGrip.addEventListener("pointermove", (event) => {
    if (!spectrumResizeState) return;
    updateSpectrumResize(event.clientY);
  });
  const finishResize = (event) => {
    if (!spectrumResizeState) return;
    if (typeof spectrumSizeGrip.releasePointerCapture === "function" && spectrumSizeGrip.hasPointerCapture(event.pointerId)) {
      spectrumSizeGrip.releasePointerCapture(event.pointerId);
    }
    endSpectrumResize();
  };
  spectrumSizeGrip.addEventListener("pointerup", finishResize);
  spectrumSizeGrip.addEventListener("pointercancel", finishResize);
  spectrumSizeGrip.addEventListener("dblclick", () => {
    spectrumManualTotalPlotHeightPx = null;
    scheduleSpectrumLayout();
  });
}

if (signalSplitSliderEl) {
  signalSplitSliderEl.value = String(signalSplitPercent);
  signalSplitSliderEl.addEventListener("input", () => {
    signalSplitPercent = clampSignalSplitPercent(Number(signalSplitSliderEl.value));
    signalSplitSliderEl.value = String(signalSplitPercent);
    updateSignalSplitControlText();
    saveSetting("signalSplitPercent", signalSplitPercent);
    scheduleSpectrumLayout();
  });
  signalSplitSliderEl.addEventListener("dblclick", (event) => {
    event.preventDefault();
    signalSplitPercent = DEFAULT_SIGNAL_SPLIT_PERCENT;
    signalSplitSliderEl.value = String(signalSplitPercent);
    updateSignalSplitControlText();
    saveSetting("signalSplitPercent", signalSplitPercent);
    scheduleSpectrumLayout();
  });
}
updateSignalSplitControlText();

function updateTitle() {
  const titleEl = document.getElementById("rig-title");
  if (titleEl) {
    if (ownerWebsiteUrl) {
      const label = ownerWebsiteName || displayLabelFromUrl(ownerWebsiteUrl);
      titleEl.innerHTML =
        `<a class="title-link" href="${escapeMapHtml(ownerWebsiteUrl)}" target="_blank" rel="noopener">${escapeMapHtml(label)}</a>`;
    } else {
      titleEl.textContent = serverVersion ? `trx-rs v${serverVersion}` : "trx-rs";
    }
  }
  updateDocumentTitle(activeChannelRds());
}

function displayLabelFromUrl(url) {
  try {
    const host = new URL(url).hostname.replace(/^www\./i, "");
    return host || url;
  } catch (_e) {
    return url;
  }
}

window.buildAisVesselUrl = function(mmsi) {
  if (!aisVesselUrlBase || !Number.isFinite(Number(mmsi))) return null;
  return `${aisVesselUrlBase}${String(mmsi)}`;
};

function render(update) {
  if (!update) return;
  if (update.server_version) serverVersion = update.server_version;
  if (update.server_build_date) serverBuildDate = update.server_build_date;
  if (update.server_callsign) serverCallsign = update.server_callsign;
  if (typeof update.owner_callsign === "string" && update.owner_callsign.length > 0) {
    ownerCallsign = update.owner_callsign;
  }
  if (typeof update.owner_website_url === "string" && update.owner_website_url.length > 0) {
    ownerWebsiteUrl = update.owner_website_url;
  }
  if (typeof update.owner_website_name === "string" && update.owner_website_name.length > 0) {
    ownerWebsiteName = update.owner_website_name;
  }
  if (typeof update.ais_vessel_url_base === "string" && update.ais_vessel_url_base.length > 0) {
    aisVesselUrlBase = update.ais_vessel_url_base;
  }
  const prevLat = serverLat, prevLon = serverLon;
  if (update.server_latitude != null) serverLat = update.server_latitude;
  if (update.server_longitude != null) serverLon = update.server_longitude;
  if (locationSubtitle && Number.isFinite(serverLat) && Number.isFinite(serverLon)
      && (serverLat !== prevLat || serverLon !== prevLon || !locationSubtitle.textContent)) {
    const grid = latLonToMaidenhead(serverLat, serverLon);
    locationSubtitle.textContent = `Location: ${grid}`;
    locationSubtitle.style.display = "";
    window.trx.map?.reverseGeocodeLocation(serverLat, serverLon, grid);
  }
  window.trx.map?.syncAprsReceiverMarker();
  if (typeof update.initial_map_zoom === "number" && Number.isFinite(update.initial_map_zoom)) {
    initialMapZoom = Math.max(1, Math.round(update.initial_map_zoom));
  }
  if (
    typeof update.spectrum_coverage_margin_hz === "number" &&
    Number.isFinite(update.spectrum_coverage_margin_hz)
  ) {
    spectrumCoverageMarginHz = Math.max(1, Math.round(update.spectrum_coverage_margin_hz));
  }
  if (
    typeof update.spectrum_usable_span_ratio === "number" &&
    Number.isFinite(update.spectrum_usable_span_ratio)
  ) {
    spectrumUsableSpanRatio = Math.max(0.01, Math.min(1.0, Number(update.spectrum_usable_span_ratio)));
  }
  if (
    typeof update.decode_history_retention_min === "number" &&
    Number.isFinite(update.decode_history_retention_min) &&
    update.decode_history_retention_min > 0
  ) {
    const nextRetentionMin = Math.max(1, Math.round(Number(update.decode_history_retention_min)));
    if (nextRetentionMin !== decodeHistoryRetentionMin) {
      decodeHistoryRetentionMin = nextRetentionMin;
      if (typeof window.applyDecodeHistoryRetention === "function") {
        window.applyDecodeHistoryRetention();
      }
    }
  }
  scheduleSpectrumLayout();
  updateTitle();

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
  // Server subtitle: keep the static "trx-client vX.Y.Z" and append callsign if available.
  if (serverSubtitle && update.server_callsign) {
    const base = serverSubtitle.textContent.split(" hosted by")[0];
    const safeCallsign = escapeMapHtml(update.server_callsign);
    const encodedCallsign = encodeURIComponent(update.server_callsign);
    serverSubtitle.innerHTML =
      `${escapeMapHtml(base)} hosted by <a href="https://qrzcq.com/call/${encodedCallsign}" target="_blank" rel="noopener">${safeCallsign}</a>`;
  }
  // Note: rig switch decoder reset is now handled in switchRigFromSelect()
  // so that other tabs' switches don't reset our state.
  updateRigSubtitle(lastActiveRigId);
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
      modeEl.replaceChildren();
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
    window.currentBandwidthHz = currentBandwidthHz;
    syncBandwidthInput(currentBandwidthHz);
    // Reposition BW overlay immediately so freq+bw render together.
    positionFastOverlay(lastFreqHz, currentBandwidthHz);
    if (window.refreshCwTonePicker) {
      window.refreshCwTonePicker();
    }
    if (
      sdrGainEl
      && typeof update.filter.sdr_gain_db === "number"
      && document.activeElement !== sdrGainEl
    ) {
      sdrGainEl.value = String(Math.round(update.filter.sdr_gain_db));
    }
    if (sdrLnaGainEl && typeof update.filter.sdr_lna_gain_db === "number"
      && document.activeElement !== sdrLnaGainEl) {
      sdrLnaGainEl.value = String(Math.round(update.filter.sdr_lna_gain_db));
      if (sdrLnaGainControlsEl) sdrLnaGainControlsEl.style.display = "";
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
    if (wfmDenoiseEl && (typeof update.filter.wfm_denoise === "string" || typeof update.filter.wfm_denoise === "boolean")) {
      const nextDenoise = typeof update.filter.wfm_denoise === "string"
        ? normalizeWfmDenoiseLevel(update.filter.wfm_denoise)
        : (update.filter.wfm_denoise ? "auto" : "off");
      if (wfmDenoiseEl.value !== nextDenoise) {
        wfmDenoiseEl.value = nextDenoise;
        saveSetting("wfmDenoise", nextDenoise);
      }
    }
    if (wfmStFlagEl && typeof update.filter.wfm_stereo_detected === "boolean") {
      const detected = update.filter.wfm_stereo_detected;
      wfmStFlagEl.textContent = detected ? "ST" : "MO";
      wfmStFlagEl.classList.toggle("wfm-st-flag-stereo", detected);
      wfmStFlagEl.classList.toggle("wfm-st-flag-mono", !detected);
    }
    if (typeof update.filter.wfm_cci === "number") updateIntfBar(wfmCciFillEl, wfmCciValEl, update.filter.wfm_cci);
    if (typeof update.filter.wfm_aci === "number") updateIntfBar(wfmAciFillEl, wfmAciValEl, update.filter.wfm_aci);
    if (samStereoWidthEl && typeof update.filter.sam_stereo_width === "number") {
      samStereoWidthEl.value = String(Math.round(update.filter.sam_stereo_width * 100));
    }
    if (samCarrierSyncEl && typeof update.filter.sam_carrier_sync === "boolean") {
      const nextVal = update.filter.sam_carrier_sync ? "on" : "off";
      if (samCarrierSyncEl.value !== nextVal) samCarrierSyncEl.value = nextVal;
    }
    const hasSdrSquelchEnabled = typeof update.filter.sdr_squelch_enabled === "boolean";
    const hasSdrSquelchThreshold = typeof update.filter.sdr_squelch_threshold_db === "number";
    if (hasSdrSquelchEnabled || hasSdrSquelchThreshold) {
      sdrSquelchSupported = true;
      syncSdrSquelchFromServer(
        hasSdrSquelchEnabled ? update.filter.sdr_squelch_enabled : true,
        hasSdrSquelchThreshold ? update.filter.sdr_squelch_threshold_db : -120,
      );
    }
    updateSdrSquelchControlVisibility();
    const hasSdrNbEnabled = typeof update.filter.sdr_nb_enabled === "boolean";
    const hasSdrNbThreshold = typeof update.filter.sdr_nb_threshold === "number";
    if (hasSdrNbEnabled || hasSdrNbThreshold) {
      sdrNbSupported = true;
      if (sdrNbWrapEl) sdrNbWrapEl.style.display = "";
      if (sdrNbThresholdControlsEl) sdrNbThresholdControlsEl.style.display = "";
      if (hasSdrNbEnabled && sdrNbEnabledEl) {
        sdrNbEnabledEl.checked = update.filter.sdr_nb_enabled;
      }
      if (hasSdrNbThreshold && sdrNbThresholdEl && document.activeElement !== sdrNbThresholdEl) {
        sdrNbThresholdEl.value = String(Math.round(update.filter.sdr_nb_threshold));
      }
    }
  }
  if (typeof update.show_sdr_gain_control === "boolean") {
    if (sdrSettingsRowEl) sdrSettingsRowEl.style.display = update.show_sdr_gain_control ? "" : "none";
  }
  // Apply server-configured bandplan defaults once, only when the user has not
  // previously overridden the setting via the UI (localStorage).
  if (!_bandplanServerDefaultApplied && typeof update.bandplan_enabled === "boolean"
      && typeof update.bandplan_region === "string") {
    _bandplanServerDefaultApplied = true;
    const hasUserOverride = localStorage.getItem(STORAGE_PREFIX + "bandplanRegion") !== null;
    if (!hasUserOverride) {
      const region = update.bandplan_enabled ? update.bandplan_region : "off";
      bandplanRegion = region;
      saveSetting("bandplanRegion", region);
      if (bandplanRegionSelect) bandplanRegionSelect.value = region;
      bandplanSegmentsCache = null;
      bandplanCacheKey = "";
      if (lastSpectrumData) scheduleSpectrumDraw();
    }
  }
  if (update.filter && sdrAgcEl && typeof update.filter.sdr_agc_enabled === "boolean") {
    sdrAgcEl.checked = update.filter.sdr_agc_enabled;
    updateSdrGainInputState();
  }
  if (update.status && update.status.freq && typeof update.status.freq.hz === "number") {
    if (update.status.freq.hz !== prevRenderData.freqHz) {
      prevRenderData.freqHz = update.status.freq.hz;
      const sseHz = update.status.freq.hz;
      // While an optimistic set_freq is in flight, suppress SSE updates that
      // would snap the marker back to the stale server frequency.
      if (_freqOptimisticHz != null && Math.abs(sseHz - _freqOptimisticHz) > 1) {
        // stale — skip
      } else {
        if (_freqOptimisticHz != null && Math.abs(sseHz - _freqOptimisticHz) <= 1) {
          _freqOptimisticHz = null; // server confirmed — clear guard early
        }
        applyLocalTunedFrequency(sseHz);
      }
    }
  }
  if (update.status && update.status.mode && update.status.mode !== prevRenderData.mode) {
    prevRenderData.mode = update.status.mode;
    const mode = normalizeMode(update.status.mode);
    const modeUpper = mode ? mode.toUpperCase() : "";
    const onVirtual = typeof vchanIsOnVirtual === "function" && vchanIsOnVirtual();
    // When subscribed to a virtual channel the mode picker must reflect
    // that channel's mode, not the primary rig mode.  Skip the update here;
    // vchan.js will apply the correct mode via vchanSyncModeDisplay().
    if (!onVirtual) {
      modeEl.value = modeUpper;
      if (modeUpper === "WFM" && lastModeName !== "WFM") {
        setJogDivisor(10);
        resetRdsDisplay();
      } else if (modeUpper !== "WFM" && lastModeName === "WFM") {
        resetRdsDisplay();
      }
      lastModeName = modeUpper;
      // When filter panel is active (SDR backend), update the BW slider range
      // to match the new mode — but only if the server hasn't already sent a
      // filter state that overrides it.
      // When SDR backend is active (spectrum visible), apply BW default for new
      // mode — but only if the server hasn't already pushed a filter_state.
      if (lastSpectrumData && !update.filter) {
        applyBwDefaultForMode(mode, false);
      }
    }
    updateWfmControls();
    updateSdrSquelchControlVisibility();
  }
  const modeUpper = update.status && update.status.mode ? normalizeMode(update.status.mode).toUpperCase() : "";
  // Mode-bound decoder status (driven by registry).
  for (const d of decoderRegistry) {
    if (d.activation !== "mode_bound") continue;
    const el = document.getElementById(d.id + "-status");
    if (!el) continue;
    const connText = _decodeConnectedText[d.id] || "Connected, listening for packets";
    setModeBoundDecodeStatus(el, d.active_modes, "Select " + d.active_modes[0] + " mode to decode", connText);
  }
  if (window.updateAisBar) window.updateAisBar();
  if (window.updateVdesBar) window.updateVdesBar();
  if (window.updateAprsBar) window.updateAprsBar();
  if (window.updateFt8Bar) window.updateFt8Bar();
  // Toggle-gated decoder status: clear "Receiving" when decoder disabled or mode wrong.
  for (const d of decoderRegistry) {
    if (d.activation !== "toggle") continue;
    const key = d.id.replace(/-/g, "_") + "_decode_enabled";
    const enabled = !!update[key];
    const modeMatch = d.active_modes.includes(modeUpper);
    const el = document.getElementById(d.id + "-status");
    if (el && (!enabled || !modeMatch) && el.textContent === "Receiving") {
      el.textContent = "Connected, listening for packets";
    }
  }
  if (update.status && typeof update.status.tx_en === "boolean" && update.status.tx_en !== prevRenderData.txEn) {
    prevRenderData.txEn = update.status.tx_en;
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
  // Decoder toggle buttons: only write DOM when the enabled flag actually changes.
  _ensureDecoderToggles();
  for (const [key, entry] of Object.entries(_decoderToggles)) {
    syncDecoderToggle(entry, !!update[key], entry.label);
  }
  // Recorder state sync.
  if (typeof update.recorder_enabled === "boolean" && window._syncRecorderState) {
    window._syncRecorderState(update.recorder_enabled);
  }
  if (window.updateSatLiveState) window.updateSatLiveState(update);
  // cwAutoEl, cwWpmEl, cwToneEl are cached at module level
  if (cwWpmEl && typeof update.cw_wpm === "number") {
    cwWpmEl.value = update.cw_wpm;
  }
  if (cwToneEl && typeof update.cw_tone_hz === "number") {
    cwToneEl.value = update.cw_tone_hz;
  }
  if (typeof update.cw_auto === "boolean") {
    if (typeof window.applyCwAutoUiFromServer === "function") {
      // cw.js is loaded: use the guarded path that respects in-flight user
      // changes, preventing a concurrent SSE poll from re-enabling auto just
      // after the user disabled it.
      window.applyCwAutoUiFromServer(update.cw_auto);
    } else if (typeof window.applyCwAutoUi === "function") {
      window.applyCwAutoUi(update.cw_auto);
    } else {
      if (cwAutoEl) cwAutoEl.checked = update.cw_auto;
      if (cwWpmEl) { cwWpmEl.disabled = update.cw_auto; cwWpmEl.readOnly = update.cw_auto; }
      if (cwToneEl) { cwToneEl.disabled = update.cw_auto; cwToneEl.readOnly = update.cw_auto; }
    }
  }
  let activeFreqColor = "var(--accent-green)";
  if (update.status && update.status.vfo && Array.isArray(update.status.vfo.entries)) {
    const entries = update.status.vfo.entries;
    const activeIdx = Number.isInteger(update.status.vfo.active) ? update.status.vfo.active : null;
    vfoPicker.replaceChildren();
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
        activeFreqColor = color;
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
  if (freqEl) {
    freqEl.style.color = activeFreqColor;
  }
  if (update.status && update.status.rx && typeof update.status.rx.sig === "number") {
    if (update.status.rx.sig !== prevRenderData.sigDbm) {
      prevRenderData.sigDbm = update.status.rx.sig;
      const sUnits = dbmToSUnits(update.status.rx.sig);
      sigLastSUnits = sUnits;
      sigLastDbm = update.status.rx.sig;
      const pct = sUnits <= 9 ? Math.max(0, Math.min(100, (sUnits / 9) * 100)) : 100;
      signalBar.style.width = `${pct}%`;
      signalValue.innerHTML = formatSignal(sUnits);
      refreshSigStrengthDisplay();
    }
  } else if (prevRenderData.sigDbm !== null) {
    prevRenderData.sigDbm = null;
    sigLastSUnits = null;
    sigLastDbm = null;
    signalBar.style.width = "0%";
    signalValue.textContent = "--";
    refreshSigStrengthDisplay();
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
  // Populate About tab — only update DOM when the about tab is visible
  if (_activeTab === "about") {
    // About — Server card (uses cached DOM refs)
    if (update.server_version && aboutServerVerEl) {
      aboutServerVerEl.textContent = `trx-server v${update.server_version}`;
    }
    if (update.server_build_date && aboutServerBuildDateEl) {
      aboutServerBuildDateEl.textContent = update.server_build_date;
    }
    if (aboutServerAddrEl) aboutServerAddrEl.textContent = location.host;
    if (update.server_callsign && aboutServerCallEl) {
      aboutServerCallEl.textContent = update.server_callsign;
    }
    if (Number.isFinite(serverLat) && Number.isFinite(serverLon) && aboutServerLocationEl) {
      const grid = latLonToMaidenhead(serverLat, serverLon);
      aboutServerLocationEl.textContent = `${grid} (${serverLat.toFixed(4)}, ${serverLon.toFixed(4)})`;
    }

    // About — Radio card
    if (update.info) {
      const parts = [update.info.manufacturer, update.info.model, update.info.revision].filter(Boolean).join(" ");
      if (parts && aboutRigInfoEl) aboutRigInfoEl.textContent = parts;
      const access = update.info.access;
      if (access) {
        if (access.Serial) {
          const serialPath = access.Serial.path || access.Serial.port || "?";
          if (aboutRigAccessEl) aboutRigAccessEl.textContent = `Serial (${serialPath}, ${access.Serial.baud || "?"} baud)`;
        } else if (access.Tcp) {
          if (aboutRigAccessEl) aboutRigAccessEl.textContent = `TCP (${access.Tcp.host || "?"}:${access.Tcp.port || "?"})`;
        } else {
          const key = Object.keys(access)[0];
          if (key && aboutRigAccessEl) aboutRigAccessEl.textContent = key;
        }
      }
      if (update.info.capabilities) {
        const cap = update.info.capabilities;
        if (Array.isArray(cap.supported_modes) && cap.supported_modes.length && aboutModesEl) {
          aboutModesEl.textContent = cap.supported_modes.map(normalizeMode).filter(Boolean).join(", ");
        }
        if (typeof cap.num_vfos === "number" && aboutVfosEl) {
          aboutVfosEl.textContent = cap.num_vfos;
        }
      }
    }
    if (lastActiveRigId && aboutActiveRigEl) {
      aboutActiveRigEl.textContent = lastActiveRigId;
    }

    // About — Audio card
    if (streamInfo) {
      if (aboutAudioCodecEl) aboutAudioCodecEl.textContent = "Opus";
      if (aboutAudioSamplerateEl) aboutAudioSamplerateEl.textContent = `${(streamInfo.sample_rate || 48000).toLocaleString()} Hz`;
      if (aboutAudioChannelsEl) aboutAudioChannelsEl.textContent = (streamInfo.channels || 1) === 1 ? "Mono" : "Stereo";
      if (streamInfo.bitrate_bps && aboutAudioBitrateEl) {
        const kbps = (streamInfo.bitrate_bps / 1000).toFixed(0);
        aboutAudioBitrateEl.textContent = `${kbps} kbps`;
      }
      if (streamInfo.frame_duration_ms && aboutAudioFrameEl) {
        aboutAudioFrameEl.textContent = `${streamInfo.frame_duration_ms} ms`;
      }
    }
    if (aboutAudioRxEl) aboutAudioRxEl.textContent = rxActive ? "Active" : "Off";
    if (typeof update.audio_clients === "number" && aboutAudioStreamsEl) {
      aboutAudioStreamsEl.textContent = update.audio_clients;
    }

    // About — Decoders card (only update when values change)
    syncAboutDecoder(0, !!update.ft8_decode_enabled);
    syncAboutDecoder(1, !!update.ft4_decode_enabled);
    syncAboutDecoder(2, !!update.ft2_decode_enabled);
    syncAboutDecoder(3, !!update.wspr_decode_enabled);
    syncAboutDecoder(4, !!update.cw_decode_enabled);
    syncAboutDecoder(5, !!(update.aprs_decode_enabled || update.hf_aprs_decode_enabled));
    syncAboutDecoder(6, !!update.lrpt_decode_enabled);

    // About — Integrations card
    if (update.pskreporter_status && aboutPskreporterEl) {
      aboutPskreporterEl.textContent = update.pskreporter_status;
    }
    if (update.aprs_is_status && aboutAprsIsEl) {
      aboutAprsIsEl.textContent = update.aprs_is_status;
    }
    if (typeof update.rigctl_clients === "number" && aboutRigctlClientsEl) {
      aboutRigctlClientsEl.textContent = update.rigctl_clients;
    }
    if (typeof update.rigctl_addr === "string" && update.rigctl_addr.length > 0 && aboutRigctlEndpointEl) {
      aboutRigctlEndpointEl.textContent = update.rigctl_addr;
    }

    // About — Clients card
    if (typeof update.clients === "number" && aboutClientsEl) {
      aboutClientsEl.textContent = update.clients;
    }
  } // end _activeTab === "about"
  if (Array.isArray(update.remotes)) {
    applyRigList(update.active_remote, update.remotes);
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
    const statusUrl = lastActiveRigId
      ? `/status?remote=${encodeURIComponent(lastActiveRigId)}`
      : "/status";
    const resp = await fetch(statusUrl, { cache: "no-store" });
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
    sseSessionId = null;
  }
  if (esHeartbeat) {
    clearInterval(esHeartbeat);
  }
  pollFreshSnapshot();
  const eventsUrl = lastActiveRigId
    ? `/events?remote=${encodeURIComponent(lastActiveRigId)}`
    : "/events";
  es = new EventSource(eventsUrl);
  lastEventAt = Date.now();
  es.onopen = () => {
    setConnLostOverlay(false);
    if (tabMainEl) tabMainEl.classList.remove("server-disconnected");
    if (!aboutUptimeStart) aboutUptimeStart = Date.now();
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
      if (data.server_connected === false) {
        powerHint.textContent = "trx-server connection lost";
        if (tabMainEl) tabMainEl.classList.add("server-disconnected");
      } else {
        if (tabMainEl) tabMainEl.classList.remove("server-disconnected");
        if (data.initialized) powerHint.textContent = readyText();
      }
    } catch (e) {
      console.error("Bad event data", e);
    }
  };
  es.addEventListener("ping", () => {
    lastEventAt = Date.now();
  });
  es.addEventListener("session", evt => {
    try {
      const d = JSON.parse(evt.data);
      sseSessionId = d.session_id || null;
    } catch (_) {}
    if (typeof vchanHandleSession === "function") vchanHandleSession(evt.data);
  });
  es.addEventListener("channels", evt => {
    if (typeof vchanHandleChannels === "function") vchanHandleChannels(evt.data);
  });
  es.onerror = () => {
    // Check if this is an auth error by looking at readyState
    if (es.readyState === EventSource.CLOSED) {
      powerHint.textContent = "trx-client connection lost, retrying\u2026";
      setConnLostOverlay(true, "trx-client connection lost", "Retrying\u2026", true);
      es.close();
      pollFreshSnapshot();
      scheduleReconnect(1000);
    }
  };

  esHeartbeat = setInterval(() => {
    const now = Date.now();
    if (now - lastEventAt > 15000) {
      powerHint.textContent = "trx-client connection lost, retrying\u2026";
      setConnLostOverlay(true, "trx-client connection lost", "Retrying\u2026", true);
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
  setDecodeHistoryOverlayVisible(false);
  setConnLostOverlay(false);
}

// Yield the main thread so the browser can paint before heavy async work.
// Uses scheduler.yield() (Chrome 115+) with a setTimeout fallback.
function yieldToMain() {
  if (typeof scheduler !== "undefined" && typeof scheduler.yield === "function") {
    return scheduler.yield();
  }
  return new Promise((resolve) => setTimeout(resolve, 0));
}

const uiFrameJobs = new Map();
let uiFrameJobsHandle = null;

function flushUiFrameJobs() {
  uiFrameJobsHandle = null;
  const jobs = Array.from(uiFrameJobs.values());
  uiFrameJobs.clear();
  for (const job of jobs) {
    try {
      job();
    } catch (err) {
      console.error("Deferred UI job failed:", err);
    }
  }
}

function scheduleUiFrameJob(key, job) {
  if (typeof job !== "function") return;
  uiFrameJobs.set(key, job);
  if (uiFrameJobsHandle !== null) return;
  if (typeof requestAnimationFrame === "function") {
    uiFrameJobsHandle = requestAnimationFrame(flushUiFrameJobs);
  } else {
    uiFrameJobsHandle = setTimeout(flushUiFrameJobs, 16);
  }
}

window.trxScheduleUiFrameJob = scheduleUiFrameJob;

async function postPath(path) {
  // Auto-append remote so each tab targets its own rig.
  // Skip when the caller already included remote (e.g. /select_rig).
  if (lastActiveRigId && !path.includes("remote=")) {
    const sep = path.includes("?") ? "&" : "?";
    path = `${path}${sep}remote=${encodeURIComponent(lastActiveRigId)}`;
  }
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

async function takeSchedulerControlForDecoderDisable(buttonEl) {
  const enabled = buttonEl?.dataset?.enabled === "true"
    || /^\s*Disable\b/i.test(buttonEl?.textContent || "");
  if (!enabled) return;
  if (typeof window.vchanTakeSchedulerControl === "function") {
    await window.vchanTakeSchedulerControl();
  }
}
window.takeSchedulerControlForDecoderDisable = takeSchedulerControlForDecoderDisable;

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
  const prevRig = lastActiveRigId;
  lastActiveRigId = selectEl.value;
  if (prevRig && prevRig !== lastActiveRigId) {
    resetDecoderStateOnRigSwitch();
  }
  updateRigSubtitle(lastActiveRigId);
  if (typeof setSchedulerRig === "function") setSchedulerRig(lastActiveRigId);
  if (typeof setBackgroundDecodeRig === "function") setBackgroundDecodeRig(lastActiveRigId);
  if (typeof bmFetch === "function") bmFetch(document.getElementById("bm-category-filter")?.value || "");
  // Reconnect decode stream so history + live messages filter to the new rig.
  connectDecode();
  // Switch this session's rig and reconnect SSE to the new rig's
  // state channel.
  try {
    const sidParam = sseSessionId ? `&session_id=${encodeURIComponent(sseSessionId)}` : "";
    await postPath(`/select_rig?remote=${encodeURIComponent(selectEl.value)}${sidParam}`);
    connect();
  } catch (err) {
    console.error("select_rig failed:", err);
  }
  // Reconnect spectrum SSE to the new rig's spectrum channel.
  stopSpectrumStreaming();
  startSpectrumStreaming();
  // Reconnect audio to the new rig if audio is active.
  if (rxActive) {
    stopRxAudio();
    startRxAudio();
  }
  showHint(`Rig: ${lastActiveRigId}`, 1500);
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

function applyFreqFromInput() {
  const parsedRaw = parseFreqInput(freqEl.value, jogUnit);
  const parsed = alignFreqToRigStep(parsedRaw);
  if (parsed === null) {
    showHint("Freq missing", 1500);
    return;
  }
  if (!freqAllowed(parsed)) {
    showUnsupportedFreqPopup(parsed);
    return;
  }
  freqDirty = false;
  // setRigFrequency is fire-and-forget; visual update is instant.
  setRigFrequency(parsed);
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
    showUnsupportedFreqPopup(parsed);
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
  } else if (e.key === "Escape") {
    freqDirty = false;
    refreshFreqDisplay();
    freqEl.blur();
  }
});
freqEl.addEventListener("blur", () => {
  if (freqDirty) {
    freqDirty = false;
    refreshFreqDisplay();
  }
});
if (centerFreqEl) {
  centerFreqEl.addEventListener("keydown", (e) => {
    centerFreqDirty = true;
    if (e.key === "Enter") {
      e.preventDefault();
      applyCenterFreqFromInput();
    } else if (e.key === "Escape") {
      centerFreqDirty = false;
      refreshCenterFreqDisplay();
      centerFreqEl.blur();
    }
  });
  centerFreqEl.addEventListener("blur", () => {
    if (centerFreqDirty) {
      centerFreqDirty = false;
      refreshCenterFreqDisplay();
    }
  });
  centerFreqEl.addEventListener("wheel", (e) => {
    e.preventDefault();
    const direction = e.deltaY < 0 ? 1 : -1;
    jogFreq(direction);
  }, { passive: false });
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

function jogFreq(direction) {
  if (lastLocked) { showHint("Locked", 1500); return; }
  if (lastFreqHz === null) return;
  const newHz = alignFreqToRigStep(lastFreqHz + direction * jogStep);
  if (!freqAllowed(newHz)) {
    showUnsupportedFreqPopup(newHz);
    return;
  }
  jogAngle = (jogAngle + direction * 15) % 360;
  jogIndicator.style.transform = `translateX(-50%) rotate(${jogAngle}deg)`;
  // setRigFrequency is fire-and-forget; visual update is instant.
  setRigFrequency(newHz);
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
    if (typeof vchanInterceptMode === "function" && await vchanInterceptMode(mode)) {
      showHint("Channel mode set", 1500);
      return;
    }
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
  CW:     [500,    100,   9_000,  50],
  CWR:    [500,    100,   9_000,  50],
  LSB:    [2_700,  300,   6_000,  100],
  USB:    [2_700,  300,   6_000,  100],
  AM:     [9_000,  500,   20_000, 500],
  SAM:    [9_000,  500,   20_000, 500],
  FM:     [12_500, 2_500, 25_000, 500],
  AIS:    [25_000, 12_500, 50_000, 500],
  VDES:   [100_000, 25_000, 200_000, 1_000],
  WFM:    [180_000, 50_000,300_000,5_000],
  DIG:    [3_000,  300,   6_000,  100],
  PKT:    [25_000, 300,  50_000,  500],
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
window.currentBandwidthHz = currentBandwidthHz;
const spectrumBwInput = document.getElementById("spectrum-bw-input");
const spectrumBwSetBtn = document.getElementById("spectrum-bw-set-btn");
const spectrumBwAutoBtn = document.getElementById("spectrum-bw-auto-btn");
const spectrumBwSweetBtn = document.getElementById("spectrum-bw-sweet-btn");

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
  window.currentBandwidthHz = currentBandwidthHz;
  syncBandwidthInput(def);
  positionFastOverlay(lastFreqHz, def);
  if (lastSpectrumData) {
    scheduleSpectrumDraw();
  }
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
  window.currentBandwidthHz = currentBandwidthHz;
  syncBandwidthInput(clamped);
  positionFastOverlay(lastFreqHz, clamped);
  if (lastSpectrumData) {
    scheduleSpectrumDraw();
  }
  try {
    if (typeof vchanInterceptBandwidth === "function" && await vchanInterceptBandwidth(clamped)) return;
    await postPath(`/set_bandwidth?hz=${clamped}`);
    if (Number.isFinite(lastFreqHz)) {
      await ensureTunedBandwidthCoverage(lastFreqHz);
    }
  } catch (_) {}
}

function estimateBandwidthAroundPeak(data, centerHz) {
  if (!data || !isBinsArray(data.bins) || data.bins.length < 3 || !Number.isFinite(centerHz)) {
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
  window.currentBandwidthHz = currentBandwidthHz;
  syncBandwidthInput(estimated);
  positionFastOverlay(lastFreqHz, estimated);
  if (lastSpectrumData) {
    scheduleSpectrumDraw();
  }
  try {
    if (typeof vchanInterceptBandwidth === "function" && await vchanInterceptBandwidth(estimated)) return;
    await postPath(`/set_bandwidth?hz=${estimated}`);
    if (Number.isFinite(lastFreqHz)) {
      await ensureTunedBandwidthCoverage(lastFreqHz);
    }
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
if (spectrumBwSweetBtn) {
  spectrumBwSweetBtn.addEventListener("click", () => { applySweetSpotCenter().catch(() => {}); });
}

// --- Tab navigation ---
let _activeTab = "main"; // tracked for render-path tab awareness
const TAB_ORDER = ["main", "bookmarks", "digital-modes", "map", "statistics", "recorder", "settings", "about"];
const TAB_PATHS = {
  main: "/",
  bookmarks: "/bookmarks",
  "digital-modes": "/digital-modes",
  map: "/map",
  recorder: "/recorder",
  settings: "/settings",
  about: "/about",
};

function normalizeTabPath(pathname) {
  const raw = typeof pathname === "string" && pathname.length > 0 ? pathname : "/";
  if (raw === "/") return "/";
  return raw.replace(/\/+$/, "") || "/";
}

function tabFromPath(pathname = window.location.pathname) {
  const normalized = normalizeTabPath(pathname);
  for (const [tabName, tabPath] of Object.entries(TAB_PATHS)) {
    if (normalized === tabPath) return tabName;
  }
  return "main";
}

function updateTabHistory(name, replaceHistory = false) {
  const targetPath = TAB_PATHS[name] || "/";
  if (normalizeTabPath(window.location.pathname) === targetPath) return;
  const nextUrl = `${targetPath}${window.location.search}${window.location.hash}`;
  const method = replaceHistory ? "replaceState" : "pushState";
  window.history[method]({}, "", nextUrl);
}

function navigateToTab(name, options = {}) {
  const { updateHistory = true, replaceHistory = false } = options;
  if (authEnabled && !authRole && name !== "main") return;
  const btn = document.querySelector(`.tab-bar .tab[data-tab="${name}"]`);
  if (!btn) return;
  _activeTab = name;
  document.querySelectorAll(".tab-bar .tab").forEach((t) => t.classList.remove("active"));
  btn.classList.add("active");
  document.querySelectorAll(".tab-panel").forEach((p) => p.style.display = "none");
  document.getElementById(`tab-${name}`).style.display = "";
  if (updateHistory) {
    updateTabHistory(name, replaceHistory);
  }
  scheduleSpectrumLayout();
  if (typeof window.loadPluginsForTab === "function") window.loadPluginsForTab(name);
  if (name === "map") {
    window.trx.map?.initAprsMap();
    window.trx.map?.sizeAprsMapToViewport();
    if (window.trx.map?.aprsMap) setTimeout(() => window.trx.map.aprsMap.invalidateSize(), 50);
  }
  if (name === "statistics") {
    window.trx.map?.scheduleStatsRender();
  }
  if (name === "recorder") {
    refreshRecorderStatus();
  }
}

document.querySelector(".tab-bar").addEventListener("click", (e) => {
  const btn = e.target.closest(".tab[data-tab]");
  if (!btn) return;
  navigateToTab(btn.dataset.tab);
});

window.addEventListener("popstate", () => {
  navigateToTab(tabFromPath(), { updateHistory: false });
});

// Swipe left/right on the main content area to switch tabs (mobile).
(function () {
  let tx = 0, ty = 0;
  const THRESHOLD = 60;          // px horizontal movement required
  const ANGLE_LIMIT = 1.6;       // |dx/dy| ratio — suppress on near-vertical drags

  // Elements where horizontal drag has its own meaning; exclude from swipe.
  const NO_SWIPE_SELECTORS = [
    "#jog-wheel", "#spectrum-canvas", "#overview-canvas",
    "#aprs-map", ".controls-tray-scroll", ".sub-tab-bar",
    "input[type=range]", "select", "input[type=text]",
    "input[type=number]", "input[type=search]",
  ];

  function isExcluded(el) {
    return NO_SWIPE_SELECTORS.some((sel) => el.closest(sel));
  }

  document.addEventListener("touchstart", (e) => {
    if (e.touches.length !== 1) return;
    if (isExcluded(e.target)) return;
    tx = e.touches[0].clientX;
    ty = e.touches[0].clientY;
  }, { passive: true });

  document.addEventListener("touchend", (e) => {
    if (e.changedTouches.length !== 1 || tx === 0) return;
    const dx = e.changedTouches[0].clientX - tx;
    const dy = e.changedTouches[0].clientY - ty;
    tx = 0;
    if (Math.abs(dx) < THRESHOLD) return;
    if (Math.abs(dy) > 0 && Math.abs(dx) / Math.abs(dy) < ANGLE_LIMIT) return;
    const activeBtn = document.querySelector(".tab-bar .tab.active");
    if (!activeBtn) return;
    const cur = TAB_ORDER.indexOf(activeBtn.dataset.tab);
    if (cur === -1) return;
    const next = dx < 0 ? cur + 1 : cur - 1;
    if (next >= 0 && next < TAB_ORDER.length) navigateToTab(TAB_ORDER[next]);
  }, { passive: true });
})();

window.addEventListener("resize", () => { scheduleSpectrumLayout(); });

// --- Auth startup sequence ---
function getAvailableRigIds() {
  return lastRigIds || [];
}

async function initializeApp() {
  showAuthGate(false);
  const authStatus = await checkAuthStatus();
  authEnabled = !authStatus.auth_disabled;

  if (!authEnabled) {
    authRole = "control";
    hideAuthGate();
    updateAuthUI();
    connect();
    connectDecode();
    initSettingsUI();
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
    connectDecode();
    initSettingsUI();
    resizeHeaderSignalCanvas();
    startHeaderSignalSampling();
  } else {
    // No valid session - show auth gate
    // Guest button is shown if guest mode is available (role granted without auth)
    const allowGuest = authStatus.role === "rx";
    showAuthGate(allowGuest);
  }
}

function initSettingsUI() {
  if (typeof initScheduler === "function") {
    initScheduler(lastActiveRigId, authRole);
    wireSchedulerEvents();
  }
  if (typeof initBackgroundDecode === "function") {
    initBackgroundDecode(lastActiveRigId, authRole);
    wireBackgroundDecodeEvents();
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
    connectDecode();
    initSettingsUI();
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
    connectDecode();
    initSettingsUI();
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

// ── Shared namespace for lazy-loaded modules ────────────────────────────────
// Modules (map-core.js, screenshot.js) access core state and utilities via
// window.trx.  Modules register their own APIs as sub-namespaces
// (e.g. window.trx.map, window.trx.screenshot).
window.trx = Object.create(null);
// -- State getters (backed by core-scoped variables) --
Object.defineProperties(window.trx, {
  serverLat:              { get() { return serverLat;              }, set(v) { serverLat = v; } },
  serverLon:              { get() { return serverLon;              }, set(v) { serverLon = v; } },
  lastFreqHz:             { get() { return lastFreqHz;             } },
  lastActiveRigId:        { get() { return lastActiveRigId;        } },
  lastRigIds:             { get() { return lastRigIds;             } },
  lastRigDisplayNames:    { get() { return lastRigDisplayNames;    } },
  initialMapZoom:         { get() { return initialMapZoom;         } },
  decodeHistoryRetentionMin: { get() { return decodeHistoryRetentionMin; } },
  authRole:               { get() { return authRole;               } },
  decoderRegistry:        { get() { return decoderRegistry;        } },
  sseSessionId:           { get() { return sseSessionId;           } },
  primaryRds:             { get() { return primaryRds;             } },
  vchanRdsById:           { get() { return vchanRdsById;           } },
  vchanSignalDbById:      { get() { return vchanSignalDbById;      } },
  lastCityLabel:          { get() { return lastCityLabel;          }, set(v) { lastCityLabel = v; } },
  serverVersion:          { get() { return serverVersion;          } },
  serverBuildDate:        { get() { return serverBuildDate;        } },
  serverCallsign:         { get() { return serverCallsign;        } },
  ownerCallsign:          { get() { return ownerCallsign;         } },
  ownerWebsiteUrl:        { get() { return ownerWebsiteUrl;       } },
  ownerWebsiteName:       { get() { return ownerWebsiteName;      } },
  aisVesselUrlBase:       { get() { return aisVesselUrlBase;      } },
  serverRigs:             { get() { return serverRigs;            } },
  serverActiveRigId:      { get() { return serverActiveRigId;     } },
  lastModeName:           { get() { return lastModeName;          } },
  lastSpectrumData:       { get() { return lastSpectrumData;      } },
  lastSpectrumRenderData: { get() { return lastSpectrumRenderData; } },
  currentBandwidthHz:     { get() { return currentBandwidthHz;     }, set(v) { currentBandwidthHz = v; window.currentBandwidthHz = v; } },
  spectrumFloor:          { get() { return spectrumFloor;         } },
  spectrumRange:          { get() { return spectrumRange;         } },
  spectrumCanvas:         { get() { return spectrumCanvas;        } },
  overviewCanvas:         { get() { return overviewCanvas;        } },
  overviewGl:             { get() { return overviewGl;            } },
  spectrumGl:             { get() { return spectrumGl;            } },
  signalOverlayGl:        { get() { return signalOverlayGl;      } },
});
// -- Shared utility functions --
Object.assign(window.trx, {
  saveSetting, loadSetting, showHint, escapeMapHtml, formatFreq, formatFreqForHumans,
  formatWavelength, formatBwLabel, formatUptime, formatSigStrength, formatSignal,
  postPath, scheduleUiFrameJob, navigateToTab, rigBadgeColor,
  latLonToMaidenhead, locatorToLatLon, haversineKm, formatDistanceKm,
  formatTimeAgo, bookmarkDistanceText, buildBookmarkTooltipText,
  nearestBookmarkForHz, currentDecodeHistoryRetentionMs,
  currentTheme, canvasPalette, currentStyle,
  cssColorToRgba, rgbaWithAlpha, isBinsArray, estimateNoiseFloorDb,
  spectrumVisibleRange, drawSpectrum,
  bandForHz: function(hz) { return window.trx.map?.bandForHz?.(hz); },
  markDecodeMapSyncPending,
  decodeHistoryMapRenderingDeferred,
  updateDocumentTitle,
  activeChannelRds,
});
Object.defineProperties(window.trx, {
  decodeHistoryReplayActive: { get() { return decodeHistoryReplayActive; } },
  decodeMapSyncPending:      { get() { return decodeMapSyncPending; } },
  _activeTab:                { get() { return _activeTab; } },
  locationSubtitle:          { get() { return locationSubtitle; } },
});

// Start the app
initializeApp();
window.addEventListener("resize", resizeHeaderSignalCanvas);


// ── Map module (extracted to map-core.js, lazy-loaded) ──────────────────────
// The map, statistics, and geolocation code (~3,450 lines) has been moved to
// map-core.js and is loaded on demand when the Map tab is first activated.
// Core communicates with the map module via window.trx.map.* namespace.

// ── Geo utilities (shared with map-core.js via window.trx) ─────────────────
function haversineKm(lat1, lon1, lat2, lon2) {
  const R = 6371;
  const dLat = (lat2 - lat1) * Math.PI / 180;
  const dLon = (lon2 - lon1) * Math.PI / 180;
  const a = Math.sin(dLat / 2) ** 2
    + Math.cos(lat1 * Math.PI / 180) * Math.cos(lat2 * Math.PI / 180) * Math.sin(dLon / 2) ** 2;
  return R * 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
}

function locatorToLatLon(locator) {
  const raw = String(locator || "").trim().toUpperCase();
  if (!/^[A-R]{2}\d{2}([A-X]{2})?$/.test(raw)) return null;
  let lon = -180;
  let lat = -90;
  lon += (raw.charCodeAt(0) - 65) * 20;
  lat += (raw.charCodeAt(1) - 65) * 10;
  lon += Number(raw.slice(2, 3)) * 2;
  lat += Number(raw.slice(3, 4));
  if (raw.length >= 6) {
    lon += (raw.charCodeAt(4) - 65) * (5 / 60);
    lat += (raw.charCodeAt(5) - 65) * (2.5 / 60);
    lon += 2.5 / 60;
    lat += 1.25 / 60;
  } else {
    lon += 1;
    lat += 0.5;
  }
  return { lat, lon };
}

function formatDistanceKm(distKm) {
  if (!Number.isFinite(distKm)) return null;
  return distKm < 1 ? `${Math.round(distKm * 1000)} m` : `${distKm.toFixed(1)} km`;
}

function bookmarkDistanceText(bm) {
  if (!bm || serverLat == null || serverLon == null) return null;
  const latLon = locatorToLatLon(bm.locator);
  if (!latLon) return null;
  return formatDistanceKm(haversineKm(serverLat, serverLon, latLon.lat, latLon.lon));
}

function buildBookmarkTooltipText(bm) {
  if (!bm) return null;
  const parts = [];
  if (bm.name) parts.push(String(bm.name));
  if (typeof bmFmtFreq === "function") parts.push(bmFmtFreq(bm.freq_hz));
  if (bm.mode) parts.push(String(bm.mode));
  if (bm.locator) parts.push(String(bm.locator));
  const distance = bookmarkDistanceText(bm);
  if (distance) parts.push(distance);
  let text = parts.join(" · ");
  if (bm.comment) {
    text += (text ? "\n" : "") + String(bm.comment);
  }
  return text;
}

function nearestBookmarkForHz(hz, widthPx, range) {
  const ref = typeof bmOverlayList !== "undefined" ? bmOverlayList : null;
  if (!Array.isArray(ref) || !Number.isFinite(hz) || !widthPx || !range || !Number.isFinite(range.visSpanHz) || range.visSpanHz <= 0) {
    return null;
  }
  const maxDeltaHz = Math.max((range.visSpanHz / widthPx) * 6, 10);
  let best = null;
  let bestDelta = Number.POSITIVE_INFINITY;
  for (const bm of ref) {
    const delta = Math.abs(Number(bm.freq_hz) - hz);
    if (delta <= maxDeltaHz && delta < bestDelta) {
      best = bm;
      bestDelta = delta;
    }
  }
  return best;
}

function formatTimeAgo(tsMs) {
  if (!tsMs) return null;
  const secs = Math.round((Date.now() - tsMs) / 1000);
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins} min ago`;
  const hrs = Math.floor(mins / 60);
  const remMins = mins % 60;
  return remMins > 0 ? `${hrs}h ${remMins}min ago` : `${hrs}h ago`;
}


function latLonToMaidenhead(lat, lon) {
  const adjustedLon = lon + 180;
  const adjustedLat = lat + 90;
  const A = "A".charCodeAt(0);
  const a = "a".charCodeAt(0);
  const field1 = String.fromCharCode(A + Math.floor(adjustedLon / 20));
  const field2 = String.fromCharCode(A + Math.floor(adjustedLat / 10));
  const square1 = Math.floor((adjustedLon % 20) / 2);
  const square2 = Math.floor(adjustedLat % 10);
  const sub1 = String.fromCharCode(a + Math.floor((adjustedLon % 2) * 12));
  const sub2 = String.fromCharCode(a + Math.floor((adjustedLat % 1) * 24));
  return `${field1}${field2}${square1}${square2}${sub1}${sub2}`;
}


// --- Sub-tab navigation ---
document.querySelectorAll(".sub-tab-bar").forEach((bar) => {
  bar.addEventListener("click", (e) => {
    const btn = e.target.closest(".sub-tab[data-subtab]");
    if (!btn) return;
    bar.querySelectorAll(".sub-tab").forEach((t) => t.classList.remove("active"));
    btn.classList.add("active");
    const parent = bar.parentElement;
    parent.querySelectorAll(".sub-tab-panel").forEach((p) => p.style.display = "none");
    const nextPanel = parent.querySelector(`#subtab-${btn.dataset.subtab}`);
    if (nextPanel) nextPanel.style.display = "";
    if (btn.dataset.subtab === "cw" && window.refreshCwTonePicker) {
      requestAnimationFrame(() => {
        if (window.refreshCwTonePicker) window.refreshCwTonePicker();
      });
    }
    // Clear SAT prediction DOM when leaving the SAT tab to reduce node count.
    if (btn.dataset.subtab !== "sat" && typeof window.clearSatPredictionDom === "function") {
      window.clearSatPredictionDom();
    }
  });
});

window.addEventListener("resize", () => {
  const mapTab = document.getElementById("tab-map");
  if (!mapTab || mapTab.style.display === "none") return;
  window.trx.map?.sizeAprsMapToViewport();
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
      sigResult.innerHTML = `Avg ${formatSignal(avg)} / Peak ${formatSignal(peak)} (${(sigMeasureAccumMs / 1000).toFixed(1)}s)`;
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
const RX_AUDIO_LABEL = "Play Audio";
const TX_AUDIO_LABEL = "Transmit Audio";
const audioStatus = document.getElementById("audio-status");
const audioLevelFill = document.getElementById("audio-level-fill");
const audioRow = document.getElementById("audio-row");
const wfmControlsCol = document.getElementById("wfm-controls-col");
const wfmDeemphasisEl = document.getElementById("wfm-deemphasis");
const wfmAudioModeEl = document.getElementById("wfm-audio-mode");
const wfmDenoiseEl = document.getElementById("wfm-denoise");
const sdrSettingsRowEl = document.getElementById("sdr-settings-row");
const sdrGainControlsEl = document.getElementById("sdr-gain-controls");
const sdrGainEl = document.getElementById("sdr-gain-db");
const sdrGainSetBtn = document.getElementById("sdr-gain-set");
const sdrLnaGainControlsEl = document.getElementById("sdr-lna-gain-controls");
const sdrLnaGainEl = document.getElementById("sdr-lna-gain-db");
const sdrLnaGainSetBtn = document.getElementById("sdr-lna-gain-set");
const sdrAgcEl = document.getElementById("sdr-agc-enabled");
const wfmStFlagEl = document.getElementById("wfm-st-flag");
const wfmCciFillEl = document.getElementById("wfm-cci-fill");
const wfmCciValEl = document.getElementById("wfm-cci-val");
const wfmAciFillEl = document.getElementById("wfm-aci-fill");
const wfmAciValEl = document.getElementById("wfm-aci-val");
const samControlsCol = document.getElementById("sam-controls-col");
const samStereoWidthEl = document.getElementById("sam-stereo-width");
const samCarrierSyncEl = document.getElementById("sam-carrier-sync");
const sdrSquelchWrapEl = document.getElementById("sdr-squelch-wrap");
const sdrSquelchEl = document.getElementById("sdr-squelch");
const sdrSquelchPctEl = document.getElementById("sdr-squelch-pct");
const SDR_SQUELCH_MIN_DB = -120;
const SDR_SQUELCH_MAX_DB = -30;
let syncFromServerSdrSquelch = false;
const sdrNbWrapEl = document.getElementById("sdr-nb-wrap");
const sdrNbEnabledEl = document.getElementById("sdr-nb-enabled");
const sdrNbThresholdControlsEl = document.getElementById("sdr-nb-threshold-controls");
const sdrNbThresholdEl = document.getElementById("sdr-nb-threshold");
const sdrNbThresholdSetBtn = document.getElementById("sdr-nb-threshold-set");
let sdrNbSupported = false;

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
let wasmOpusDecoder = null;
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
const hasWasmOpus = typeof window["opus-decoder"] !== "undefined" && typeof window["opus-decoder"].OpusDecoder !== "undefined";
const MAX_RX_BUFFER_SECS = 0.25;
const TARGET_RX_BUFFER_SECS = 0.04;
const MIN_RX_JITTER_SAMPLES = 512;

if (rxAudioBtn) {
  rxAudioBtn.textContent = RX_AUDIO_LABEL;
  rxAudioBtn.setAttribute("aria-label", RX_AUDIO_LABEL);
}
if (txAudioBtn) {
  txAudioBtn.textContent = TX_AUDIO_LABEL;
  txAudioBtn.setAttribute("aria-label", TX_AUDIO_LABEL);
}

function setAudioLevel(levelPct) {
  if (!audioLevelFill) return;
  const clamped = Math.max(0, Math.min(100, Number.isFinite(levelPct) ? levelPct : 0));
  audioLevelFill.style.width = `${clamped}%`;
}

// Create/resume the output context from a direct user gesture so Chromium
// does not leave playback suspended until a later click.
function ensureRxAudioContext(preferredSampleRate) {
  if (!audioCtx) {
    try {
      audioCtx = Number.isFinite(preferredSampleRate) && preferredSampleRate > 0
        ? new AudioContext({ sampleRate: preferredSampleRate })
        : new AudioContext();
    } catch (e) {
      audioCtx = new AudioContext();
    }
  }
  audioCtx.resume().catch(() => {});
  if (!rxGainNode) {
    rxGainNode = audioCtx.createGain();
    rxGainNode.connect(audioCtx.destination);
  }
}

function levelFromChannels(channels, frameCount) {
  if (!Array.isArray(channels) || channels.length === 0 || !Number.isFinite(frameCount) || frameCount <= 0) {
    return 0;
  }
  let sumSquares = 0;
  let samples = 0;
  for (const channel of channels) {
    if (!channel) continue;
    const limit = Math.min(frameCount, channel.length);
    for (let i = 0; i < limit; i++) {
      const sample = channel[i];
      sumSquares += sample * sample;
    }
    samples += limit;
  }
  if (samples <= 0) return 0;
  const rms = Math.sqrt(sumSquares / samples);
  return Math.min(100, rms * 220);
}

function normalizeWfmDenoiseLevel(value) {
  const next = String(value ?? "").toLowerCase();
  if (next === "off" || next === "auto" || next === "low" || next === "medium" || next === "high") return next;
  return "auto";
}

function clampSdrSquelchPercent(value) {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, Math.round(value)));
}

function sdrSquelchPercentToServer(percent) {
  const pct = clampSdrSquelchPercent(percent);
  if (pct <= 0) {
    return { enabled: false, thresholdDb: SDR_SQUELCH_MIN_DB };
  }
  const ratio = pct / 100;
  const thresholdDb = SDR_SQUELCH_MIN_DB + ratio * (SDR_SQUELCH_MAX_DB - SDR_SQUELCH_MIN_DB);
  return { enabled: true, thresholdDb };
}

function sdrSquelchServerToPercent(enabled, thresholdDb) {
  if (!enabled) return 0;
  if (!Number.isFinite(thresholdDb)) return 0;
  const ratio = (thresholdDb - SDR_SQUELCH_MIN_DB) / (SDR_SQUELCH_MAX_DB - SDR_SQUELCH_MIN_DB);
  return clampSdrSquelchPercent(ratio * 100);
}

function updateSdrSquelchPctLabel() {
  if (!sdrSquelchEl || !sdrSquelchPctEl) return;
  const pct = clampSdrSquelchPercent(Number(sdrSquelchEl.value));
  sdrSquelchPctEl.textContent = pct <= 0 ? "Open" : `${pct}%`;
}

function updateSdrSquelchControlVisibility() {
  if (!sdrSquelchWrapEl) return;
  const mode = (modeEl && modeEl.value ? modeEl.value : "").toUpperCase();
  sdrSquelchWrapEl.style.display = sdrSquelchSupported && mode !== "WFM" ? "" : "none";
}

function syncSdrSquelchFromServer(enabled, thresholdDb) {
  if (!sdrSquelchEl) return;
  if (document.activeElement === sdrSquelchEl) return;
  const pct = sdrSquelchServerToPercent(enabled, thresholdDb);
  syncFromServerSdrSquelch = true;
  sdrSquelchEl.value = String(pct);
  updateSdrSquelchPctLabel();
  syncFromServerSdrSquelch = false;
  saveSetting("sdrSquelchPct", pct);
}

function submitSdrSquelchPercent(percent) {
  if (!sdrSquelchSupported) return;
  const { enabled, thresholdDb } = sdrSquelchPercentToServer(percent);
  postPath(
    `/set_sdr_squelch?enabled=${enabled ? "true" : "false"}&threshold_db=${encodeURIComponent(thresholdDb.toFixed(2))}`,
  ).catch(() => {});
}

if (sdrSquelchEl) {
  const savedPct = clampSdrSquelchPercent(Number(loadSetting("sdrSquelchPct", 0)));
  sdrSquelchEl.value = String(savedPct);
  updateSdrSquelchPctLabel();
  sdrSquelchEl.addEventListener("input", () => {
    const pct = clampSdrSquelchPercent(Number(sdrSquelchEl.value));
    sdrSquelchEl.value = String(pct);
    updateSdrSquelchPctLabel();
    saveSetting("sdrSquelchPct", pct);
    if (!syncFromServerSdrSquelch) {
      submitSdrSquelchPercent(pct);
    }
  });
}

const sdrSquelchAutoBtn = document.getElementById("sdr-squelch-auto");
if (sdrSquelchAutoBtn) {
  sdrSquelchAutoBtn.addEventListener("click", () => {
    if (!sdrSquelchSupported) return;
    let pct = 0; // default: Off
    const data = lastSpectrumData || window.lastSpectrumData;
    if (data && isBinsArray(data.bins) && data.bins.length > 0) {
      const noiseDb = estimateNoiseFloorDb(data.bins);
      if (noiseDb != null && Number.isFinite(noiseDb)) {
        // Set threshold slightly above noise floor so squelch closes on noise
        const thresholdDb = noiseDb + 6;
        const clamped = Math.max(SDR_SQUELCH_MIN_DB, Math.min(SDR_SQUELCH_MAX_DB, thresholdDb));
        pct = clampSdrSquelchPercent(
          ((clamped - SDR_SQUELCH_MIN_DB) / (SDR_SQUELCH_MAX_DB - SDR_SQUELCH_MIN_DB)) * 100,
        );
      }
    }
    if (sdrSquelchEl) {
      sdrSquelchEl.value = String(pct);
      updateSdrSquelchPctLabel();
      saveSetting("sdrSquelchPct", pct);
    }
    submitSdrSquelchPercent(pct);
  });
}

if (wfmAudioModeEl) {
  wfmAudioModeEl.value = loadSetting("wfmAudioMode", "stereo");
  wfmAudioModeEl.addEventListener("change", () => {
    saveSetting("wfmAudioMode", wfmAudioModeEl.value);
    const enabled = wfmAudioModeEl.value !== "mono";
    postPath(`/set_wfm_stereo?enabled=${enabled ? "true" : "false"}`).catch(() => {});
  });
}
if (wfmDenoiseEl) {
  wfmDenoiseEl.value = normalizeWfmDenoiseLevel(loadSetting("wfmDenoise", "auto"));
  wfmDenoiseEl.addEventListener("change", () => {
    const level = normalizeWfmDenoiseLevel(wfmDenoiseEl.value);
    wfmDenoiseEl.value = level;
    saveSetting("wfmDenoise", level);
    postPath(`/set_wfm_denoise?level=${encodeURIComponent(level)}`).catch(() => {});
  });
}
if (wfmDeemphasisEl) {
  wfmDeemphasisEl.addEventListener("change", () => {
    postPath(`/set_wfm_deemphasis?us=${encodeURIComponent(wfmDeemphasisEl.value)}`).catch(() => {});
  });
}
if (samStereoWidthEl) {
  samStereoWidthEl.addEventListener("input", () => {
    const width = Number(samStereoWidthEl.value) / 100;
    postPath(`/set_sam_stereo_width?width=${width}`).catch(() => {});
  });
}
if (samCarrierSyncEl) {
  samCarrierSyncEl.addEventListener("change", () => {
    const enabled = samCarrierSyncEl.value === "on";
    postPath(`/set_sam_carrier_sync?enabled=${enabled}`).catch(() => {});
  });
}
function submitSdrGain() {
  if (!sdrGainEl) return;
  const parsed = Number.parseFloat(sdrGainEl.value);
  if (!Number.isFinite(parsed) || parsed < 0) return;
  postPath(`/set_sdr_gain?db=${encodeURIComponent(parsed)}`).catch(() => {});
}
function updateSdrGainInputState() {
  if (!sdrAgcEl) return;
  const agcOn = sdrAgcEl.checked;
  if (sdrGainEl) sdrGainEl.disabled = agcOn;
  if (sdrGainSetBtn) sdrGainSetBtn.disabled = agcOn;
  if (sdrLnaGainEl) sdrLnaGainEl.disabled = agcOn;
  if (sdrLnaGainSetBtn) sdrLnaGainSetBtn.disabled = agcOn;
}
if (sdrAgcEl) {
  sdrAgcEl.addEventListener("change", () => {
    postPath(`/set_sdr_agc?enabled=${sdrAgcEl.checked ? "true" : "false"}`).catch(() => {});
    updateSdrGainInputState();
  });
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
function submitSdrLnaGain() {
  if (!sdrLnaGainEl) return;
  const parsed = Number.parseFloat(sdrLnaGainEl.value);
  if (!Number.isFinite(parsed) || parsed < 0) return;
  postPath(`/set_sdr_lna_gain?db=${encodeURIComponent(parsed)}`).catch(() => {});
}
if (sdrLnaGainSetBtn) {
  sdrLnaGainSetBtn.addEventListener("click", submitSdrLnaGain);
}
if (sdrLnaGainEl) {
  sdrLnaGainEl.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      submitSdrLnaGain();
    }
  });
}
function submitSdrNbState() {
  if (!sdrNbSupported) return;
  const enabled = sdrNbEnabledEl ? sdrNbEnabledEl.checked : false;
  const threshold = sdrNbThresholdEl ? Number.parseFloat(sdrNbThresholdEl.value) : 10;
  if (!Number.isFinite(threshold) || threshold < 1 || threshold > 100) return;
  postPath(
    `/set_sdr_noise_blanker?enabled=${enabled ? "true" : "false"}&threshold=${encodeURIComponent(threshold)}`,
  ).catch(() => {});
}
if (sdrNbEnabledEl) {
  sdrNbEnabledEl.addEventListener("change", () => {
    submitSdrNbState();
  });
}
function submitSdrNbThreshold() {
  if (!sdrNbThresholdEl) return;
  const parsed = Number.parseFloat(sdrNbThresholdEl.value);
  if (!Number.isFinite(parsed) || parsed < 1 || parsed > 100) return;
  submitSdrNbState();
}
if (sdrNbThresholdSetBtn) {
  sdrNbThresholdSetBtn.addEventListener("click", submitSdrNbThreshold);
}
if (sdrNbThresholdEl) {
  sdrNbThresholdEl.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      submitSdrNbThreshold();
    }
  });
}
function updateWfmControls() {
  const mode = (modeEl && modeEl.value ? modeEl.value : "").toUpperCase();
  if (wfmControlsCol) wfmControlsCol.style.display = mode === "WFM" ? "" : "none";
  if (samControlsCol) samControlsCol.style.display = mode === "SAM" ? "" : "none";
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
  if (wasmOpusDecoder) {
    try { wasmOpusDecoder.free(); } catch (e) {}
    wasmOpusDecoder = null;
  }
  nextPlayTime = 0;
}

function configureRxStream(nextInfo) {
  const nextSampleRate = (nextInfo && nextInfo.sample_rate) || 48000;
  streamInfo = nextInfo;
  updateWfmControls();
  resetRxDecoder();
  ensureRxAudioContext(nextSampleRate);
  rxGainNode.gain.value = rxVolSlider.value / 100;
  rxActive = true;
  setAudioLevel(0);
  rxAudioBtn.style.borderColor = "#00d17f";
  rxAudioBtn.style.color = "#00d17f";
  audioStatus.textContent = "RX";
  syncHeaderAudioBtn();
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

// Optional channel_id injected by vchan.js when connecting to a virtual channel.
let _audioChannelOverride = null;

/** Schedule decoded PCM channels for playback via Web Audio API. */
function scheduleDecodedAudio(channelData, frameCount, sampleRate) {
  if (!audioCtx || !rxGainNode) return;
  const levelNow = Date.now();
  if (levelNow - lastLevelUpdate >= 50) {
    setAudioLevel(levelFromChannels(channelData, frameCount));
    lastLevelUpdate = levelNow;
  }
  const forceMono = channelData.length >= 2
    && wfmAudioModeEl
    && wfmAudioModeEl.value === "mono"
    && modeEl
    && (modeEl.value || "").toUpperCase() === "WFM";
  const outChannels = forceMono ? 1 : channelData.length;
  const ab = audioCtx.createBuffer(outChannels, frameCount, sampleRate);
  if (forceMono) {
    const monoData = new Float32Array(frameCount);
    for (let ch = 0; ch < channelData.length; ch++) {
      const plane = channelData[ch];
      for (let i = 0; i < frameCount; i++) monoData[i] += plane[i];
    }
    const inv = 1 / Math.max(1, channelData.length);
    for (let i = 0; i < frameCount; i++) monoData[i] *= inv;
    ab.copyToChannel(monoData, 0);
  } else {
    for (let ch = 0; ch < channelData.length; ch++) {
      ab.copyToChannel(channelData[ch], ch);
    }
  }
  const src = audioCtx.createBufferSource();
  src.buffer = ab;
  src.connect(rxGainNode);
  const now = audioCtx.currentTime;
  const sr = (streamInfo && streamInfo.sample_rate) || sampleRate || 48000;
  const minLeadSecs = Math.max(0, MIN_RX_JITTER_SAMPLES / Math.max(1, sr));
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
}

function startRxAudio() {
  if (rxActive) { stopRxAudio(); return; }
  if (!hasWebCodecs && !hasWasmOpus) {
    audioStatus.textContent = "Audio not supported in this browser";
    return;
  }
  ensureRxAudioContext((streamInfo && streamInfo.sample_rate) || 48000);
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  let audioPath;
  if (_audioChannelOverride) {
    const remoteParam = lastActiveRigId
      ? `&remote=${encodeURIComponent(lastActiveRigId)}`
      : "";
    audioPath = `/audio?channel_id=${encodeURIComponent(_audioChannelOverride)}${remoteParam}`;
  } else if (lastActiveRigId) {
    audioPath = `/audio?remote=${encodeURIComponent(lastActiveRigId)}`;
  } else {
    audioPath = "/audio";
  }
  audioWs = new WebSocket(`${proto}//${location.host}${audioPath}`);
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

    // Binary Opus data
    if (!audioCtx) return;
    const data = new Uint8Array(evt.data);

    // Lazily initialise a decoder: prefer WebCodecs, fall back to WASM.
    if (!opusDecoder && !wasmOpusDecoder) {
      const channels = (streamInfo && streamInfo.channels) || 1;
      const sampleRate = (streamInfo && streamInfo.sample_rate) || 48000;
      // Try WebCodecs AudioDecoder first (Chrome/Edge).
      if (hasWebCodecs) {
        try {
          opusDecoder = new AudioDecoder({
            output: (frame) => {
              const ch = extractAudioFrameChannels(frame);
              scheduleDecodedAudio(ch, frame.numberOfFrames, frame.sampleRate);
              frame.close();
            },
            error: (e) => { console.error("AudioDecoder error", e); }
          });
          opusDecoder.configure({ codec: "opus", sampleRate, numberOfChannels: channels });
        } catch (e) {
          console.warn("WebCodecs Opus not supported, trying WASM fallback", e);
          opusDecoder = null;
        }
      }
      // WASM fallback (Safari/Firefox).
      if (!opusDecoder && hasWasmOpus) {
        try {
          const coupledStreamCount = channels >= 2 ? 1 : 0;
          const mapping = channels >= 2 ? [0, 1] : [0];
          wasmOpusDecoder = new window["opus-decoder"].OpusDecoder({
            sampleRate,
            channels,
            streamCount: 1,
            coupledStreamCount,
            channelMappingTable: mapping,
            preSkip: 0,
          });
          // .ready is a Promise that resolves when WASM is compiled.
          wasmOpusDecoder.ready.then(() => {
            audioStatus.textContent = "RX";
          }).catch((e) => {
            console.error("WASM Opus init failed", e);
            wasmOpusDecoder = null;
          });
        } catch (e) {
          console.warn("WASM Opus decoder init failed", e);
          wasmOpusDecoder = null;
        }
      }
    }

    // Decode with whichever decoder is available.
    if (opusDecoder) {
      try {
        opusDecoder.decode(new EncodedAudioChunk({
          type: "key",
          timestamp: performance.now() * 1000,
          data: data,
        }));
      } catch (e) { /* ignore per-frame errors */ }
    } else if (wasmOpusDecoder) {
      try {
        const result = wasmOpusDecoder.decodeFrame(data);
        if (result && result.samplesDecoded > 0) {
          scheduleDecodedAudio(result.channelData, result.samplesDecoded, result.sampleRate);
        }
      } catch (e) { /* ignore per-frame errors */ }
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
    setAudioLevel(0);
    rxGainNode = null;
    if (opusDecoder) {
      try { opusDecoder.close(); } catch(e) {}
      opusDecoder = null;
    }
    if (wasmOpusDecoder) {
      try { wasmOpusDecoder.free(); } catch(e) {}
      wasmOpusDecoder = null;
    }
    nextPlayTime = 0;
    syncHeaderAudioBtn();
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
  if (wasmOpusDecoder) {
    try { wasmOpusDecoder.free(); } catch(e) {}
    wasmOpusDecoder = null;
  }
  nextPlayTime = 0;
  rxAudioBtn.style.borderColor = "";
  rxAudioBtn.style.color = "";
  audioStatus.textContent = "Off";
  setAudioLevel(0);
  syncHeaderAudioBtn();
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

// Header play button mirrors the RX audio toggle.
const headerAudioToggle = document.getElementById("header-audio-toggle");
const _audioIconPlay = '<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M5 3v10l8-5z"/></svg>';
const _audioIconPause = '<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><rect x="3" y="3" width="3.5" height="10" rx="0.8"/><rect x="9.5" y="3" width="3.5" height="10" rx="0.8"/></svg>';
function syncHeaderAudioBtn() {
  if (!headerAudioToggle) return;
  headerAudioToggle.classList.toggle("audio-active", rxActive);
  headerAudioToggle.title = rxActive ? "Stop audio" : "Play audio";
  headerAudioToggle.innerHTML = rxActive ? _audioIconPause : _audioIconPlay;
}
if (headerAudioToggle) {
  headerAudioToggle.addEventListener("click", startRxAudio);
}

// ── Recorder ───────────────────────────────────────────────────────────────
let recorderActive = false;
const recorderStartBtn = document.getElementById("recorder-start-btn");
const recorderStopBtn = document.getElementById("recorder-stop-btn");
const recorderStatusInd = document.getElementById("recorder-status-indicator");
const headerRecBtn = document.getElementById("header-rec-btn");

function syncRecorderUi() {
  if (recorderStartBtn) recorderStartBtn.disabled = recorderActive;
  if (recorderStopBtn) recorderStopBtn.disabled = !recorderActive;
  if (recorderStatusInd) {
    recorderStatusInd.textContent = recorderActive ? "Recording" : "";
    recorderStatusInd.classList.toggle("rec-active", recorderActive);
  }
  if (headerRecBtn) headerRecBtn.classList.toggle("rec-active", recorderActive);
  const tabBtn = document.querySelector('.tab[data-tab="recorder"]');
  if (tabBtn) tabBtn.classList.toggle("rec-active", recorderActive);
}

if (recorderStartBtn) {
  recorderStartBtn.addEventListener("click", async () => {
    try { await postPath("/api/recorder/start"); } catch (e) { console.error("Recorder start failed", e); }
  });
}
if (recorderStopBtn) {
  recorderStopBtn.addEventListener("click", async () => {
    try { await postPath("/api/recorder/stop"); } catch (e) { console.error("Recorder stop failed", e); }
  });
}
if (headerRecBtn) {
  headerRecBtn.addEventListener("click", async () => {
    try {
      if (recorderActive) { await postPath("/api/recorder/stop"); }
      else { await postPath("/api/recorder/start"); }
    } catch (e) { console.error("Recorder toggle failed", e); }
  });
}

window._syncRecorderState = function (enabled) {
  recorderActive = enabled;
  syncRecorderUi();
};

let _recorderFiles = [];
let _recFilesPage = 0;
const REC_PAGE_SIZE = 15;

async function refreshRecorderStatus() {
  try {
    const [statusResp, filesResp] = await Promise.all([
      fetch("/api/recorder/status"),
      fetch("/api/recorder/files"),
    ]);
    if (statusResp.ok) {
      const active = await statusResp.json();
      renderRecorderActive(active);
    }
    if (filesResp.ok) {
      _recorderFiles = await filesResp.json();
      renderRecorderFiles();
    }
  } catch (e) {
    console.error("Recorder status fetch failed", e);
  }
}

function renderRecorderActive(list) {
  const el = document.getElementById("recorder-active-list");
  if (!el) return;
  if (!list.length) {
    el.innerHTML = '<p class="recorder-empty">No active recordings.</p>';
    return;
  }
  let html = '<table class="recorder-table"><thead><tr><th>Rig</th><th>VChan</th><th>File</th><th>Started</th></tr></thead><tbody>';
  for (const r of list) {
    const started = new Date(r.started_at * 1000).toLocaleTimeString();
    const fname = r.path.split("/").pop();
    html += `<tr><td>${escapeMapHtml(r.rig_id)}</td><td>${r.vchan_id ? escapeMapHtml(r.vchan_id) : "-"}</td><td>${escapeMapHtml(fname)}</td><td>${started}</td></tr>`;
  }
  html += "</tbody></table>";
  el.innerHTML = html;
}

function recorderFormatSize(bytes) {
  if (bytes < 1024) return bytes + " B";
  if (bytes < 1048576) return (bytes / 1024).toFixed(1) + " KB";
  return (bytes / 1048576).toFixed(1) + " MB";
}

function recFilterAndSort() {
  const filterEl = document.getElementById("recorder-filter");
  const sortEl = document.getElementById("recorder-sort");
  const filter = (filterEl ? filterEl.value : "").toLowerCase();
  const sortMode = sortEl ? sortEl.value : "name-desc";

  let filtered = _recorderFiles;
  if (filter) {
    filtered = filtered.filter(function (f) {
      return f.name.toLowerCase().includes(filter);
    });
  }

  const sorted = filtered.slice();
  switch (sortMode) {
    case "name-asc":  sorted.sort(function (a, b) { return a.name.localeCompare(b.name); }); break;
    case "name-desc": sorted.sort(function (a, b) { return b.name.localeCompare(a.name); }); break;
    case "size-asc":  sorted.sort(function (a, b) { return a.size - b.size; }); break;
    case "size-desc": sorted.sort(function (a, b) { return b.size - a.size; }); break;
  }
  return sorted;
}

function renderRecorderFiles() {
  const el = document.getElementById("recorder-files-list");
  if (!el) return;

  const sorted = recFilterAndSort();
  const total = sorted.length;
  const totalPages = Math.max(1, Math.ceil(total / REC_PAGE_SIZE));
  if (_recFilesPage >= totalPages) _recFilesPage = totalPages - 1;
  if (_recFilesPage < 0) _recFilesPage = 0;
  const start = _recFilesPage * REC_PAGE_SIZE;
  const page = sorted.slice(start, start + REC_PAGE_SIZE);

  const summaryEl = document.getElementById("rec-page-summary");
  const indicatorEl = document.getElementById("rec-page-indicator");
  const prevBtn = document.getElementById("rec-page-prev");
  const nextBtn = document.getElementById("rec-page-next");

  if (summaryEl) {
    summaryEl.textContent = total ? "Showing " + (start + 1) + "-" + Math.min(start + REC_PAGE_SIZE, total) + " of " + total : "Showing 0-0 of 0";
  }
  if (indicatorEl) indicatorEl.textContent = "Page " + (_recFilesPage + 1) + " of " + totalPages;
  if (prevBtn) prevBtn.disabled = _recFilesPage <= 0;
  if (nextBtn) nextBtn.disabled = _recFilesPage >= totalPages - 1;

  const filterEl = document.getElementById("recorder-filter");
  const filter = filterEl ? filterEl.value : "";

  if (!page.length) {
    el.innerHTML = '<p class="recorder-empty">' + (filter ? "No files match filter." : "No recorded files.") + "</p>";
    return;
  }

  let html = '<table class="recorder-table"><thead><tr><th>File</th><th>Size</th><th></th></tr></thead><tbody>';
  for (const f of page) {
    const safeName = escapeMapHtml(f.name);
    const encodedName = encodeURIComponent(f.name);
    html += "<tr>"
      + "<td>" + safeName + "</td>"
      + "<td>" + recorderFormatSize(f.size) + "</td>"
      + '<td><div class="rec-file-actions">'
      + '<a class="rec-file-btn" href="/api/recorder/download/' + encodedName + '" download="' + safeName + '">Download</a>'
      + '<button class="rec-file-btn rec-delete-btn" data-name="' + safeName + '" type="button">Remove</button>'
      + "</div></td></tr>";
  }
  html += "</tbody></table>";
  el.innerHTML = html;

  el.querySelectorAll(".rec-delete-btn").forEach(function (btn) {
    btn.addEventListener("click", async function () {
      const name = btn.dataset.name;
      if (!confirm("Delete recording " + name + "?")) return;
      try {
        const resp = await fetch("/api/recorder/files/" + encodeURIComponent(name), { method: "DELETE" });
        if (!resp.ok) throw new Error("HTTP " + resp.status);
        _recorderFiles = _recorderFiles.filter(function (f) { return f.name !== name; });
        renderRecorderFiles();
      } catch (e) {
        console.error("Delete failed", e);
      }
    });
  });
}

(function () {
  const filterEl = document.getElementById("recorder-filter");
  const sortEl = document.getElementById("recorder-sort");
  if (filterEl) filterEl.addEventListener("input", function () { _recFilesPage = 0; renderRecorderFiles(); });
  if (sortEl) sortEl.addEventListener("change", function () { _recFilesPage = 0; renderRecorderFiles(); });
  const prevBtn = document.getElementById("rec-page-prev");
  const nextBtn = document.getElementById("rec-page-next");
  if (prevBtn) prevBtn.addEventListener("click", function () { _recFilesPage--; renderRecorderFiles(); });
  if (nextBtn) nextBtn.addEventListener("click", function () { _recFilesPage++; renderRecorderFiles(); });
})();

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
if (sdrSquelchEl) {
  sdrSquelchEl.addEventListener("wheel", (e) => {
    e.preventDefault();
    const step = e.deltaY < 0 ? 2 : -2;
    const next = clampSdrSquelchPercent(Number(sdrSquelchEl.value) + step);
    sdrSquelchEl.value = String(next);
    updateSdrSquelchPctLabel();
    saveSetting("sdrSquelchPct", next);
    submitSdrSquelchPercent(next);
  }, { passive: false });
}

document.getElementById("copyright-year").textContent = new Date().getFullYear();

// --- Server-side decode SSE ---
let decodeSource = null;
let decodeConnected = false;
let decodeHistoryWorker = null;
function setModeBoundDecodeStatus(el, activeModes, inactiveText, connectedText) {
  if (!el) return;
  const modeUpper = (document.getElementById("mode")?.value || "").toUpperCase();
  const isActiveMode = activeModes.includes(modeUpper);
  if (el.textContent === "Receiving" && isActiveMode) return;
  el.textContent = isActiveMode ? connectedText : inactiveText;
}
// Custom connected-state text overrides per decoder.
const _decodeConnectedText = {
  vdes: "Connected, listening for bursts",
  cw: "Connected, listening for CW",
};
function updateDecodeStatus(text) {
  // Mode-bound decoders: show mode-gated status text.
  for (const d of decoderRegistry) {
    if (d.activation !== "mode_bound") continue;
    const el = document.getElementById(d.id + "-status");
    if (!el) continue;
    const connText = _decodeConnectedText[d.id] || text;
    setModeBoundDecodeStatus(el, d.active_modes, "Select " + d.active_modes[0] + " mode to decode", connText);
  }
  // Toggle-gated decoders: update status text if not currently receiving.
  for (const d of decoderRegistry) {
    if (d.activation !== "toggle") continue;
    const el = document.getElementById(d.id + "-status");
    if (el && el.textContent !== "Receiving") el.textContent = text;
  }
}
function dispatchDecodeMessage(msg, skipStats) {
  if (msg.type === "ais" && window.onServerAis) window.onServerAis(msg);
  if (msg.type === "vdes" && window.onServerVdes) window.onServerVdes(msg);
  if (msg.type === "aprs" && window.onServerAprs) window.onServerAprs(msg);
  if (msg.type === "hf_aprs" && window.onServerHfAprs) window.onServerHfAprs(msg);
  if (msg.type === "cw" && window.onServerCw) window.onServerCw(msg);
  if (msg.type === "ft8" && window.onServerFt8) window.onServerFt8(msg);
  if (msg.type === "ft4" && window.onServerFt4) window.onServerFt4(msg);
  if (msg.type === "ft2" && window.onServerFt2) window.onServerFt2(msg);
  if (msg.type === "wspr" && window.onServerWspr) window.onServerWspr(msg);
  if (msg.type === "lrpt_image" && window.onServerLrptImage) window.onServerLrptImage(msg);
  if (msg.type === "lrpt_progress" && window.onServerLrptProgress) window.onServerLrptProgress(msg);
  if (!skipStats && msg.type && msg.type !== "lrpt_image" && msg.type !== "lrpt_progress") {
    window.trx.map?.statsRecordDecode(msg.type, msg.rig_id || msg.remote || null);
    window.trx.map?.scheduleStatsRender();
  }
}

function dispatchDecodeBatch(batch) {
  if (!Array.isArray(batch) || batch.length === 0) return;
  // Record statistics for every message in the batch regardless of dispatch path.
  for (const msg of batch) {
    if (msg.type && msg.type !== "lrpt_image" && msg.type !== "lrpt_progress") {
      window.trx.map?.statsRecordDecode(msg.type, msg.rig_id || msg.remote || null);
    }
  }
  window.trx.map?.scheduleStatsRender();
  const type = String(batch[0]?.type || "");
  const uniformType = batch.every((msg) => String(msg?.type || "") === type);
  if (uniformType) {
    if (type === "ais" && window.onServerAisBatch) {
      window.onServerAisBatch(batch);
      return;
    }
    if (type === "vdes" && window.onServerVdesBatch) {
      window.onServerVdesBatch(batch);
      return;
    }
    if (type === "aprs" && window.onServerAprsBatch) {
      window.onServerAprsBatch(batch);
      return;
    }
    if (type === "hf_aprs" && window.onServerHfAprsBatch) {
      window.onServerHfAprsBatch(batch);
      return;
    }
    if (type === "ft8" && window.onServerFt8Batch) {
      window.onServerFt8Batch(batch);
      return;
    }
    if (type === "ft4" && window.onServerFt4Batch) {
      window.onServerFt4Batch(batch);
      return;
    }
    if (type === "ft2" && window.onServerFt2Batch) {
      window.onServerFt2Batch(batch);
      return;
    }
    if (type === "wspr" && window.onServerWsprBatch) {
      window.onServerWsprBatch(batch);
      return;
    }
  }
  for (const msg of batch) {
    dispatchDecodeMessage(msg, true);
  }
}

const DECODE_HISTORY_TYPE_BATCH_LIMIT = 192;
const DECODE_HISTORY_WORKER_GROUP_LIMIT = 512;
const DECODE_HISTORY_BATCH_DRAIN_BUDGET_MS = 8;

function terminateDecodeHistoryWorker() {
  if (!decodeHistoryWorker) return;
  try { decodeHistoryWorker.terminate(); } catch (_) {}
  decodeHistoryWorker = null;
}

function scheduleDecodeHistoryDrainStep(callback) {
  if (typeof callback !== "function") return;
  if (typeof requestAnimationFrame === "function") {
    requestAnimationFrame(() => callback());
  } else {
    setTimeout(callback, 16);
  }
}

function decodeHistoryUrl() {
  return "/decode/history";
}

function loadDecodeHistoryOnMainThread(onReady, onError) {
  fetch(decodeHistoryUrl()).then(async (resp) => {
    if (!resp.ok) return null;
    setDecodeHistoryOverlayVisible(true, "Loading decode history…", "Receiving compressed history payload");
    const payload = await resp.arrayBuffer();
    if (!payload || payload.byteLength === 0) return {};
    setDecodeHistoryOverlayVisible(true, "Loading decode history…", "Decoding compressed history payload");
    return decodeCborPayload(payload);
  }).then((groups) => {
    if (typeof onReady === "function") onReady(groups && typeof groups === "object" ? groups : {});
  }).catch((err) => {
    if (typeof onError === "function") onError(err);
  });
}

function restoreDecodeHistoryGroup(kind, messages) {
  if (!Array.isArray(messages) || messages.length === 0) return;
  // Record statistics for restored history messages.
  if (kind !== "lrpt_image" && kind !== "lrpt_progress") {
    for (const msg of messages) {
      window.trx.map?.statsRecordDecode(kind, msg.rig_id || msg.remote || null, msg.ts_ms || undefined);
    }
    window.trx.map?.scheduleStatsRender();
  }
  if (kind === "ais" && window.restoreAisHistory) {
    window.restoreAisHistory(messages);
    return;
  }
  if (kind === "vdes" && window.restoreVdesHistory) {
    window.restoreVdesHistory(messages);
    return;
  }
  if (kind === "aprs" && window.restoreAprsHistory) {
    window.restoreAprsHistory(messages);
    return;
  }
  if (kind === "hf_aprs" && window.restoreHfAprsHistory) {
    window.restoreHfAprsHistory(messages);
    return;
  }
  if (kind === "cw" && window.restoreCwHistory) {
    window.restoreCwHistory(messages);
    return;
  }
  if (kind === "ft8" && window.restoreFt8History) {
    window.restoreFt8History(messages);
    return;
  }
  if (kind === "ft4" && window.restoreFt4History) {
    window.restoreFt4History(messages);
    return;
  }
  if (kind === "ft2" && window.restoreFt2History) {
    window.restoreFt2History(messages);
    return;
  }
  if (kind === "wspr" && window.restoreWsprHistory) {
    window.restoreWsprHistory(messages);
    return;
  }
}

function connectDecode() {
  if (decodeSource) { decodeSource.close(); }
  terminateDecodeHistoryWorker();
  decodeHistoryReplayActive = false;
  decodeMapSyncPending = false;
  if (window.resetAisHistoryView) window.resetAisHistoryView();
  if (window.resetVdesHistoryView) window.resetVdesHistoryView();
  if (window.resetAprsHistoryView) window.resetAprsHistoryView();
  if (window.resetCwHistoryView) window.resetCwHistoryView();
  if (window.resetFt8HistoryView) window.resetFt8HistoryView();
  if (window.resetFt4HistoryView) window.resetFt4HistoryView();
  if (window.resetWsprHistoryView) window.resetWsprHistoryView();

  // Buffer live messages until history fetch settles so history always appears
  // before any live updates, regardless of network ordering.
  let historySettled = false;
  let historyWorkerDone = false;
  let historyFallbackStarted = false;
  let historyBatchDrainScheduled = false;
  let historyTotal = 0;
  let historyProcessed = 0;
  const historyGroupQueue = [];
  const liveBuffer = [];
  function flushLiveBuffer() {
    historySettled = true;
    terminateDecodeHistoryWorker();
    setDecodeHistoryReplayActive(false);
    setDecodeHistoryOverlayVisible(false);
    for (const msg of liveBuffer) {
      try { dispatchDecodeMessage(msg); } catch (_) {}
    }
    liveBuffer.length = 0;
  }

  function updateHistoryReplayOverlay() {
    setDecodeHistoryOverlayVisible(
      true,
      "Loading decode history…",
      `Replaying ${historyProcessed} / ${historyTotal} decoded messages`
    );
  }

  function maybeFinishHistoryReplay() {
    if (historySettled) return;
    if (historyWorkerDone && historyGroupQueue.length === 0) {
      clearTimeout(historyTimeout);
      flushLiveBuffer();
    }
  }

  function pumpDecodeHistoryGroupQueue() {
    historyBatchDrainScheduled = false;
    const startedAt = typeof performance !== "undefined" && typeof performance.now === "function"
      ? performance.now()
      : 0;
    while (historyGroupQueue.length > 0) {
      const next = historyGroupQueue.shift();
      restoreDecodeHistoryGroup(next.kind, next.messages);
      historyProcessed += Array.isArray(next.messages) ? next.messages.length : 0;
      updateHistoryReplayOverlay();
      if (startedAt > 0 && (performance.now() - startedAt) >= DECODE_HISTORY_BATCH_DRAIN_BUDGET_MS) {
        break;
      }
    }
    if (historyGroupQueue.length > 0) {
      scheduleDecodeHistoryDrainStep(pumpDecodeHistoryGroupQueue);
      historyBatchDrainScheduled = true;
      return;
    }
    maybeFinishHistoryReplay();
  }

  function enqueueDecodeHistoryGroup(kind, messages) {
    if (!Array.isArray(messages) || messages.length === 0) return;
    historyGroupQueue.push({ kind, messages });
    if (historyBatchDrainScheduled) return;
    historyBatchDrainScheduled = true;
    scheduleDecodeHistoryDrainStep(pumpDecodeHistoryGroupQueue);
  }

  function totalDecodeHistoryMessages(groups) {
    if (!groups || typeof groups !== "object") return 0;
    return ["ais", "vdes", "aprs", "hf_aprs", "cw", "ft8", "ft4", "ft2", "wspr"]
      .reduce((sum, key) => sum + (Array.isArray(groups[key]) ? groups[key].length : 0), 0);
  }

  function enqueueDecodeHistoryGroups(groups) {
    historyTotal = totalDecodeHistoryMessages(groups);
    historyProcessed = 0;
    if (historyTotal > 0) {
      setDecodeHistoryReplayActive(true);
      updateHistoryReplayOverlay();
    }
    for (const kind of ["ais", "vdes", "aprs", "hf_aprs", "cw", "ft8", "ft4", "ft2", "wspr"]) {
      const messages = groups && Array.isArray(groups[kind]) ? groups[kind] : [];
      if (messages.length === 0) continue;
      for (let index = 0; index < messages.length; index += DECODE_HISTORY_WORKER_GROUP_LIMIT) {
        enqueueDecodeHistoryGroup(kind, messages.slice(index, index + DECODE_HISTORY_WORKER_GROUP_LIMIT));
      }
    }
    historyWorkerDone = true;
    maybeFinishHistoryReplay();
  }

  function startDecodeHistoryFallback() {
    if (historyFallbackStarted || historySettled) return;
    historyFallbackStarted = true;
    loadDecodeHistoryOnMainThread((groups) => {
      clearTimeout(historyTimeout);
      const total = totalDecodeHistoryMessages(groups);
      if (total > 0) {
        enqueueDecodeHistoryGroups(groups);
      } else {
        flushLiveBuffer();
      }
    }, (err) => {
      console.error("Decode history fallback failed", err);
      clearTimeout(historyTimeout);
      flushLiveBuffer();
    });
  }

  function startDecodeHistoryWorkerReplay() {
    if (typeof Worker !== "function") return false;
    let worker;
    try {
      worker = new Worker("/decode-history-worker.js");
    } catch (err) {
      console.error("Decode history worker startup failed", err);
      return false;
    }
    decodeHistoryWorker = worker;
    worker.onmessage = (evt) => {
      if (historySettled || worker !== decodeHistoryWorker) return;
      const data = evt?.data || {};
      if (data.type === "status") {
        const phase = String(data.phase || "");
        if (phase === "fetching") {
          setDecodeHistoryOverlayVisible(true, "Loading decode history…", "Fetching recent decodes from the client buffer");
        } else if (phase === "decoding") {
          setDecodeHistoryOverlayVisible(true, "Loading decode history…", "Decoding compressed history in background");
        }
        return;
      }
      if (data.type === "start") {
        historyTotal = Math.max(0, Number(data.total) || 0);
        historyProcessed = 0;
        if (historyTotal > 0) {
          setDecodeHistoryReplayActive(true);
          updateHistoryReplayOverlay();
        }
        return;
      }
      if (data.type === "group") {
        enqueueDecodeHistoryGroup(String(data.kind || ""), data.messages);
        return;
      }
      if (data.type === "done") {
        historyWorkerDone = true;
        clearTimeout(historyTimeout);
        terminateDecodeHistoryWorker();
        maybeFinishHistoryReplay();
        return;
      }
      if (data.type === "error") {
        console.error("Decode history worker failed", data.message || "unknown worker failure");
        terminateDecodeHistoryWorker();
        startDecodeHistoryFallback();
      }
    };
    worker.postMessage({
      type: "fetch-history",
      url: decodeHistoryUrl(),
      batchLimit: DECODE_HISTORY_WORKER_GROUP_LIMIT,
    });
    return true;
  }

  // Safety valve: if the history fetch hangs, unblock after 20 s.
  const historyTimeout = setTimeout(() => {
    if (!historySettled) {
      terminateDecodeHistoryWorker();
      flushLiveBuffer();
    }
  }, 20000);
  setDecodeHistoryOverlayVisible(true, "Loading decode history…", "Fetching recent decodes from the client buffer");

  decodeSource = new EventSource("/decode");
  decodeSource.onopen = () => {
    decodeConnected = true;
    updateDecodeStatus("Connected, listening for packets");
  };
  decodeSource.onmessage = (evt) => {
    try {
      const msg = JSON.parse(evt.data);
      if (historySettled) dispatchDecodeMessage(msg);
      else liveBuffer.push(msg);
    } catch (e) { /* ignore parse errors */ }
  };
  decodeSource.onerror = () => {
    // readyState CLOSED (2) = server rejected (404/error), CONNECTING (0) = temporary drop
    const wasClosed = decodeSource.readyState === 2;
    decodeSource.close();
    decodeConnected = false;
    terminateDecodeHistoryWorker();
    if (!historySettled) flushLiveBuffer();
    if (wasClosed) {
      updateDecodeStatus("Decode not available (check client audio config)");
      setTimeout(connectDecode, 10000);
    } else {
      updateDecodeStatus("Decode disconnected, retrying…");
      setTimeout(connectDecode, 5000);
    }
  };

  if (!startDecodeHistoryWorkerReplay()) {
    startDecodeHistoryFallback();
  }
}
// connectDecode() is called from initializeApp() after auth succeeds,
// and from login/guest handlers — no standalone window.load call needed.

// Release PTT on page unload to prevent stuck transmit
window.addEventListener("beforeunload", () => {
  if (txActive) {
    navigator.sendBeacon("/set_ptt?ptt=false", "");
  }
});


// ── Spectrum display ─────────────────────────────────────────────────────────
const spectrumCanvas  = document.getElementById("spectrum-canvas");
const spectrumGl = typeof createTrxWebGlRenderer === "function"
  ? createTrxWebGlRenderer(spectrumCanvas, spectrumSnapshotGlOptions)
  : null;
const spectrumDbAxis = document.getElementById("spectrum-db-axis");
const spectrumFreqAxis = document.getElementById("spectrum-freq-axis");
const spectrumTooltip = document.getElementById("spectrum-tooltip");
const spectrumCenterLeftBtn = document.getElementById("spectrum-center-left-btn");
const spectrumCenterRightBtn = document.getElementById("spectrum-center-right-btn");
let spectrumSource = null;
let spectrumReconnectTimer = null;
let spectrumDrawPending = false;
let spectrumAxisKey = "";
let spectrumDbAxisKey = "";
let lastSpectrumRenderData = null;
let spectrumPeakHoldFrames = [];
let pendingSpectrumFrameWaiters = [];
let sweetSpotScanInFlight = false;
const spectrumTmpGridSegments = [];
const spectrumTmpFillPoints = [];
const spectrumTmpPeakPoints = [];
const spectrumTmpMarkerPoints = [];

// Zoom / pan state.  zoom >= 1; panFrac in [0,1] is the fraction of the full
// bandwidth at the centre of the visible window.
let spectrumZoom    = 1;
let spectrumPanFrac = 0.5;

// Y-axis level: floor = bottom dB value shown; range = total dB span.
let spectrumFloor = -115;
let spectrumRange = 90;
let waterfallGamma = 1.0;
const SPECTRUM_HEADROOM_DB = 20;
const SPECTRUM_SMOOTH_ALPHA = 0.42;
let _spectrumBinBuf = []; // Reusable buffer for SSE bin decoding
// Fast base64 → Int8Array decoder using a lookup table.
// Avoids atob() (which allocates a UTF-16 string) and the subsequent
// charCodeAt loop, decoding directly into a reusable typed array.
const _b64Lut = new Uint8Array(128);
for (let i = 0; i < 128; i++) _b64Lut[i] = 255;
"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".split("").forEach((c, i) => {
  _b64Lut[c.charCodeAt(0)] = i;
});
let _spectrumBinI8 = new Int8Array(0); // Reusable typed-array bin buffer
// Check if a value is an array-like bins buffer (Array or TypedArray).
function isBinsArray(v) { return Array.isArray(v) || ArrayBuffer.isView(v); }
function decodeBase64ToInt8(b64) {
  // Strip trailing '=' padding
  let end = b64.length;
  while (end > 0 && b64.charCodeAt(end - 1) === 61) end--;
  const outLen = (end * 3 >>> 2); // exact byte count without padding
  if (_spectrumBinI8.length !== outLen) _spectrumBinI8 = new Int8Array(outLen);
  const out = _spectrumBinI8;
  let j = 0;
  for (let i = 0; i < end; ) {
    const a = _b64Lut[b64.charCodeAt(i++)];
    const b = i < end ? _b64Lut[b64.charCodeAt(i++)] : 0;
    const c = i < end ? _b64Lut[b64.charCodeAt(i++)] : 0;
    const d = i < end ? _b64Lut[b64.charCodeAt(i++)] : 0;
    const n = (a << 18) | (b << 12) | (c << 6) | d;
    if (j < outLen) out[j++] = (n >> 16) & 0xff;
    if (j < outLen) out[j++] = (n >> 8) & 0xff;
    if (j < outLen) out[j++] = n & 0xff;
  }
  return out;
}

// Crosshair state (CSS coords relative to spectrum canvas).
let spectrumCrosshairX = null;
let spectrumCrosshairY = null;

// BW-strip drag state.
let _bwDragEdge     = null; // "left" | "right" | null
let _bwDragStartX   = 0;
let _bwDragStartBwHz = 0;
let _bwDragCanvas   = null;

function spectrumBgColor() {
  return canvasPalette().bg;
}

function clearSpectrumPeakHoldFrames() {
  spectrumPeakHoldFrames = [];
}

function settlePendingSpectrumFrameWaiters(frame) {
  if (!pendingSpectrumFrameWaiters.length) return;
  const remaining = [];
  for (const waiter of pendingSpectrumFrameWaiters) {
    if (!waiter) continue;
    const targetCenterHz = Number(waiter.targetCenterHz);
    if (
      Number.isFinite(targetCenterHz) &&
      (!frame || Math.abs(Number(frame.center_hz) - targetCenterHz) >= 2)
    ) {
      remaining.push(waiter);
      continue;
    }
    if (waiter.timer) {
      clearTimeout(waiter.timer);
      waiter.timer = null;
    }
    if (typeof waiter.resolve === "function") {
      waiter.resolve(frame);
    }
  }
  pendingSpectrumFrameWaiters = remaining;
}

function rejectPendingSpectrumFrameWaiters(error) {
  if (!pendingSpectrumFrameWaiters.length) return;
  for (const waiter of pendingSpectrumFrameWaiters) {
    if (!waiter) continue;
    if (waiter.timer) {
      clearTimeout(waiter.timer);
      waiter.timer = null;
    }
    if (typeof waiter.reject === "function") {
      waiter.reject(error || new Error("Spectrum unavailable"));
    }
  }
  pendingSpectrumFrameWaiters = [];
}

function waitForSpectrumFrame(expectedCenterHz = null, timeoutMs = 1200) {
  const targetCenterHz = Number(expectedCenterHz);
  if (
    lastSpectrumData &&
    (!Number.isFinite(targetCenterHz) || Math.abs(Number(lastSpectrumData.center_hz) - targetCenterHz) < 2)
  ) {
    return Promise.resolve(lastSpectrumData);
  }

  return new Promise((resolve, reject) => {
    const waiter = {
      targetCenterHz,
      resolve,
      reject,
      timer: null,
    };
    waiter.timer = setTimeout(() => {
      pendingSpectrumFrameWaiters = pendingSpectrumFrameWaiters.filter((entry) => entry !== waiter);
      reject(new Error("Timed out waiting for spectrum frame"));
    }, Math.max(200, timeoutMs));
    pendingSpectrumFrameWaiters.push(waiter);
  });
}

function pruneSpectrumPeakHoldFrames(now = Date.now()) {
  const holdMs = Math.max(0, Number.isFinite(overviewPeakHoldMs) ? overviewPeakHoldMs : 0);
  if (holdMs <= 0) {
    clearSpectrumPeakHoldFrames();
    return;
  }
  // In-place removal from front (frames are time-ordered).
  let removeCount = 0;
  for (let i = 0; i < spectrumPeakHoldFrames.length; i++) {
    const f = spectrumPeakHoldFrames[i];
    if (f && isBinsArray(f.bins) && now - f.t <= holdMs) break;
    removeCount++;
  }
  if (removeCount > 0) spectrumPeakHoldFrames.splice(0, removeCount);
}

function pushSpectrumPeakHoldFrame(frame) {
  if (!frame || !isBinsArray(frame.bins) || frame.bins.length === 0) {
    clearSpectrumPeakHoldFrames();
    return;
  }
  const holdMs = Math.max(0, Number.isFinite(overviewPeakHoldMs) ? overviewPeakHoldMs : 0);
  if (holdMs <= 0) {
    clearSpectrumPeakHoldFrames();
    return;
  }
  const now = Date.now();
  pruneSpectrumPeakHoldFrames(now);
  const lastFrame = spectrumPeakHoldFrames[spectrumPeakHoldFrames.length - 1];
  if (lastFrame && lastFrame.bins.length !== frame.bins.length) {
    clearSpectrumPeakHoldFrames();
  }
  spectrumPeakHoldFrames.push({ t: now, bins: frame.bins.slice() });
}

function buildSpectrumPeakHoldBins(currentBins) {
  const holdMs = Math.max(0, Number.isFinite(overviewPeakHoldMs) ? overviewPeakHoldMs : 0);
  if (holdMs <= 0 || !isBinsArray(currentBins) || currentBins.length === 0) {
    return null;
  }
  pruneSpectrumPeakHoldFrames();
  if (spectrumPeakHoldFrames.length === 0) return null;
  const peakBins = currentBins.slice();
  for (const frame of spectrumPeakHoldFrames) {
    if (!frame || !isBinsArray(frame.bins) || frame.bins.length !== peakBins.length) continue;
    for (let i = 0; i < peakBins.length; i++) {
      if (frame.bins[i] > peakBins[i]) peakBins[i] = frame.bins[i];
    }
  }
  return peakBins;
}

// Estimate noise floor as the 15th-percentile of visible bins (same heuristic as Auto).
// Uses O(N) nth-element selection instead of O(N log N) sort.
function estimateNoiseFloorDb(bins) {
  if (!isBinsArray(bins) || bins.length === 0) return null;
  const k = Math.floor(bins.length * 0.15);
  return nthElement(bins, k);
}

// O(N) average-case selection algorithm (Floyd-Rivest / quickselect).
function nthElement(arr, k) {
  const tmp = _nthScratch.length >= arr.length ? _nthScratch : new Float64Array(arr.length);
  if (tmp.length > _nthScratch.length) _nthScratch = tmp;
  for (let i = 0; i < arr.length; i++) tmp[i] = arr[i];
  let lo = 0, hi = arr.length - 1;
  while (lo < hi) {
    const pivot = tmp[lo + ((hi - lo) >> 1)];
    let i = lo, j = hi;
    while (i <= j) {
      while (tmp[i] < pivot) i++;
      while (tmp[j] > pivot) j--;
      if (i <= j) { const t = tmp[i]; tmp[i] = tmp[j]; tmp[j] = t; i++; j--; }
    }
    if (j < k) lo = i;
    if (k < i) hi = j;
  }
  return tmp[k];
}
let _nthScratch = new Float64Array(0);

// Pre-allocated buffer for smoothed spectrum bins (avoids .map() allocation per frame).
let _smoothBins = [];

function buildSpectrumRenderData(frame) {
  if (!frame || !isBinsArray(frame.bins)) return frame;
  const n = frame.bins.length;
  const prev = lastSpectrumRenderData;
  const canBlend =
    prev &&
    isBinsArray(prev.bins) &&
    prev.bins.length === n &&
    prev.sample_rate === frame.sample_rate &&
    prev.center_hz === frame.center_hz;
  if (_smoothBins.length !== n) _smoothBins = new Array(n);
  const src = frame.bins;
  if (canBlend) {
    const prevBins = prev.bins;
    const alpha = SPECTRUM_SMOOTH_ALPHA;
    for (let i = 0; i < n; i++) {
      _smoothBins[i] = prevBins[i] + (src[i] - prevBins[i]) * alpha;
    }
  } else {
    for (let i = 0; i < n; i++) _smoothBins[i] = src[i];
  }
  // Return object reusing the frame's metadata.
  return { bins: _smoothBins, center_hz: frame.center_hz, sample_rate: frame.sample_rate, rds: frame.rds };
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
  if (!data || !isBinsArray(data.bins) || data.bins.length === 0 || cssW <= 0) {
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

function spectrumTargetHzAt(cssX, cssW, data) {
  if (!data) return null;
  const range = spectrumVisibleRange(data);
  return nearestSpectrumPeakHz(cssX, cssW, data)
    ?? Math.round(canvasXToHz(cssX, cssW, range));
}

function visibleSpectrumPeakIndices(data, limit = 24) {
  if (!data || !isBinsArray(data.bins) || data.bins.length < 3) {
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
  const spectrumUrl = lastActiveRigId
    ? `/spectrum?remote=${encodeURIComponent(lastActiveRigId)}`
    : "/spectrum";
  spectrumSource = new EventSource(spectrumUrl);
  // Unnamed event = reset signal.
  spectrumSource.onmessage = (evt) => {
    if (evt.data === "null") {
      rejectPendingSpectrumFrameWaiters(new Error("Spectrum stream reset"));
      lastSpectrumData = null;
      lastSpectrumRenderData = null;
      clearSpectrumPeakHoldFrames();
      overviewWaterfallRows = [];
      overviewWaterfallPushCount = 0;
      overviewWfResetTextureCache();
      spectrumWfRows = [];
      spectrumWfPushCount = 0;
      spectrumWfTexReady = false;
      scheduleOverviewDraw();
      clearSpectrumCanvas();
      updateRdsPsOverlay(null);
    }
  };
  // Named "b" event = compact binary frame: "{center_hz},{sample_rate},{base64_i8_bins}"
  // Bins are i8 (1 dB/step), base64-encoded for ~5× size reduction vs JSON f32 array.
  // Named "b" event = compact binary frame: "{center_hz},{sample_rate},{base64_i8_bins}"
  // Bins are i8 (1 dB/step), base64-encoded for ~5× size reduction vs JSON f32 array.
  spectrumSource.addEventListener("b", (evt) => {
    try {
      const commaA = evt.data.indexOf(",");
      const commaB = evt.data.indexOf(",", commaA + 1);
      const centerHz = Number(evt.data.slice(0, commaA));
      const sampleRate = Number(evt.data.slice(commaA + 1, commaB));
      const b64 = evt.data.slice(commaB + 1);
      const hadSpectrum = !!lastSpectrumData;
      const bins = decodeBase64ToInt8(b64);
      // Preserve any RDS data from the last rds event.
      const rds = lastSpectrumData?.rds;
      lastSpectrumData = { bins, center_hz: centerHz, sample_rate: sampleRate, rds };
      window.lastSpectrumData = lastSpectrumData;
      // Server confirmed a new center — clear optimistic pending value.
      if (spectrumCenterPendingHz !== null && Math.abs(centerHz - spectrumCenterPendingHz) < 1000) {
        spectrumCenterPendingHz = null;
      }
      lastSpectrumRenderData = buildSpectrumRenderData(lastSpectrumData);
      settlePendingSpectrumFrameWaiters(lastSpectrumData);
      pushSpectrumPeakHoldFrame(lastSpectrumRenderData);
      pushOverviewWaterfallFrame(lastSpectrumData);
      pushSpectrumWaterfallFrame(lastSpectrumData);
      refreshCenterFreqDisplay();
      if (window.refreshCwTonePicker) window.refreshCwTonePicker();
      scheduleSpectrumDraw();
      if (!hadSpectrum) {
        updateRdsPsOverlay(lastSpectrumData.rds);
      } else {
        positionRdsPsOverlay();
      }
    } catch (_) {}
  });
  // Named "rds" event = RDS metadata changed (emitted only when it changes).
  spectrumSource.addEventListener("rds", (evt) => {
    try {
      const rds = evt.data === "null" ? undefined : JSON.parse(evt.data);
      if (lastSpectrumData) lastSpectrumData.rds = rds;
      updateRdsPsOverlay(rds ?? null);
    } catch (_) {}
  });
  spectrumSource.addEventListener("rds_vchan", (evt) => {
    try {
      const payload = evt.data === "null" ? [] : JSON.parse(evt.data);
      const next = new Map();
      const nextSig = new Map();
      if (Array.isArray(payload)) {
        payload.forEach((entry) => {
          if (entry && entry.id) {
            next.set(entry.id, entry.rds ?? null);
            if (typeof entry.signal_db === "number") nextSig.set(entry.id, entry.signal_db);
          }
        });
      }
      vchanRdsById = next;
      vchanSignalDbById = nextSig;
      if (typeof vchanActiveId !== "undefined" && vchanActiveId && nextSig.has(vchanActiveId)) {
        sigLastDbm = nextSig.get(vchanActiveId);
        refreshSigStrengthDisplay();
      }
      updateRdsPsOverlay(primaryRds);
    } catch (_) {}
  });
  spectrumSource.onerror = () => {
    rejectPendingSpectrumFrameWaiters(new Error("Spectrum stream disconnected"));
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
  rejectPendingSpectrumFrameWaiters(new Error("Spectrum streaming stopped"));
  clearSpectrumPeakHoldFrames();
  overviewWaterfallRows = [];
  overviewWaterfallPushCount = 0;
  overviewWfResetTextureCache();
  spectrumWfRows = [];
  spectrumWfPushCount = 0;
  spectrumWfTexReady = false;
  scheduleOverviewDraw();
  updateRdsPsOverlay(null);
  clearSpectrumCanvas();
}

// ── Rendering ────────────────────────────────────────────────────────────────
function clearSpectrumCanvas() {
  if (!spectrumCanvas || !spectrumGl || !spectrumGl.ready) return;
  const cssW = spectrumCanvas.clientWidth || 1;
  const cssH = spectrumCanvas.clientHeight || 1;
  spectrumGl.ensureSize(cssW, cssH, window.devicePixelRatio || 1);
  spectrumGl.clear(cssColorToRgba(spectrumBgColor()));
  if (spectrumDbAxis) {
    spectrumDbAxis.replaceChildren();
    spectrumDbAxisKey = "";
  }
}

function formatOverlayPs(ps) {
  return String(ps ?? "")
    .slice(0, 8)
    .padEnd(8, "_")
    .replaceAll(" ", "_");
}

function formatPsHtml(ps) {
  const clipped = String(ps ?? "").slice(0, 8);
  let html = "";
  for (let i = 0; i < 8; i += 1) {
    const ch = clipped[i];
    if (ch == null || ch === " ") {
      html += `<span class="rds-ps-gap">_</span>`;
    } else {
      html += escapeMapHtml(ch);
    }
  }
  return html;
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
  const freqHz = activeChannelFreqHz();
  return {
    time: formatMinuteTimestamp(),
    freq_hz: Number.isFinite(freqHz) ? Math.round(freqHz) : null,
    ...rds,
  };
}

function formatRdsAfMHz(hz) {
  return `${(hz / 1_000_000).toFixed(1)} MHz`;
}

function tuneRdsAlternativeFrequency(hz) {
  if (!Number.isFinite(hz) || hz <= 0) return;
  const targetHz = Math.round(hz);
  setRigFrequency(targetHz);
  showHint(`Tuned ${formatRdsAfMHz(targetHz)}`, 1200);
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
  afEl.replaceChildren();
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

async function copyRdsPsToClipboard(rdsOverride = null, freqOverrideHz = null) {
  const rds = rdsOverride || activeChannelRds();
  const ps = rds?.program_service;
  if (!rds || !ps || ps.length === 0) {
    showHint("No RDS PS", 1200);
    return;
  }
  const freqHz = Number.isFinite(freqOverrideHz) ? freqOverrideHz : activeChannelFreqHz();
  const freqMhz = Number.isFinite(freqHz) ? (Math.round((freqHz / 100_000)) / 10).toFixed(1) : "--.-";
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
  primaryRds = rds || null;
  const activeRds = activeChannelRds();
  updateDocumentTitle(activeRds);
  renderRdsOverlays();

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

  if (!activeRds) {
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
      const freqHz = activeChannelFreqHz();
      rawEl.textContent = JSON.stringify({
        time: formatMinuteTimestamp(),
        freq_hz: Number.isFinite(freqHz) ? Math.round(freqHz) : null,
        ...rest,
      }, null, 2);
    }
    return;
  }

  statusEl.textContent = "Decoding";
  statusEl.className = "rds-value rds-decoding";
  piEl.textContent = activeRds.pi != null ? `0x${activeRds.pi.toString(16).toUpperCase().padStart(4, "0")}` : "--";
  if (psEl) {
    if (activeRds.program_service) {
      psEl.innerHTML = formatPsHtml(activeRds.program_service);
    } else {
      psEl.textContent = "--";
    }
  }
  ptyEl.textContent = activeRds.pty_name ?? (activeRds.pty != null ? String(activeRds.pty) : "--");
  ptyNameEl.textContent = activeRds.pty != null ? String(activeRds.pty) : "--";
  if (ptynEl) ptynEl.textContent = activeRds.program_type_name_long ?? "--";
  if (tpEl) tpEl.textContent = formatRdsFlag(activeRds.traffic_program);
  if (taEl) taEl.textContent = formatRdsFlag(activeRds.traffic_announcement);
  if (musicEl) musicEl.textContent = formatRdsAudio(activeRds.music);
  if (stereoEl) stereoEl.textContent = formatRdsFlag(activeRds.stereo);
  if (compEl) compEl.textContent = formatRdsFlag(activeRds.compressed);
  if (headEl) headEl.textContent = formatRdsFlag(activeRds.artificial_head);
  if (dynPtyEl) dynPtyEl.textContent = formatRdsFlag(activeRds.dynamic_pty);
  renderRdsAlternativeFrequencies(activeRds.alternative_frequencies_hz);
  if (rtEl) rtEl.textContent = activeRds.radio_text ?? "--";
  rawEl.textContent = JSON.stringify(buildRdsRawPayload(activeRds), null, 2);
}

window.refreshRdsUi = () => updateRdsPsOverlay(primaryRds);

function scheduleSpectrumDraw() {
  if (spectrumDrawPending) return;
  spectrumDrawPending = true;
  requestAnimationFrame(() => {
    spectrumDrawPending = false;
    if (lastSpectrumRenderData) {
      drawSpectrum(lastSpectrumRenderData);
      if (overviewWaterfallRows.length > 0) scheduleOverviewDraw();
      if (spectrumWfRows.length > 0) scheduleSpectrumWaterfallDraw();
    }
  });
}

function drawSpectrum(data) {
  if (!spectrumCanvas || !spectrumGl || !spectrumGl.ready) return;

  const dpr = _cachedDpr;
  const cssW = _cachedSpectrumCssW;
  const cssH = _cachedSpectrumCssH;
  spectrumGl.ensureSize(cssW, cssH, dpr);
  const W = spectrumCanvas.width;
  const H = spectrumCanvas.height;

  const pal = canvasPalette();
  const range = spectrumVisibleRange(data);
  const bins = data.bins;
  const peakHoldBins = buildSpectrumPeakHoldBins(bins);
  const n = bins.length;

  spectrumGl.clear(cssColorToRgba(pal.bg));
  if (!n) return;

  const DB_MIN = spectrumFloor;
  const DB_MAX = spectrumFloor + spectrumRange;
  const dbRange = DB_MAX - DB_MIN;
  const fullSpanHz = data.sample_rate;
  const loHz = data.center_hz - fullSpanHz / 2;

  const gridStep = spectrumRange > 100 ? 20 : 10;
  spectrumTmpGridSegments.length = 0;
  for (let db = Math.ceil(DB_MIN / gridStep) * gridStep; db <= DB_MAX; db += gridStep) {
    const y = Math.round(H * (1 - (db - DB_MIN) / dbRange));
    spectrumTmpGridSegments.push(0, y, W, y);
  }
  spectrumGl.drawSegments(spectrumTmpGridSegments, cssColorToRgba(pal.spectrumGrid), 1);
  updateSpectrumDbAxis(DB_MIN, DB_MAX, gridStep, H, dpr);

  function hzToX(hz) {
    return ((hz - range.visLoHz) / range.visSpanHz) * W;
  }
  function binX(i) {
    return hzToX(loHz + (i / (n - 1)) * fullSpanHz);
  }
  function binYFromBins(srcBins, i) {
    const db = Math.max(DB_MIN, Math.min(DB_MAX, srcBins[i]));
    return H * (1 - (db - DB_MIN) / dbRange);
  }

  spectrumTmpFillPoints.length = 0;
  for (let i = 0; i < n; i++) {
    spectrumTmpFillPoints.push(binX(i), binYFromBins(bins, i));
  }
  spectrumGl.drawFilledArea(spectrumTmpFillPoints, H, cssColorToRgba(pal.spectrumFill));

  if (isBinsArray(peakHoldBins) && peakHoldBins.length === n) {
    spectrumTmpPeakPoints.length = 0;
    for (let i = 0; i < n; i++) {
      spectrumTmpPeakPoints.push(binX(i), binYFromBins(peakHoldBins, i));
    }
    spectrumGl.drawPolyline(spectrumTmpPeakPoints, rgbaWithAlpha(pal.waveformPeak, 0.7), Math.max(1, dpr * 0.9));
  }

  spectrumGl.drawPolyline(spectrumTmpFillPoints, cssColorToRgba(pal.spectrumLine), Math.max(1, dpr));

  // ── Noise floor reference line ──
  const noiseDb = estimateNoiseFloorDb(bins);
  if (noiseDb != null && noiseDb >= DB_MIN && noiseDb <= DB_MAX) {
    const noiseY = Math.round(H * (1 - (noiseDb - DB_MIN) / dbRange));
    const nfSegments = [];
    const dashLen = Math.max(4, Math.round(6 * dpr));
    const gapLen = Math.max(3, Math.round(5 * dpr));
    for (let x = 0; x < W; x += dashLen + gapLen) {
      nfSegments.push(x, noiseY, Math.min(W, x + dashLen), noiseY);
    }
    spectrumGl.drawSegments(nfSegments, rgbaWithAlpha(pal.waveformPeak, 0.35), Math.max(1, dpr * 0.8));
  }

  const markerPeaks = visibleSpectrumPeakIndices(data);
  if (markerPeaks.length > 0) {
    spectrumTmpMarkerPoints.length = 0;
    for (const idx of markerPeaks) {
      spectrumTmpMarkerPoints.push(binX(idx), binYFromBins(bins, idx));
    }
    spectrumGl.drawPoints(spectrumTmpMarkerPoints, Math.max(2, dpr * 1.6), cssColorToRgba(pal.waveformPeak));
  }

  // ── Crosshair lines ──
  if (spectrumCrosshairX != null && spectrumCrosshairY != null) {
    const cx = spectrumCrosshairX * dpr;
    const cy = spectrumCrosshairY * dpr;
    const chColor = rgbaWithAlpha(pal.spectrumLabel, 0.5);
    spectrumGl.drawSegments([cx, 0, cx, H], chColor, Math.max(1, dpr * 0.6));
    spectrumGl.drawSegments([0, cy, W, cy], chColor, Math.max(1, dpr * 0.6));
  }

  // ── Zoom indicator ──
  if (_spectrumZoomEl) {
    if (spectrumZoom > 1.01) {
      _spectrumZoomEl.textContent = spectrumZoom.toFixed(1) + "x";
      _spectrumZoomEl.style.display = "block";
    } else {
      _spectrumZoomEl.style.display = "none";
    }
  }

  // ── Zoom minimap ──
  if (_spectrumMinimapEl) {
    if (spectrumZoom > 1.01) {
      _spectrumMinimapEl.style.display = "block";
      const viewFrac = 1 / spectrumZoom;
      const halfVis = viewFrac / 2;
      const panClamped = Math.min(Math.max(spectrumPanFrac, halfVis), 1 - halfVis);
      const viewL = panClamped - halfVis;
      const viewR = panClamped + halfVis;
      if (_spectrumMinimapInner) {
        _spectrumMinimapInner.style.left = (viewL * 100) + "%";
        _spectrumMinimapInner.style.width = ((viewR - viewL) * 100) + "%";
      }
    } else {
      _spectrumMinimapEl.style.display = "none";
    }
  }

  updateSpectrumFreqAxis(range);
  updateBookmarkAxis(range);
  updateBandplanStrip(range);  // use precise spectrum range when available
  drawSignalOverlay();
}

// ── Full waterfall panel below spectrum ───────────────────────────────────────
const spectrumWaterfallCanvas = document.getElementById("spectrum-waterfall-canvas");
const spectrumWaterfallGl = (typeof createTrxWebGlRenderer === "function" && spectrumWaterfallCanvas)
  ? createTrxWebGlRenderer(spectrumWaterfallCanvas, spectrumSnapshotGlOptions)
  : null;
let spectrumWfRows = [];
let spectrumWfPushCount = 0;
let spectrumWfTexData = null;
let spectrumWfTexWidth = 0;
let spectrumWfTexHeight = 0;
let spectrumWfTexPushCount = 0;
let spectrumWfTexPalKey = "";
let spectrumWfTexReady = false;
let spectrumWfDrawPending = false;
const SPECTRUM_WF_TEX_MAX_W = 1024;

// Cached DOM references for drawSpectrum (avoid getElementById per frame).
const _spectrumZoomEl = document.getElementById("spectrum-zoom-indicator");
const _spectrumMinimapEl = document.getElementById("spectrum-minimap");
const _spectrumMinimapInner = _spectrumMinimapEl ? _spectrumMinimapEl.querySelector(".minimap-view") : null;

// Cached canvas dimensions (updated on resize instead of reading clientWidth/clientHeight per frame).
let _cachedSpectrumCssW = 640, _cachedSpectrumCssH = 160;
let _cachedSpecWfCssW = 640, _cachedSpecWfCssH = 120;
let _cachedDpr = window.devicePixelRatio || 1;

function _updateCachedCanvasSizes() {
  _cachedDpr = window.devicePixelRatio || 1;
  if (spectrumCanvas) {
    _cachedSpectrumCssW = spectrumCanvas.clientWidth || 640;
    _cachedSpectrumCssH = spectrumCanvas.clientHeight || 160;
  }
  if (spectrumWaterfallCanvas) {
    _cachedSpecWfCssW = spectrumWaterfallCanvas.clientWidth || 640;
    _cachedSpecWfCssH = spectrumWaterfallCanvas.clientHeight || 120;
  }
}
// Refresh on resize; also called from scheduleSpectrumLayout.
window.addEventListener("resize", _updateCachedCanvasSizes);
// Initial read.
_updateCachedCanvasSizes();

function pushSpectrumWaterfallFrame(data) {
  if (!spectrumWaterfallCanvas || !data || !isBinsArray(data.bins) || data.bins.length === 0) return;
  spectrumWfRows.push(data.bins.slice());
  spectrumWfPushCount++;
  trimSpectrumWaterfallRows();
  scheduleSpectrumWaterfallDraw();
}

function trimSpectrumWaterfallRows() {
  if (!spectrumWaterfallCanvas) return;
  const maxRows = Math.max(1, Math.floor(_cachedSpecWfCssH * _cachedDpr));
  if (spectrumWfRows.length > maxRows) {
    spectrumWfRows.splice(0, spectrumWfRows.length - maxRows);
  }
}

function scheduleSpectrumWaterfallDraw() {
  if (!spectrumWaterfallCanvas || spectrumWfDrawPending) return;
  spectrumWfDrawPending = true;
  requestAnimationFrame(() => {
    spectrumWfDrawPending = false;
    drawSpectrumWaterfall();
  });
}

function drawSpectrumWaterfall() {
  if (!spectrumWaterfallCanvas || !spectrumWaterfallGl || !spectrumWaterfallGl.ready) return;
  if (!lastSpectrumData || spectrumWfRows.length === 0) return;

  const dpr = _cachedDpr;
  const cssW = _cachedSpecWfCssW;
  const cssH = _cachedSpecWfCssH;
  spectrumWaterfallGl.ensureSize(cssW, cssH, dpr);
  const W = spectrumWaterfallCanvas.width;
  const H = spectrumWaterfallCanvas.height;
  if (W <= 0 || H <= 0) return;

  const pal = canvasPalette();
  const maxVisible = Math.max(1, Math.floor(H));
  const rows = spectrumWfRows.slice(-maxVisible);
  if (rows.length === 0) return;

  const iW = Math.max(96, Math.min(SPECTRUM_WF_TEX_MAX_W, Math.ceil(W / 2)));
  const iH = Math.max(1, rows.length);
  const minDb = Number.isFinite(spectrumFloor) ? spectrumFloor : -115;
  const maxDb = minDb + Math.max(20, Number.isFinite(spectrumRange) ? spectrumRange : 90);
  const view = spectrumVisibleRange(lastSpectrumData);
  const viewKey = `${Math.round(view.visLoHz)}:${Math.round(view.visHiHz)}`;
  const palKey = `swf|${pal.waterfallHue}|${pal.waterfallSat}|${pal.waterfallLight}|${pal.waterfallAlpha}|${spectrumFloor}|${spectrumRange}|${waterfallGamma}|${viewKey}`;
  const rowStride = iW * 4;
  const expectedSize = iW * iH * 4;
  const newPushes = spectrumWfPushCount - spectrumWfTexPushCount;
  const sizeChanged = spectrumWfTexWidth !== iW || spectrumWfTexHeight !== iH;
  const palChanged = spectrumWfTexPalKey !== palKey;
  const needsFull = !spectrumWfTexData || sizeChanged || palChanged || spectrumWfTexPushCount === 0;
  let texUpdated = false;

  if (!spectrumWfTexData || spectrumWfTexData.length !== expectedSize) {
    spectrumWfTexData = new Uint8Array(expectedSize);
  }
  spectrumWfTexWidth = iW;
  spectrumWfTexHeight = iH;

  ensureWaterfallLut(pal, minDb, maxDb);

  function renderRow(dstY, srcBins) {
    if (!isBinsArray(srcBins) || srcBins.length === 0) return;
    const { startIdx, endIdx } = overviewVisibleBinWindow(lastSpectrumData, srcBins.length);
    const spanBins = Math.max(1, endIdx - startIdx);
    const rowBase = dstY * rowStride;
    const iwM1 = Math.max(1, iW - 1);
    for (let x = 0; x < iW; x++) {
      const binIdx = Math.min(endIdx, startIdx + ((x * spanBins / iwM1) | 0));
      waterfallLutWrite(spectrumWfTexData, rowBase + x * 4, srcBins[binIdx]);
    }
  }

  if (needsFull) {
    for (let y = 0; y < iH; y++) renderRow(y, rows[y]);
    spectrumWfTexPushCount = spectrumWfPushCount;
    spectrumWfTexPalKey = palKey;
    texUpdated = true;
  } else if (newPushes > 0) {
    const newCount = Math.min(newPushes, iH);
    if (newCount >= iH) {
      for (let y = 0; y < iH; y++) renderRow(y, rows[y]);
    } else {
      const shiftBytes = newCount * rowStride;
      spectrumWfTexData.copyWithin(0, shiftBytes);
      const startRow = iH - newCount;
      for (let y = startRow; y < iH; y++) renderRow(y, rows[y]);
    }
    spectrumWfTexPushCount = spectrumWfPushCount;
    spectrumWfTexPalKey = palKey;
    texUpdated = true;
  }

  if (texUpdated || !spectrumWfTexReady) {
    spectrumWaterfallGl.uploadRgbaTexture("spectrum-waterfall", iW, iH, spectrumWfTexData, "linear");
    spectrumWfTexReady = true;
  }
  spectrumWaterfallGl.drawTexture("spectrum-waterfall", 0, 0, W, H, 1, true);
}

function bmHexToRgba(hex, alpha) {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}

// WCAG relative luminance; threshold 0.4 splits well across the palette.
function bmLuminance(hex) {
  const lin = (c) => c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  const r = lin(parseInt(hex.slice(1, 3), 16) / 255);
  const g = lin(parseInt(hex.slice(3, 5), 16) / 255);
  const b = lin(parseInt(hex.slice(5, 7), 16) / 255);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function bmContrastFg(bgHex) {
  return bmLuminance(bgHex) >= 0.4 ? "#1a202c" : "#ffffff";
}

// Read a theme CSS colour variable from the live theme and return it as a hex string.
function bmResolveThemeColor(name, fallbackHex) {
  const val = getComputedStyle(document.documentElement)
    .getPropertyValue(name).trim();
  if (/^#[0-9a-f]{6}$/i.test(val)) return val;
  if (/^#[0-9a-f]{3}$/i.test(val))
    return "#" + [...val.slice(1)].map((c) => c + c).join("");
  const m = val.match(/\d+/g);
  if (m && m.length >= 3)
    return "#" + m.slice(0, 3).map((n) => (+n).toString(16).padStart(2, "0")).join("");
  return fallbackHex;
}

function bmBlendHex(aHex, bHex, ratio = 0.5) {
  const mix = Math.max(0, Math.min(1, Number.isFinite(ratio) ? ratio : 0.5));
  const aR = parseInt(aHex.slice(1, 3), 16);
  const aG = parseInt(aHex.slice(3, 5), 16);
  const aB = parseInt(aHex.slice(5, 7), 16);
  const bR = parseInt(bHex.slice(1, 3), 16);
  const bG = parseInt(bHex.slice(3, 5), 16);
  const bB = parseInt(bHex.slice(5, 7), 16);
  const toHex = (value) => Math.round(value).toString(16).padStart(2, "0");
  return "#" + [
    aR + (bR - aR) * mix,
    aG + (bG - aG) * mix,
    aB + (bB - aB) * mix,
  ].map(toHex).join("");
}

function bmThemePalette() {
  const yellow = bmResolveThemeColor("--accent-yellow", "#f0ad4e");
  const green = bmResolveThemeColor("--accent-green", "#c24b1a");
  const red = bmResolveThemeColor("--accent-red", "#e55353");
  const heading = bmResolveThemeColor("--text-heading", "#c6d5ea");
  const border = bmResolveThemeColor("--border-light", "#304766");
  return [
    yellow,
    bmBlendHex(yellow, heading, 0.28),
    bmBlendHex(yellow, green, 0.45),
    bmBlendHex(green, heading, 0.22),
    bmBlendHex(yellow, red, 0.42),
    bmBlendHex(red, heading, 0.18),
    bmBlendHex(border, yellow, 0.58),
    bmBlendHex(border, heading, 0.5),
  ];
}

// Returns a map of category → hex colour, including "" for uncategorised.
function bmCategoryColorMap() {
  const ref = typeof bmOverlayList !== "undefined" ? bmOverlayList : [];
  const cats = [...new Set(ref.map((b) => b.category).filter(Boolean))].sort();
  const palette = bmThemePalette();
  const map = { "": palette[0] };
  cats.forEach((cat, i) => { map[cat] = palette[(i + 1) % palette.length]; });
  return map;
}

function createBookmarkChip(bm, colorMap, options = {}) {
  const span = document.createElement("span");
  const freqStr = typeof bmFmtFreq === "function"
    ? bmFmtFreq(bm.freq_hz) : bm.freq_hz + "\u202fHz";
  const esc = (s) => String(s)
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  span.className = "spectrum-bookmark-chip";
  if (options.sideStack) {
    span.classList.add("spectrum-bookmark-chip-side");
  } else {
    // Keep main in-band bookmark chips pinned at the very top of the spectrum strip.
    span.style.top = "2px";
    span.style.transform = "translateX(-50%)";
  }
  span.title = buildBookmarkTooltipText(bm) || (bm.name + " \u2014 " + freqStr + (bm.comment ? "\n" + bm.comment : ""));
  span.dataset.bmId = bm.id;
  const labelHtml = options.sideStack
    ? (
      `<span class="spectrum-bookmark-side-head">` +
      `<svg class='bm-icon-svg' viewBox='0 0 8 12' width='8' height='12' aria-hidden='true'>` +
      "<path d='M0,0 h8 v10 l-4,2 l-4,-2 Z'/>" +
      `</svg>` +
      `<span class="spectrum-bookmark-freq">${escapeMapHtml(freqStr)}</span>` +
      `</span>` +
      `<span class="spectrum-bookmark-name">${escapeMapHtml(bm.name)}</span>`
    )
    : (
      "<svg class='bm-icon-svg' viewBox='0 0 8 12' width='8' height='12' aria-hidden='true'>" +
      "<path d='M0,0 h8 v10 l-4,2 l-4,-2 Z'/>" +
      "</svg>\u00a0<span class='spectrum-bookmark-name'>" + escapeMapHtml(bm.name) + "</span>"
    );
  span.innerHTML =
    labelHtml;
  const col = colorMap[bm.category || ""];
  span.style.setProperty("--bm-cat-bg", col);
  span.style.setProperty("--bm-cat-fg", bmContrastFg(col));
  span.addEventListener("click", () => {
    if (typeof bmApply === "function") bmApply(bm);
  });
  return span;
}

function updateSideBookmarkStack(container, bookmarks, colorMap) {
  if (!container) return;
  const rev = typeof bmOverlayRevision !== "undefined" ? bmOverlayRevision : 0;
  const nextKey = Array.isArray(bookmarks) ? `${rev}:${bookmarks.map((bm) => bm.id).join(",")}` : "";
  if (!Array.isArray(bookmarks) || bookmarks.length === 0) {
    if (container.dataset.bmKey) {
      container.replaceChildren();
      container.dataset.bmKey = "";
    }
    container.classList.remove("bm-side-visible");
    return;
  }

  if (container.dataset.bmKey !== nextKey) {
    container.dataset.bmKey = nextKey;
    container.replaceChildren();
    for (const bm of bookmarks) {
      container.appendChild(createBookmarkChip(bm, colorMap, { sideStack: true }));
    }
  }

  container.classList.add("bm-side-visible");
}

function updateBookmarkAxis(range) {
  const axisEl = document.getElementById("spectrum-bookmark-axis");
  const leftSideEl = document.getElementById("spectrum-bookmark-side-left");
  const rightSideEl = document.getElementById("spectrum-bookmark-side-right");
  if (!axisEl) return;

  const _bmRef = typeof bmOverlayList !== "undefined" ? bmOverlayList : null;
  const allBookmarks = Array.isArray(_bmRef) ? _bmRef : [];
  const visBookmarks = allBookmarks.filter((bm) => bm.freq_hz >= range.visLoHz && bm.freq_hz <= range.visHiHz);
  const leftBookmarks = allBookmarks
    .filter((bm) => bm.freq_hz < range.visLoHz)
    .sort((a, b) => b.freq_hz - a.freq_hz)
    .slice(0, 3);
  const rightBookmarks = allBookmarks
    .filter((bm) => bm.freq_hz > range.visHiHz)
    .sort((a, b) => a.freq_hz - b.freq_hz)
    .slice(0, 3);
  const colorMap = bmCategoryColorMap();

  updateSideBookmarkStack(leftSideEl, leftBookmarks, colorMap);
  updateSideBookmarkStack(rightSideEl, rightBookmarks, colorMap);

  const hasVisible = visBookmarks.length > 0;
  axisEl.classList.toggle("bm-axis-visible", hasVisible);

  if (!hasVisible) {
    if (axisEl.dataset.bmKey) { axisEl.replaceChildren(); axisEl.dataset.bmKey = ""; }
    return;
  }

  // Only rebuild DOM when the set of visible bookmarks changes.
  // Positions are always updated to handle pan/zoom smoothly.
  const rev = typeof bmOverlayRevision !== "undefined" ? bmOverlayRevision : 0;
  const newKey = `${rev}:${visBookmarks.map((b) => b.id).join(",")}`;
  if (axisEl.dataset.bmKey !== newKey) {
    axisEl.dataset.bmKey = newKey;
    axisEl.replaceChildren();
    for (const bm of visBookmarks) {
      axisEl.appendChild(createBookmarkChip(bm, colorMap));
    }
  }

  // Always recompute horizontal positions (pan/zoom changes frac every frame).
  const axisWidth = axisEl.clientWidth || 0;
  const edgePad = 8;
  const spans = axisEl.querySelectorAll(":scope > span");
  visBookmarks.forEach((bm, i) => {
    const span = spans[i];
    if (!span) return;
    const frac = (bm.freq_hz - range.visLoHz) / range.visSpanHz;
    if (axisWidth > 0) {
      const lw = span.offsetWidth || 0;
      const clamped = Math.max(edgePad + lw / 2, Math.min(axisWidth - edgePad - lw / 2, frac * axisWidth));
      span.style.left = clamped + "px";
    } else {
      span.style.left = (frac * 100).toFixed(2) + "%";
    }
  });
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
  const leftShiftBtn = document.getElementById("spectrum-center-left-btn");
  const rightShiftBtn = document.getElementById("spectrum-center-right-btn");
  spectrumFreqAxis.replaceChildren();
  if (leftShiftBtn) spectrumFreqAxis.appendChild(leftShiftBtn);
  if (rightShiftBtn) spectrumFreqAxis.appendChild(rightShiftBtn);
  const axisWidth = spectrumFreqAxis.clientWidth || 0;
  const buttonReserve = Math.max(
    leftShiftBtn?.offsetWidth || 0,
    rightShiftBtn?.offsetWidth || 0,
    0,
  );
  const edgePad = Math.max(6, buttonReserve + 10);
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
    spectrumFreqAxis.appendChild(span);
    const labelWidth = span.offsetWidth || 0;
    if (axisWidth > 0 && labelWidth > 0) {
      const minCenter = edgePad + labelWidth / 2;
      const maxCenter = axisWidth - edgePad - labelWidth / 2;
      const desiredCenter = frac * axisWidth;
      const clampedCenter = Math.max(minCenter, Math.min(maxCenter, desiredCenter));
      span.style.left = `${clampedCenter}px`;
    } else {
      span.style.left = (frac * 100).toFixed(2) + "%";
    }
  }
}

function updateSpectrumDbAxis(dbMin, dbMax, gridStep, heightPx, dpr) {
  if (!spectrumDbAxis) return;
  const key = [
    Math.round(dbMin),
    Math.round(dbMax),
    Math.round(gridStep),
    Math.round(heightPx),
    Math.round((dpr || 1) * 100),
    currentTheme(),
    currentStyle(),
  ].join(":");
  if (key === spectrumDbAxisKey) return;
  spectrumDbAxisKey = key;
  spectrumDbAxis.replaceChildren();

  const spanDb = Math.max(1, dbMax - dbMin);
  const cssHeight = heightPx / Math.max(1, dpr || 1);
  for (let db = Math.ceil(dbMin / gridStep) * gridStep; db <= dbMax; db += gridStep) {
    const yPx = Math.round(heightPx * (1 - (db - dbMin) / spanDb));
    const yCss = yPx / Math.max(1, dpr || 1);
    if (yCss <= 7 || yCss >= cssHeight - 4) continue;
    const span = document.createElement("span");
    span.textContent = `${db}`;
    span.style.top = `${yCss}px`;
    spectrumDbAxis.appendChild(span);
  }
}


// ── Screenshot module (extracted to screenshot.js, loaded on demand) ────────
// Spectrum screenshot capture (~245 lines) moved to screenshot.js.

function shouldIgnoreGlobalShortcut(target) {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  if (target.isContentEditable) return true;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  return !!target.closest("[contenteditable='true']");
}

// ── Shortcut help overlay ─────────────────────────────────────────────────────
function toggleShortcutOverlay() {
  const el = document.getElementById("shortcut-overlay");
  if (!el) return;
  el.classList.toggle("is-hidden");
}
function hideShortcutOverlay() {
  const el = document.getElementById("shortcut-overlay");
  if (el) el.classList.add("is-hidden");
}
function isShortcutOverlayVisible() {
  const el = document.getElementById("shortcut-overlay");
  return el && !el.classList.contains("is-hidden");
}
document.addEventListener("DOMContentLoaded", () => {
  const overlay = document.getElementById("shortcut-overlay");
  if (overlay) overlay.addEventListener("click", (e) => {
    if (e.target === overlay) hideShortcutOverlay();
  });
});

window.addEventListener("keydown", (event) => {
  if (event.defaultPrevented || event.repeat || event.isComposing) return;

  const key = (event.key || "").toLowerCase();

  // F1 — toggle shortcut help
  if (event.key === "F1") {
    event.preventDefault();
    toggleShortcutOverlay();
    return;
  }

  // Escape — close shortcut overlay if open
  if (event.key === "Escape" && isShortcutOverlayVisible()) {
    event.preventDefault();
    hideShortcutOverlay();
    return;
  }

  // F — focus frequency input
  if (key === "f" && !event.ctrlKey && !event.metaKey && !event.altKey && !shouldIgnoreGlobalShortcut(event.target)) {
    event.preventDefault();
    const fi = document.getElementById("freq");
    if (fi) { fi.focus(); fi.select(); }
    return;
  }

  if (event.ctrlKey || event.metaKey || event.altKey) return;
  if (shouldIgnoreGlobalShortcut(event.target)) return;

  // S — spectrum screenshot (lazy-loads screenshot.js on first use)
  if (key === "s") {
    event.preventDefault();
    if (window.trx.screenshot) {
      void window.trx.screenshot.captureSpectrumScreenshot();
    } else {
      const s = document.createElement("script");
      s.src = "/screenshot.js";
      s.onload = () => { void window.trx.screenshot?.captureSpectrumScreenshot(); };
      document.body.appendChild(s);
    }
    return;
  }

  // R — round frequency to nearest jog step boundary
  if (key === "r") {
    event.preventDefault();
    if (lastLocked) { showHint("Locked", 1500); return; }
    if (lastFreqHz != null) {
      const step = Math.max(1, jogStep);
      const rounded = Math.round(lastFreqHz / step) * step;
      if (rounded !== lastFreqHz) {
        if (!freqAllowed(rounded)) { showUnsupportedFreqPopup(rounded); return; }
        setRigFrequency(rounded);
        showHint(`Rounded → ${formatFreq(rounded)}`, 1200);
      } else {
        showHint("Already on step", 1200);
      }
    }
    return;
  }

  // B — jump to previous frequency/bw/mode/decode state
  if (key === "b") {
    event.preventDefault();
    void restorePreviousTuneState();
    return;
  }

  // [ — narrow bandwidth by 10 kHz
  if (key === "[") {
    event.preventDefault();
    const [, minBw] = mwDefaultsForMode(modeEl ? modeEl.value : "USB");
    const next = Math.max(minBw, currentBandwidthHz - 10_000);
    if (next !== currentBandwidthHz) {
      currentBandwidthHz = next;
      window.currentBandwidthHz = currentBandwidthHz;
      syncBandwidthInput(next);
      positionFastOverlay(lastFreqHz, next);
      if (lastSpectrumData) scheduleSpectrumDraw();
      postPath(`/set_bandwidth?hz=${next}`).catch(() => {});
      showHint(`BW ${formatBwLabel(next)}`, 1200);
    }
    return;
  }

  // ] — widen bandwidth by 10 kHz
  if (key === "]") {
    event.preventDefault();
    const [, , maxBw] = mwDefaultsForMode(modeEl ? modeEl.value : "USB");
    const next = Math.min(maxBw, currentBandwidthHz + 10_000);
    if (next !== currentBandwidthHz) {
      currentBandwidthHz = next;
      window.currentBandwidthHz = currentBandwidthHz;
      syncBandwidthInput(next);
      positionFastOverlay(lastFreqHz, next);
      if (lastSpectrumData) scheduleSpectrumDraw();
      postPath(`/set_bandwidth?hz=${next}`).catch(() => {});
      showHint(`BW ${formatBwLabel(next)}`, 1200);
    }
    return;
  }

  // Left/Right arrows — retune by current jog step
  if (key === "arrowleft" || key === "arrowright") {
    event.preventDefault();
    jogFreq(key === "arrowright" ? 1 : -1);
    return;
  }

  // Up/Down arrows — shift center (spectrum) frequency
  if (key === "arrowup" || key === "arrowdown") {
    event.preventDefault();
    void shiftSpectrumCenter(key === "arrowup" ? 1 : -1);
    return;
  }

  // M — open mode picker
  if (key === "m") {
    event.preventDefault();
    if (modeEl && !modeEl.disabled) {
      modeEl.focus();
      modeEl.click();
      // Attempt to programmatically open the <select> via showPicker (modern browsers)
      if (typeof modeEl.showPicker === "function") {
        try { modeEl.showPicker(); } catch (_) {}
      }
    }
    return;
  }

  // Z — toggle mono/stereo (WFM)
  if (key === "z") {
    event.preventDefault();
    if (wfmAudioModeEl) {
      const next = wfmAudioModeEl.value === "mono" ? "stereo" : "mono";
      wfmAudioModeEl.value = next;
      saveSetting("wfmAudioMode", next);
      const enabled = next !== "mono";
      postPath(`/set_wfm_stereo?enabled=${enabled ? "true" : "false"}`).catch(() => {});
      showHint(next === "stereo" ? "Stereo" : "Mono", 1200);
    } else {
      showHint("Stereo N/A", 1200);
    }
    return;
  }

  // N — toggle noise blanker
  if (key === "n") {
    event.preventDefault();
    if (sdrNbSupported && sdrNbEnabledEl) {
      sdrNbEnabledEl.checked = !sdrNbEnabledEl.checked;
      submitSdrNbState();
      showHint(sdrNbEnabledEl.checked ? "NB On" : "NB Off", 1200);
    } else {
      showHint("NB N/A", 1200);
    }
    return;
  }

  // Q — toggle squelch (cycle 0 → auto → 0)
  if (key === "q") {
    event.preventDefault();
    if (sdrSquelchSupported && sdrSquelchEl) {
      const current = clampSdrSquelchPercent(Number(sdrSquelchEl.value));
      let nextPct;
      if (current > 0) {
        nextPct = 0; // turn off
      } else {
        // Auto: estimate from noise floor
        let auto = 30;
        const data = lastSpectrumData || window.lastSpectrumData;
        if (data && isBinsArray(data.bins) && data.bins.length > 0) {
          const noiseDb = estimateNoiseFloorDb(data.bins);
          if (noiseDb != null && Number.isFinite(noiseDb)) {
            const thresholdDb = noiseDb + 6;
            const clamped = Math.max(SDR_SQUELCH_MIN_DB, Math.min(SDR_SQUELCH_MAX_DB, thresholdDb));
            auto = clampSdrSquelchPercent(
              ((clamped - SDR_SQUELCH_MIN_DB) / (SDR_SQUELCH_MAX_DB - SDR_SQUELCH_MIN_DB)) * 100,
            );
          }
        }
        nextPct = auto;
      }
      sdrSquelchEl.value = String(nextPct);
      updateSdrSquelchPctLabel();
      saveSetting("sdrSquelchPct", nextPct);
      submitSdrSquelchPercent(nextPct);
      showHint(nextPct > 0 ? `Squelch ${nextPct}%` : "Squelch Off", 1200);
    } else {
      showHint("Squelch N/A", 1200);
    }
    return;
  }

  // Spectrum keyboard navigation
  if (lastSpectrumData && spectrumCanvas) {
    // +/= — zoom in
    if (key === "+" || key === "=") {
      event.preventDefault();
      const cssW = spectrumCanvas.clientWidth || 640;
      spectrumZoomAt(cssW / 2, cssW, lastSpectrumData, 1.25);
      scheduleSpectrumDraw();
      scheduleOverviewDraw();
      return;
    }
    // - — zoom out
    if (key === "-") {
      event.preventDefault();
      const cssW = spectrumCanvas.clientWidth || 640;
      spectrumZoomAt(cssW / 2, cssW, lastSpectrumData, 1 / 1.25);
      scheduleSpectrumDraw();
      scheduleOverviewDraw();
      return;
    }
    // 0 — reset zoom
    if (key === "0") {
      event.preventDefault();
      spectrumZoom = 1;
      spectrumPanFrac = 0.5;
      scheduleSpectrumDraw();
      scheduleOverviewDraw();
      return;
    }
  }
}, { capture: true });

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
function handleSpectrumWheel(e, canvasEl) {
  e.preventDefault();
  if (!lastSpectrumData || !canvasEl) return;
  if (e.ctrlKey) {
    const direction = e.deltaY < 0 ? 1 : -1;
    jogFreq(direction);
    return;
  }
  const rect = canvasEl.getBoundingClientRect();
  const cssX = e.clientX - rect.left;
  const factor = e.deltaY < 0 ? 1.25 : 1 / 1.25;
  spectrumZoomAt(cssX, rect.width, lastSpectrumData, factor);
  scheduleSpectrumDraw();
  scheduleOverviewDraw();
}

function handleSpectrumClick(e, canvasEl) {
  if (_sDragMoved) {
    _sDragMoved = false;
    return;
  }
  if (!lastSpectrumData || !canvasEl) return;
  const rect = canvasEl.getBoundingClientRect();
  const cssX = e.clientX - rect.left;
  const targetHz = spectrumTargetHzAt(cssX, rect.width, lastSpectrumData);
  if (!Number.isFinite(targetHz)) return;
  setRigFrequency(targetHz);
}

if (spectrumCanvas) {
  spectrumCanvas.addEventListener("wheel", (e) => {
    handleSpectrumWheel(e, spectrumCanvas);
  }, { passive: false });
}

// Keep waterfall (overview strip) wheel behavior aligned with waveform/spectrum.
if (overviewCanvas) {
  overviewCanvas.addEventListener("wheel", (e) => {
    handleSpectrumWheel(e, overviewCanvas);
  }, { passive: false });
  overviewCanvas.addEventListener("click", (e) => {
    handleSpectrumClick(e, overviewCanvas);
  });
}

// Full waterfall panel interactions.
if (spectrumWaterfallCanvas) {
  spectrumWaterfallCanvas.addEventListener("wheel", (e) => {
    handleSpectrumWheel(e, spectrumWaterfallCanvas);
  }, { passive: false });
  spectrumWaterfallCanvas.addEventListener("click", (e) => {
    handleSpectrumClick(e, spectrumWaterfallCanvas);
  });
  spectrumWaterfallCanvas.addEventListener("mousedown", (e) => {
    onSpectrumMouseDown(e, spectrumWaterfallCanvas);
  });
}


// ── BW strip edge hit-test (CSS pixels) ──────────────────────────────────────
function getBwEdgeHit(cssX, cssW, range) {
  const bwCenterHz = activeBandwidthCenterHz();
  if (!Number.isFinite(bwCenterHz) || !currentBandwidthHz || !lastSpectrumData) return null;

  const HIT = 8;
  let bestEdge = null;
  let bestDist = Number.POSITIVE_INFINITY;
  for (const spec of visibleBandwidthSpecs(bwCenterHz)) {
    const span = displaySpanForBandwidthSpec(spec);
    const xL = ((span.loHz - range.visLoHz) / range.visSpanHz) * cssW;
    const xR = ((span.hiHz - range.visLoHz) / range.visSpanHz) * cssW;
    if (span.side < 0) {
      const distL = Math.abs(cssX - xL);
      if (distL < HIT && distL < bestDist) {
        bestEdge = "left";
        bestDist = distL;
      }
      continue;
    }
    if (span.side > 0) {
      const distR = Math.abs(cssX - xR);
      if (distR < HIT && distR < bestDist) {
        bestEdge = "right";
        bestDist = distR;
      }
      continue;
    }
    const distL = Math.abs(cssX - xL);
    const distR = Math.abs(cssX - xR);
    if (distL < HIT && distL < bestDist) {
      bestEdge = "left";
      bestDist = distL;
    }
    if (distR < HIT && distR < bestDist) {
      bestEdge = "right";
      bestDist = distR;
    }
  }
  if (bestEdge) return bestEdge;
  return null;
}

// ── Mouse drag to pan / BW resize ─────────────────────────────────────────────
let _sDragStart = null;  // { clientX, panFrac }
let _sDragMoved = false;
let _sDragCanvas = null;

function onSpectrumMouseDown(e, canvasEl) {
  if (!canvasEl || e.button !== 0) return;
  if (lastSpectrumData) {
    const rect = canvasEl.getBoundingClientRect();
    const cssX = e.clientX - rect.left;
    const range = spectrumVisibleRange(lastSpectrumData);
    const edge = getBwEdgeHit(cssX, rect.width, range);
    if (edge) {
      _bwDragEdge = edge;
      _bwDragStartX = cssX;
      _bwDragStartBwHz = currentBandwidthHz;
      _bwDragCanvas = canvasEl;
      _sDragStart = null;
      _sDragCanvas = null;
      _sDragMoved = true; // suppress click-to-tune
      return;
    }
  }
  _sDragStart = { clientX: e.clientX, panFrac: spectrumPanFrac };
  _sDragCanvas = canvasEl;
  _sDragMoved = false;
}

if (spectrumCanvas) {
  spectrumCanvas.addEventListener("mousedown", (e) => { onSpectrumMouseDown(e, spectrumCanvas); });
}
if (overviewCanvas) {
  overviewCanvas.addEventListener("mousedown", (e) => { onSpectrumMouseDown(e, overviewCanvas); });
}

if (spectrumCanvas || overviewCanvas) {
  window.addEventListener("mousemove", (e) => {
    if (_bwDragEdge && lastSpectrumData) {
      const dragCanvas = _bwDragCanvas || spectrumCanvas;
      if (!dragCanvas) return;
      const rect  = dragCanvas.getBoundingClientRect();
      const cssX  = e.clientX - rect.left;
      const range = spectrumVisibleRange(lastSpectrumData);
      const dxHz  = ((cssX - _bwDragStartX) / rect.width) * range.visSpanHz;
      const side = sidebandDirectionForMode(modeEl ? modeEl.value : "USB");
      let newBw;
      if (side === 0) {
        newBw = _bwDragEdge === "right"
          ? _bwDragStartBwHz + dxHz * 2
          : _bwDragStartBwHz - dxHz * 2;
      } else {
        newBw = _bwDragEdge === "right"
          ? _bwDragStartBwHz + dxHz
          : _bwDragStartBwHz - dxHz;
      }
      const [, minBw, maxBw] = mwDefaultsForMode(modeEl ? modeEl.value : "USB");
      newBw = Math.round(Math.max(minBw, Math.min(maxBw, newBw)));
      currentBandwidthHz = newBw;
      window.currentBandwidthHz = currentBandwidthHz;
      syncBandwidthInput(newBw);
      positionFastOverlay(lastFreqHz, newBw);
      scheduleSpectrumDraw();
      scheduleOverviewDraw();
      return;
    }
    if (!_sDragStart || !lastSpectrumData) return;
    const dragCanvas = _sDragCanvas || spectrumCanvas || overviewCanvas;
    if (!dragCanvas) return;
    const rect  = dragCanvas.getBoundingClientRect();
    const dx    = e.clientX - _sDragStart.clientX;
    if (Math.abs(dx) > 3) _sDragMoved = true;
    spectrumPanFrac = _sDragStart.panFrac - (dx / rect.width) / spectrumZoom;
    scheduleSpectrumDraw();
  });

  window.addEventListener("mouseup", async () => {
    if (_bwDragEdge) {
      try {
        const bwHz = Math.round(currentBandwidthHz);
        if (!(typeof vchanInterceptBandwidth === "function" && await vchanInterceptBandwidth(bwHz))) {
          await postPath(`/set_bandwidth?hz=${bwHz}`);
          if (Number.isFinite(lastFreqHz)) {
            await ensureTunedBandwidthCoverage(lastFreqHz, currentBandwidthHz);
          }
        }
      } catch (_) {}
      _bwDragEdge = null;
      _bwDragCanvas = null;
      return;
    }
    _sDragStart = null;
    _sDragCanvas = null;
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
    const bookmark = edge ? null : nearestBookmarkForHz(hz, rect.width, range);
    const peak = edge ? null : nearestSpectrumPeak(cssX, rect.width, lastSpectrumData);
    const peakHz = peak?.hz ?? null;
    const peakDb = peak && Number.isFinite(peak.db) ? `${peak.db.toFixed(1)} dB` : null;
    if (bookmark) {
      spectrumTooltip.textContent = buildBookmarkTooltipText(bookmark);
    } else if (peakHz != null && Math.abs(peakHz - hz) >= Math.max(minFreqStepHz, 10)) {
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
    // Update crosshair position
    spectrumCrosshairX = cssX;
    spectrumCrosshairY = e.clientY - rect.top;
    scheduleSpectrumDraw();
  });
  spectrumCanvas.addEventListener("mouseleave", () => {
    if (spectrumTooltip) spectrumTooltip.style.display = "none";
    spectrumCanvas.style.cursor = "crosshair";
    spectrumCrosshairX = null;
    spectrumCrosshairY = null;
    scheduleSpectrumDraw();
  });
}

// ── Click to tune (only when not dragging) ────────────────────────────────────
if (spectrumCanvas) {
  spectrumCanvas.addEventListener("click", (e) => {
    handleSpectrumClick(e, spectrumCanvas);
  });
}

if (spectrumCenterLeftBtn) {
  spectrumCenterLeftBtn.addEventListener("click", () => {
    shiftSpectrumCenter(-1).catch(() => {});
  });
}
if (spectrumCenterRightBtn) {
  spectrumCenterRightBtn.addEventListener("click", () => {
    shiftSpectrumCenter(1).catch(() => {});
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

  const rangeInput = document.getElementById("spectrum-range-input");
  if (rangeInput) {
    rangeInput.value = spectrumRange;
    rangeInput.addEventListener("change", () => {
      const v = Number(rangeInput.value);
      if (!isNaN(v) && v >= 10) {
        spectrumRange = v;
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
      spectrumRange = Math.max(60, Math.ceil((peak - spectrumFloor) / 10) * 10 + SPECTRUM_HEADROOM_DB);
      if (floorInput) floorInput.value = spectrumFloor;
      if (rangeInput) rangeInput.value = spectrumRange;
      scheduleSpectrumDraw();
    });
  }

  const gammaInput = document.getElementById("spectrum-gamma-input");
  const gammaValue = document.getElementById("spectrum-gamma-value");
  if (gammaInput) {
    gammaInput.addEventListener("input", () => {
      const v = Number(gammaInput.value);
      if (Number.isFinite(v) && v > 0) {
        waterfallGamma = v;
        if (gammaValue) gammaValue.textContent = v.toFixed(1);
        if (lastSpectrumData) scheduleSpectrumDraw();
      }
    });
    gammaInput.addEventListener("dblclick", () => {
      waterfallGamma = 1.0;
      gammaInput.value = "1.0";
      if (gammaValue) gammaValue.textContent = "1.0";
      if (lastSpectrumData) scheduleSpectrumDraw();
    });
  }
})();

// ── Bandplan strip ──────────────────────────────────────────────────────────
let bandplanData = null;
let bandplanRegion = loadSetting("bandplanRegion", "off");
let bandplanShowLabels = loadSetting("bandplanLabels", true);
let _bandplanServerDefaultApplied = false;
let bandplanSegmentsCache = null;
let bandplanCacheKey = "";

const bandplanStripEl = document.getElementById("spectrum-bandplan-strip");
const bandplanRegionSelect = document.getElementById("bandplan-region-select");
const bandplanLabelsCheck = document.getElementById("bandplan-labels-check");

(function loadBandplanJson() {
  fetch("/bandplan.json")
    .then((r) => { if (!r.ok) throw new Error(r.status); return r.json(); })
    .then((d) => { bandplanData = d; bandplanSegmentsCache = null; bandplanCacheKey = ""; })
    .catch(() => {});
})();

if (bandplanRegionSelect) {
  bandplanRegionSelect.value = bandplanRegion;
  bandplanRegionSelect.addEventListener("change", () => {
    bandplanRegion = bandplanRegionSelect.value;
    saveSetting("bandplanRegion", bandplanRegion);
    bandplanSegmentsCache = null;
    bandplanCacheKey = "";
    if (lastSpectrumData) scheduleSpectrumDraw();
  });
}
if (bandplanLabelsCheck) {
  bandplanLabelsCheck.checked = bandplanShowLabels;
  bandplanLabelsCheck.addEventListener("change", () => {
    bandplanShowLabels = bandplanLabelsCheck.checked;
    saveSetting("bandplanLabels", bandplanShowLabels);
    bandplanSegmentsCache = null;
    bandplanCacheKey = "";
    if (lastSpectrumData) scheduleSpectrumDraw();
  });
}

function bandplanComputeRange() {
  // When spectrum data is available (SDR), use the zoomed visible range
  if (lastSpectrumData) {
    return spectrumVisibleRange(lastSpectrumData);
  }
  // For non-SDR rigs, derive a range from the current tuned frequency.
  // Find the band containing the frequency and show that full band.
  const freq = lastFreqHz;
  if (!freq || !Number.isFinite(freq)) return null;

  // Check bandplan data for the current region to find the matching band
  if (bandplanData && bandplanData[bandplanRegion]) {
    const bands = bandplanData[bandplanRegion].bands;
    for (const band of bands) {
      if (freq >= band.low_hz && freq <= band.high_hz) {
        const margin = (band.high_hz - band.low_hz) * 0.05;
        return {
          visLoHz: band.low_hz - margin,
          visHiHz: band.high_hz + margin,
          visSpanHz: (band.high_hz - band.low_hz) + 2 * margin,
        };
      }
    }
  }
  // Fallback: show a 500 kHz window around the frequency
  const span = 500000;
  return { visLoHz: freq - span / 2, visHiHz: freq + span / 2, visSpanHz: span };
}

function bandplanVisibleSegments(region, loHz, hiHz) {
  if (!bandplanData || !bandplanData[region]) return [];
  const bands = bandplanData[region].bands;
  const result = [];
  for (const band of bands) {
    if (band.high_hz < loHz || band.low_hz > hiHz) continue;
    for (const seg of band.segments) {
      if (seg.high_hz <= loHz || seg.low_hz >= hiHz) continue;
      result.push({
        low_hz: seg.low_hz,
        high_hz: seg.high_hz,
        mode: seg.mode,
        label: seg.label,
        band: band.name,
      });
    }
  }
  return result;
}

function _hideBandplanStrip() {
  if (!bandplanStripEl) return;
  bandplanStripEl.classList.remove("bp-visible");
  bandplanStripEl.replaceChildren();
  bandplanCacheKey = "";
}

function updateBandplanStrip(range) {
  if (!bandplanStripEl) return;
  if (!range || bandplanRegion === "off" || !bandplanData) {
    if (bandplanStripEl.classList.contains("bp-visible")) _hideBandplanStrip();
    return;
  }

  const segments = bandplanVisibleSegments(bandplanRegion, range.visLoHz, range.visHiHz);
  if (segments.length === 0) {
    if (bandplanStripEl.classList.contains("bp-visible")) _hideBandplanStrip();
    return;
  }

  bandplanStripEl.classList.add("bp-visible");

  const newKey = bandplanRegion + ":" + (bandplanShowLabels ? "L" : "N") + ":" +
    segments.map((s) => s.low_hz + "-" + s.high_hz).join(",");

  const stripW = bandplanStripEl.clientWidth || 1;

  if (bandplanCacheKey !== newKey) {
    bandplanCacheKey = newKey;
    bandplanStripEl.replaceChildren();

    const seenBands = new Set();
    for (const seg of segments) {
      const el = document.createElement("div");
      el.className = "bp-segment";
      el.dataset.mode = seg.mode;
      el.title = seg.band + " \u2013 " + seg.label + " (" + seg.mode + ")";
      if (bandplanShowLabels) {
        const lbl = document.createElement("span");
        lbl.className = "bp-segment-label";
        lbl.textContent = seg.label;
        el.appendChild(lbl);
      }
      bandplanStripEl.appendChild(el);

      if (!seenBands.has(seg.band)) {
        seenBands.add(seg.band);
        const bandLbl = document.createElement("div");
        bandLbl.className = "bp-band-label";
        bandLbl.textContent = seg.band;
        bandLbl.dataset.bandLow = seg.low_hz;
        bandplanStripEl.appendChild(bandLbl);
      }
    }
    bandplanSegmentsCache = segments;
  }

  const children = bandplanStripEl.querySelectorAll(".bp-segment");
  const bandLabels = bandplanStripEl.querySelectorAll(".bp-band-label");
  const segs = bandplanSegmentsCache || segments;

  segs.forEach((seg, i) => {
    const el = children[i];
    if (!el) return;
    const l = Math.max(0, (seg.low_hz - range.visLoHz) / range.visSpanHz);
    const r = Math.min(1, (seg.high_hz - range.visLoHz) / range.visSpanHz);
    const leftPx = l * stripW;
    const widthPx = Math.max(1, (r - l) * stripW);
    el.style.left = leftPx + "px";
    el.style.width = widthPx + "px";

    const lbl = el.querySelector(".bp-segment-label");
    if (lbl) {
      lbl.style.display = widthPx < 20 ? "none" : "";
    }
  });

  bandLabels.forEach((lbl) => {
    const bandLow = Number(lbl.dataset.bandLow);
    const frac = (bandLow - range.visLoHz) / range.visSpanHz;
    const px = Math.max(2, frac * stripW);
    lbl.style.left = px + "px";
    lbl.style.display = (frac < -0.1 || frac > 1.05) ? "none" : "";
  });
}
