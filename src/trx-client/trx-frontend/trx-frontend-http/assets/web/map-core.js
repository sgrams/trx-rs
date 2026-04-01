// Map, statistics, and geolocation module (lazy-loaded on map tab activation).
// Communicates with app.js core via window.trx namespace.
(function () {
  "use strict";
  const T = window.trx;

  // Destructure shared utility functions for convenience
  const { saveSetting, loadSetting, showHint, formatFreq, formatFreqForHumans,
          postPath, scheduleUiFrameJob, navigateToTab, rigBadgeColor,
          formatUptime, latLonToMaidenhead, locatorToLatLon, haversineKm,
          formatDistanceKm, formatTimeAgo, currentDecodeHistoryRetentionMs,
          formatWavelength, bookmarkDistanceText, buildBookmarkTooltipText,
          nearestBookmarkForHz } = T;

  function updateMapRigFilter() {
    const el = document.getElementById("map-rig-filter");
    if (!el) return;
    const prev = el.value;
    while (el.options.length > 1) el.remove(1);
    for (const id of T.lastRigIds) {
      const opt = document.createElement("option");
      opt.value = id;
      opt.textContent = T.lastRigDisplayNames[id] || id;
      el.appendChild(opt);
    }
    if (prev && T.lastRigIds.includes(prev)) {
      el.value = prev;
    } else {
      el.value = "";
      mapRigFilter = "";
    }
    updateStatsRigFilter();
  }

  // --- Leaflet Map (lazy-initialized) ---
  let aprsMap = null;
  let aprsMapBaseLayer = null;
  let aprsMapReceiverMarker = null;
  let aprsMapReceiverMarkers = {}; // keyed by rig remote id
  let aprsRadioPaths = [];
  let selectedLocatorMarker = null;
  let selectedLocatorPulseRaf = null;
  let mapFullscreenListenerBound = false;
  let mapP2pRadioPathsEnabled = loadSetting("mapP2pRadioPathsEnabled", true) !== false;
  let mapDecodeContactPathsEnabled = loadSetting("mapDecodeContactPathsEnabled", true) !== false;
  let mapOverlayPanelVisible = loadSetting("mapOverlayPanelVisible", true) !== false;
  const MAP_HISTORY_LIMIT_OPTIONS = [15, 30, 60, 180, 360, 720, 1440];
  const MAP_QSO_SUMMARY_LIMIT = 5;
  const stationMarkers = new Map();
  const locatorMarkers = new Map();
  const decodeContactPaths = new Map();
  let selectedMapQsoKey = null;
  const mapMarkers = new Set();
  const DEFAULT_MAP_SOURCE_FILTER = { ais: true, vdes: true, aprs: true, bookmark: false, ft8: true, ft4: true, ft2: true, wspr: true, sat: false };
  const mapFilter = { ...DEFAULT_MAP_SOURCE_FILTER };
  const mapLocatorFilter = { phase: "band", bands: new Set() };
  let mapSearchFilter = "";
  let mapRigFilter = ""; // "" = all rigs
  let mapHistoryPruneTimer = null;
  let mapHistoryLimitMinutes = normalizeMapHistoryLimitMinutes(
    Number(loadSetting("mapHistoryLimitMinutes", 1440))
  );
  const APRS_TRACK_MAX_POINTS = 64;
  const AIS_TRACK_MAX_POINTS = 64;
  const aisMarkers = new Map();
  const vdesMarkers = new Map();
  let selectedAprsTrackCall = null;
  let selectedAisTrackMmsi = null;
  const HAM_BANDS = [
    { label: "2200m", meters: 2200 },
    { label: "630m", meters: 630 },
    { label: "160m", meters: 160 },
    { label: "80m", meters: 80 },
    { label: "60m", meters: 60 },
    { label: "40m", meters: 40 },
    { label: "30m", meters: 30 },
    { label: "20m", meters: 20 },
    { label: "17m", meters: 17 },
    { label: "15m", meters: 15 },
    { label: "12m", meters: 12 },
    { label: "10m", meters: 10 },
    { label: "6m", meters: 6 },
    { label: "4m", meters: 4 },
    { label: "3m", meters: 3 },
    { label: "2m", meters: 2 },
    { label: "1m", meters: 1 },
    { label: "70cm", meters: 0.7 },
    { label: "23cm", meters: 0.23 },
    { label: "13cm", meters: 0.13 },
    { label: "9cm", meters: 0.09 },
    { label: "6cm", meters: 0.06 },
    { label: "3cm", meters: 0.03 },
  ].map((band) => ({
    ...band,
    nominalHz: 299_792_458 / band.meters,
  }));

  function normalizeLocatorFreqHz(hz) {
    if (!Number.isFinite(hz) || hz <= 0) return null;
    if (hz >= 100_000) return hz;
    const baseHz = Number(window.ft8BaseHz);
    if (Number.isFinite(baseHz) && baseHz > 0) {
      return baseHz + hz;
    }
    return hz;
  }

  function normalizeMapHistoryLimitMinutes(value) {
    const minutes = Math.round(Number(value));
    return MAP_HISTORY_LIMIT_OPTIONS.includes(minutes) ? minutes : 1440;
  }

  function mapHistoryCutoffMs() {
    return Date.now() - (mapHistoryLimitMinutes * 60 * 1000);
  }

  function trimTrackHistory(history, cutoffMs, maxPoints) {
    const list = Array.isArray(history) ? history : [];
    const trimmed = list.filter((point) => Number(point?.tsMs) >= cutoffMs);
    if (trimmed.length > maxPoints) {
      trimmed.splice(0, trimmed.length - maxPoints);
    }
    return trimmed;
  }

  function refreshAprsTrack(call, entry) {
    if (!entry) return;
    if (!Array.isArray(entry.trackPoints) || entry.trackPoints.length < 2) {
      if (entry.track) {
        entry.track.remove();
        entry.track = null;
      }
      return;
    }
    if (entry.track) {
      entry.track.setLatLngs(entry.trackPoints);
      return;
    }
    const track = L.polyline(entry.trackPoints, {
      color: "#f0be4d",
      weight: 2,
      opacity: 0.72,
      lineCap: "round",
      lineJoin: "round",
      interactive: false,
    });
    track.__trxType = "aprs";
    track._aprsCall = call;
    entry.track = track;
  }

  function refreshAisTrack(mmsi, entry) {
    if (!entry) return;
    if (!Array.isArray(entry.trackPoints) || entry.trackPoints.length < 2) {
      if (entry.track) {
        entry.track.remove();
        entry.track = null;
      }
      return;
    }
    if (entry.track) {
      entry.track.setLatLngs(entry.trackPoints);
      return;
    }
    const track = L.polyline(entry.trackPoints, {
      color: getAisAccentColor(),
      weight: 2,
      opacity: 0.68,
      lineCap: "round",
      lineJoin: "round",
      interactive: false,
      dashArray: "5 4",
    });
    track.__trxType = "ais";
    track._aisMmsi = mmsi;
    entry.track = track;
  }

  function removeMapMarker(marker) {
    if (!marker) return;
    if (marker === selectedLocatorMarker) {
      setSelectedLocatorMarker(null);
      clearMapRadioPath();
    }
    if (aprsMap && aprsMap.hasLayer(marker)) marker.removeFrom(aprsMap);
    mapMarkers.delete(marker);
  }

  function setRetainedMapMarkerVisible(marker, visible) {
    if (!marker) return;
    marker.__trxHistoryVisible = visible !== false;
    if (!visible) {
      if (marker === selectedLocatorMarker) {
        setSelectedLocatorMarker(null);
        clearMapRadioPath();
      }
      if (aprsMap && aprsMap.hasLayer(marker)) marker.removeFrom(aprsMap);
    }
  }

  function ensureAprsMarker(call, entry) {
    if (!aprsMap || !entry || entry.marker || entry.lat == null || entry.lon == null) return;
    _aprsAddMarkerToMap(call, entry);
  }

  function ensureAisMarker(key, entry) {
    if (!aprsMap || !entry || entry.marker || entry?.msg?.lat == null || entry?.msg?.lon == null) return;
    const marker = createAisMarker(entry.msg.lat, entry.msg.lon, entry.msg)
      .addTo(aprsMap)
      .bindPopup(buildAisPopupHtml(entry.msg));
    marker.__trxType = "ais";
    marker.__trxRigIds = entry.rigIds || new Set();
    marker._aisMmsi = String(key);
    entry.marker = marker;
    mapMarkers.add(marker);
  }

  function ensureVdesMarker(key, entry) {
    if (!aprsMap || !entry || entry.marker || entry?.msg?.lat == null || entry?.msg?.lon == null) return;
    const marker = L.circleMarker([entry.msg.lat, entry.msg.lon], {
      radius: 5,
      color: "#5c394f",
      fillColor: "#c46392",
      fillOpacity: 0.82,
    }).addTo(aprsMap).bindPopup(buildVdesPopupHtml(entry.msg));
    marker.__trxType = "vdes";
    marker.__trxRigIds = entry.rigIds || new Set();
    marker._vdesKey = String(key);
    entry.marker = marker;
    mapMarkers.add(marker);
  }

  function ensureDecodeLocatorMarker(entry) {
    if (!aprsMap || !entry || entry.marker || !entry.grid || (entry.sourceType !== "ft8" && entry.sourceType !== "ft4" && entry.sourceType !== "ft2" && entry.sourceType !== "wspr")) return;
    const bounds = maidenheadToBounds(entry.grid);
    if (!bounds) return;
    const count = Math.max(entry.stationDetails?.size || 0, entry.stations?.size || 0, 1);
    const tooltipHtml = buildDecodeLocatorTooltipHtml(entry.grid, entry, entry.sourceType);
    const marker = L.rectangle(bounds, locatorStyleForEntry(entry, count))
      .addTo(aprsMap)
      .bindPopup(tooltipHtml);
    marker.__trxType = entry.sourceType;
    marker.__trxRigIds = entry.rigIds || new Set();
    sendLocatorOverlayToBack(marker);
    assignLocatorMarkerMeta(marker, entry.sourceType, entry.bandMeta);
    entry.marker = marker;
    mapMarkers.add(marker);
  }

  function pruneAprsEntry(call, entry, cutoffMs) {
    const canRenderMap = !!aprsMap && !T.decodeHistoryReplayActive;
    const pktTsMs = Number(entry?.pkt?._tsMs);
    const visible = Number.isFinite(pktTsMs) && pktTsMs >= cutoffMs;
    entry.visibleInHistoryWindow = visible;
    entry.trackPoints = trimTrackHistory(entry.trackHistory, cutoffMs, APRS_TRACK_MAX_POINTS)
      .map((point) => [point.lat, point.lon]);
    if (canRenderMap) {
      refreshAprsTrack(call, entry);
    } else {
      T.markDecodeMapSyncPending();
    }
    if (!visible) {
      if (canRenderMap && selectedAprsTrackCall && String(selectedAprsTrackCall) === String(call)) {
        selectedAprsTrackCall = null;
      }
      if (canRenderMap && entry?.track) {
        entry.track.remove();
        entry.track = null;
      }
      if (canRenderMap) setRetainedMapMarkerVisible(entry?.marker, false);
      return false;
    }
    if (!canRenderMap) return true;
    ensureAprsMarker(call, entry);
    setRetainedMapMarkerVisible(entry?.marker, true);
    if (entry?.marker) {
      entry.marker.setLatLng([entry.lat, entry.lon]);
      entry.marker.setPopupContent(buildAprsPopupHtml(call, entry.lat, entry.lon, entry.info || "", entry.pkt));
    }
    return true;
  }

  function pruneAisEntry(key, entry, cutoffMs) {
    const canRenderMap = !!aprsMap && !T.decodeHistoryReplayActive;
    const msgTsMs = Number(entry?.msg?._tsMs);
    const visible = Number.isFinite(msgTsMs) && msgTsMs >= cutoffMs;
    entry.visibleInHistoryWindow = visible;
    entry.trackPoints = trimTrackHistory(entry.trackHistory, cutoffMs, AIS_TRACK_MAX_POINTS)
      .map((point) => [point.lat, point.lon]);
    if (canRenderMap) {
      refreshAisTrack(key, entry);
    } else {
      T.markDecodeMapSyncPending();
    }
    if (!visible) {
      if (canRenderMap && selectedAisTrackMmsi && String(selectedAisTrackMmsi) === String(key)) {
        selectedAisTrackMmsi = null;
      }
      if (canRenderMap && entry?.track) {
        entry.track.remove();
        entry.track = null;
      }
      if (canRenderMap) setRetainedMapMarkerVisible(entry?.marker, false);
      return false;
    }
    if (!canRenderMap) return true;
    ensureAisMarker(key, entry);
    setRetainedMapMarkerVisible(entry?.marker, true);
    if (entry?.marker) {
      updateAisMarker(entry.marker, entry.msg, buildAisPopupHtml(entry.msg));
    }
    return true;
  }

  function pruneLocatorEntry(key, entry, cutoffMs) {
    const canRenderMap = !!aprsMap && !T.decodeHistoryReplayActive;
    if (!entry || (entry.sourceType !== "ft8" && entry.sourceType !== "ft4" && entry.sourceType !== "ft2" && entry.sourceType !== "wspr")) return true;
    if (!(entry.allStationDetails instanceof Map)) {
      entry.allStationDetails = entry.stationDetails instanceof Map
        ? new Map(entry.stationDetails)
        : new Map();
    }
    const nextDetails = new Map();
    for (const [detailKey, detail] of entry.allStationDetails.entries()) {
      const tsMs = Number(detail?.ts_ms);
      if (Number.isFinite(tsMs) && tsMs >= cutoffMs) {
        nextDetails.set(detailKey, detail);
      }
    }
    entry.visibleInHistoryWindow = nextDetails.size > 0;
    if (nextDetails.size === 0) {
      entry.stationDetails = new Map();
      entry.stations = new Set();
      entry.bandMeta = new Map();
      if (canRenderMap) setRetainedMapMarkerVisible(entry.marker, false);
      else T.markDecodeMapSyncPending();
      return false;
    }
    const nextStations = new Set();
    for (const detail of nextDetails.values()) {
      const source = String(detail?.source || detail?.station || "").trim().toUpperCase();
      if (source) nextStations.add(source);
    }
    entry.stationDetails = nextDetails;
    entry.stations = nextStations;
    entry.bandMeta = collectBandMeta(
      Array.from(nextDetails.values()).map((detail) => Number(detail?.freq_hz))
    );
    const count = Math.max(nextDetails.size, nextStations.size || 0, 1);
    if (!canRenderMap) {
      T.markDecodeMapSyncPending();
      return true;
    }
    ensureDecodeLocatorMarker(entry);
    setRetainedMapMarkerVisible(entry.marker, true);
    if (entry.marker) {
      entry.marker.setStyle(locatorStyleForEntry(entry, count));
      entry.marker.setPopupContent(buildDecodeLocatorTooltipHtml(entry.grid, entry, entry.sourceType));
      assignLocatorMarkerMeta(entry.marker, entry.sourceType, entry.bandMeta);
    }
    return true;
  }

  function pruneMapHistory() {
    const cutoffMs = mapHistoryCutoffMs();
    for (const [call, entry] of stationMarkers.entries()) {
      pruneAprsEntry(call, entry, cutoffMs);
    }
    for (const [key, entry] of aisMarkers.entries()) {
      pruneAisEntry(key, entry, cutoffMs);
    }
    for (const [key, entry] of vdesMarkers.entries()) {
      const tsMs = Number(entry?.msg?._tsMs);
      const visible = Number.isFinite(tsMs) && tsMs >= cutoffMs;
      entry.visibleInHistoryWindow = visible;
      if (!visible) {
        setRetainedMapMarkerVisible(entry?.marker, false);
        continue;
      }
      ensureVdesMarker(key, entry);
      setRetainedMapMarkerVisible(entry?.marker, true);
      if (entry?.marker) {
        entry.marker.setLatLng([entry.msg.lat, entry.msg.lon]);
        entry.marker.setPopupContent(buildVdesPopupHtml(entry.msg));
      }
    }
    for (const [key, entry] of locatorMarkers.entries()) {
      pruneLocatorEntry(key, entry, cutoffMs);
    }
    if (!aprsMap || T.decodeHistoryReplayActive) {
      T.markDecodeMapSyncPending();
      return;
    }
    rebuildDecodeContactPaths();
    rebuildMapLocatorFilters();
    applyMapFilter();
  }

  function locatorSourceLabel(type) {
    if (type === "bookmark") return "Bookmarks";
    if (type === "wspr") return "WSPR";
    if (type === "ft4") return "FT4";
    if (type === "ft2") return "FT2";
    return "FT8";
  }

  function mapSourceLabel(type) {
    if (type === "bookmark") return "Bookmarks";
    return String(type || "").toUpperCase();
  }

  function locatorFilterColor(type) {
    const hues = locatorThemeHues();
    const lightTheme = T.currentTheme() === "light";
    const sat = lightTheme ? 66 : 76;
    const light = lightTheme ? 42 : 56;
    const hue = type === "bookmark"
      ? hues.bookmark
      : (type === "wspr" ? hues.wspr : (type === "ft4" ? hues.ft4 : (type === "ft2" ? hues.ft2 : hues.ft8)));
    return `hsl(${hue.toFixed(1)} ${sat}% ${light}%)`;
  }

  function mapSourceColor(type) {
    if (type === "ais") return "#38bdf8";
    if (type === "vdes") return "#a78bfa";
    if (type === "sat") return "#f59e0b";
    if (type === "aprs") return "#00d17f";
    return locatorFilterColor(type);
  }

  function bandForHz(hz) {
    const rfHz = normalizeLocatorFreqHz(hz);
    if (!Number.isFinite(rfHz) || rfHz <= 0) return null;
    let bestBand = null;
    let bestDistance = Infinity;
    for (const band of HAM_BANDS) {
      const distance = Math.abs(Math.log(rfHz / band.nominalHz));
      if (distance < bestDistance) {
        bestDistance = distance;
        bestBand = band;
      }
    }
    return bestBand;
  }

  function collectBandMeta(freqs) {
    const out = new Map();
    if (!Array.isArray(freqs)) return out;
    for (const hz of freqs) {
      const band = bandForHz(hz);
      if (band && !out.has(band.label)) out.set(band.label, band.nominalHz);
    }
    return out;
  }

  function assignLocatorMarkerMeta(marker, sourceType, bandMeta) {
    if (!marker) return;
    const safeMeta = bandMeta instanceof Map ? bandMeta : new Map();
    marker._locatorFilterMeta = {
      sourceType,
      bands: new Set(safeMeta.keys()),
      bandMeta: new Map(safeMeta),
    };
  }

  function parseMapColor(input) {
    const value = String(input || "").trim();
    if (!value) return null;
    const hex = value.match(/^#([0-9a-f]{3,8})$/i);
    if (hex) {
      const raw = hex[1];
      if (raw.length === 3 || raw.length === 4) {
        const chars = raw.split("");
        return {
          r: parseInt(chars[0] + chars[0], 16),
          g: parseInt(chars[1] + chars[1], 16),
          b: parseInt(chars[2] + chars[2], 16),
        };
      }
      if (raw.length === 6 || raw.length === 8) {
        return {
          r: parseInt(raw.slice(0, 2), 16),
          g: parseInt(raw.slice(2, 4), 16),
          b: parseInt(raw.slice(4, 6), 16),
        };
      }
    }
    const rgb = value.match(/^rgba?\(\s*([0-9.]+)\s*,\s*([0-9.]+)\s*,\s*([0-9.]+)/i);
    if (rgb) {
      return {
        r: Math.max(0, Math.min(255, Number(rgb[1]))),
        g: Math.max(0, Math.min(255, Number(rgb[2]))),
        b: Math.max(0, Math.min(255, Number(rgb[3]))),
      };
    }
    return null;
  }

  function rgbToHsl(rgb) {
    if (!rgb) return null;
    const r = rgb.r / 255;
    const g = rgb.g / 255;
    const b = rgb.b / 255;
    const max = Math.max(r, g, b);
    const min = Math.min(r, g, b);
    const l = (max + min) / 2;
    if (max === min) {
      return { h: 0, s: 0, l: l * 100 };
    }
    const d = max - min;
    const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    let h;
    switch (max) {
      case r:
        h = ((g - b) / d) + (g < b ? 6 : 0);
        break;
      case g:
        h = ((b - r) / d) + 2;
        break;
      default:
        h = ((r - g) / d) + 4;
        break;
    }
    return { h: (h * 60) % 360, s: s * 100, l: l * 100 };
  }

  function wrapHue(hue) {
    const value = Number(hue) || 0;
    return ((value % 360) + 360) % 360;
  }

  function paletteHue(input, fallback) {
    const hsl = rgbToHsl(parseMapColor(input));
    return Number.isFinite(hsl?.h) ? hsl.h : fallback;
  }

  function locatorThemeHues() {
    const pal = T.canvasPalette();
    const baseHue = paletteHue(pal?.spectrumLine, 145);
    const waveHue = paletteHue(pal?.waveformLine, baseHue + 34);
    const peakHue = paletteHue(pal?.waveformPeak, baseHue - 42);
    return {
      bookmark: wrapHue(baseHue),
      ft8: wrapHue(peakHue),
      ft4: wrapHue(peakHue + 30),
      ft2: wrapHue(peakHue + 60),
      wspr: wrapHue((waveHue + baseHue) / 2),
      bandBase: wrapHue((baseHue * 0.65) + (peakHue * 0.35)),
    };
  }

  function locatorBandIndex(label) {
    const idx = HAM_BANDS.findIndex((band) => band.label === label);
    return idx >= 0 ? idx : 0;
  }

  function locatorBandChipColor(label) {
    const hues = locatorThemeHues();
    const lightTheme = T.currentTheme() === "light";
    const hue = wrapHue(hues.bandBase + locatorBandIndex(label) * 137.508);
    const sat = lightTheme ? 68 : 78;
    const light = lightTheme ? 44 : 58;
    return `hsl(${hue.toFixed(1)} ${sat}% ${light}%)`;
  }

  function locatorBandLabelForEntry(entry) {
    const meta = entry?.bandMeta instanceof Map ? entry.bandMeta : new Map();
    if (meta.size === 0) return null;
    if (mapLocatorFilter.phase === "band" && mapLocatorFilter.bands.size > 0) {
      for (const label of mapLocatorFilter.bands) {
        if (meta.has(label)) return label;
      }
    }
    let bestLabel = null;
    let bestHz = -Infinity;
    for (const [label, hz] of meta.entries()) {
      const value = Number.isFinite(hz) ? Number(hz) : 0;
      if (value > bestHz) {
        bestHz = value;
        bestLabel = label;
      }
    }
    return bestLabel;
  }

  function locatorHueForEntry(entry) {
    const hues = locatorThemeHues();
    if (mapLocatorFilter.phase === "band") {
      const label = locatorBandLabelForEntry(entry);
      if (label) {
        return wrapHue(hues.bandBase + locatorBandIndex(label) * 137.508);
      }
    }
    if (entry?.sourceType === "bookmark") return hues.bookmark;
    if (entry?.sourceType === "wspr") return hues.wspr;
    if (entry?.sourceType === "ft4") return hues.ft4;
    if (entry?.sourceType === "ft2") return hues.ft2;
    return hues.ft8;
  }

  function locatorStyleForEntry(entry, count) {
    const safeCount = Math.max(1, Number.isFinite(count) ? count : 1);
    const intensity = Math.min(1, Math.log2(safeCount + 1) / 5);
    const hue = locatorHueForEntry(entry);
    const lightTheme = T.currentTheme() === "light";
    const strokeSat = lightTheme ? 62 : 74;
    const fillSat = lightTheme ? 68 : 78;
    const strokeLight = lightTheme ? 40 : 56;
    const fillLight = lightTheme ? 60 : 42;
    return {
      color: `hsl(${hue.toFixed(1)} ${Math.min(92, strokeSat + intensity * 10).toFixed(1)}% ${Math.max(24, strokeLight - intensity * 4).toFixed(1)}%)`,
      opacity: 0.42 + intensity * 0.5,
      weight: 1 + intensity * 1.2,
      fillColor: `hsl(${hue.toFixed(1)} ${Math.min(96, fillSat + intensity * 8).toFixed(1)}% ${Math.max(20, fillLight - intensity * 5).toFixed(1)}%)`,
      fillOpacity: 0.16 + intensity * 0.34,
    };
  }

  function locatorEntryCount(entry) {
    if (Array.isArray(entry?.bookmarks)) return Math.max(entry.bookmarks.length, 1);
    if (entry?.stationDetails instanceof Map) return Math.max(entry.stationDetails.size, 1);
    if (entry?.stations instanceof Set) return Math.max(entry.stations.size, 1);
    return 1;
  }

  function locatorEntryForMarker(marker) {
    if (!marker) return null;
    for (const entry of locatorMarkers.values()) {
      if (entry?.marker === marker) return entry;
    }
    return null;
  }

  function syncLocatorMarkerStyles() {
    for (const entry of locatorMarkers.values()) {
      if (!entry?.marker) continue;
      entry.marker.setStyle(locatorStyleForEntry(entry, locatorEntryCount(entry)));
    }
    for (const entry of decodeContactPaths.values()) {
      if (!entry?.line) continue;
      const color = decodeContactPathColor(entry);
      entry.line.setStyle({ color, opacity: 0.78 });
    }
  }

  function stopSelectedLocatorPulse() {
    if (selectedLocatorPulseRaf != null) {
      cancelAnimationFrame(selectedLocatorPulseRaf);
      selectedLocatorPulseRaf = null;
    }
  }

  function startSelectedLocatorPulse(marker) {
    stopSelectedLocatorPulse();
    if (!marker || !aprsMap || !aprsMap.hasLayer(marker)) return;
    const tick = (ts) => {
      if (!selectedLocatorMarker || selectedLocatorMarker !== marker || !aprsMap || !aprsMap.hasLayer(marker)) {
        return;
      }
      const entry = locatorEntryForMarker(marker);
      const base = locatorStyleForEntry(entry, locatorEntryCount(entry));
      const phase = (ts % 1600) / 1600;
      const wave = (Math.sin(phase * Math.PI * 2 - Math.PI / 2) + 1) / 2;
      marker.setStyle({
        ...base,
        opacity: Math.min(1, (base.opacity || 0.8) + 0.12 * wave),
        weight: (base.weight || 1.8) + 1.8 * wave,
      });
      selectedLocatorPulseRaf = requestAnimationFrame(tick);
    };
    selectedLocatorPulseRaf = requestAnimationFrame(tick);
  }

  function clearMapRadioPath() {
    for (const p of aprsRadioPaths) p.remove();
    aprsRadioPaths = [];
  }

  function clearDecodeContactPathRender(entry) {
    if (!entry) return;
    if (entry.line) {
      entry.line.remove();
      entry.line = null;
    }
    if (entry.labelMarker) {
      entry.labelMarker.remove();
      entry.labelMarker = null;
    }
  }

  function clearDecodeContactPaths() {
    for (const entry of decodeContactPaths.values()) {
      clearDecodeContactPathRender(entry);
    }
    decodeContactPaths.clear();
    updateMapPathsAnimationClass();
  }

  const MAP_PATHS_STATIC_THRESHOLD = 20;
  function updateMapPathsAnimationClass() {
    const mapEl = document.getElementById("aprs-map");
    if (!mapEl) return;
    mapEl.classList.toggle("map-paths-static", decodeContactPaths.size > MAP_PATHS_STATIC_THRESHOLD);
  }

  function formatDecodeContactDistance(distanceKm) {
    const text = formatDistanceKm(distanceKm);
    return text || "--";
  }

  function decodeLocatorPathVisibility(grid) {
    const normalizedGrid = String(grid || "").trim().toUpperCase();
    if (!normalizedGrid || !aprsMap) return false;
    for (const entry of locatorMarkers.values()) {
      if (!entry || entry.grid !== normalizedGrid) continue;
      if (entry.sourceType !== "ft8" && entry.sourceType !== "wspr") continue;
      if (entry.marker && aprsMap.hasLayer(entry.marker)) return true;
    }
    return false;
  }

  function midpointLatLon(a, b) {
    if (!a || !b) return null;
    if (!Number.isFinite(a.lat) || !Number.isFinite(a.lon) || !Number.isFinite(b.lat) || !Number.isFinite(b.lon)) {
      return null;
    }
    return {
      lat: (a.lat + b.lat) / 2,
      lon: (a.lon + b.lon) / 2,
    };
  }

  function decodeContactPathColor(entry) {
    if (entry?.bandLabel) return locatorBandChipColor(entry.bandLabel);
    const srcEntry = locatorMarkers.get(entry?.sourceGrid);
    if (srcEntry) {
      const label = locatorBandLabelForEntry(srcEntry);
      if (label) return locatorBandChipColor(label);
      return locatorStyleForEntry(srcEntry, locatorEntryCount(srcEntry)).color;
    }
    return locatorFilterColor("ft8");
  }

  function ensureDecodeContactPathRendered(entry) {
    if (!entry || !aprsMap) return;
    const linePoints = [
      [entry.from.lat, entry.from.lon],
      [entry.to.lat, entry.to.lon],
    ];
    const color = decodeContactPathColor(entry);
    if (!entry.line) {
      entry.line = L.polyline(linePoints, {
        color,
        opacity: 0.78,
        className: "decode-contact-path",
        weight: 2.8,
        interactive: false,
      }).addTo(aprsMap);
    } else {
      entry.line.setLatLngs(linePoints);
      entry.line.setStyle({ color, opacity: 0.78 });
      if (!aprsMap.hasLayer(entry.line)) entry.line.addTo(aprsMap);
    }
    const mid = midpointLatLon(entry.from, entry.to);
    if (!mid) return;
    const title = `${entry.source} ↔ ${entry.target} · ${entry.distanceText}`;
    const icon = L.divIcon({
      className: "decode-contact-distance-label",
      html: `<span class="decode-contact-distance-pill" title="${escapeMapHtml(title)}">${escapeMapHtml(entry.distanceText)}</span>`,
    });
    if (!entry.labelMarker) {
      entry.labelMarker = L.marker([mid.lat, mid.lon], {
        icon,
        interactive: false,
        keyboard: false,
        zIndexOffset: 900,
      }).addTo(aprsMap);
    } else {
      entry.labelMarker.setLatLng([mid.lat, mid.lon]);
      entry.labelMarker.setIcon(icon);
      if (!aprsMap.hasLayer(entry.labelMarker)) entry.labelMarker.addTo(aprsMap);
    }
    if (typeof entry.line.bringToBack === "function") entry.line.bringToBack();
  }

  function decodeContactPathMatchesCurrentMap(entry) {
    return decodeLocatorPathVisibility(entry.sourceGrid)
      && decodeLocatorPathVisibility(entry.targetGrid);
  }

  function decodeContactPathRenderVisible(entry) {
    return mapDecodeContactPathsEnabled
      && decodeContactPathMatchesCurrentMap(entry);
  }

  function syncDecodeContactPathVisibility() {
    if (selectedMapQsoKey) {
      const selectedEntry = decodeContactPaths.get(selectedMapQsoKey);
      if (!selectedEntry || !decodeContactPathMatchesCurrentMap(selectedEntry)) {
        selectedMapQsoKey = null;
      }
    }
    for (const entry of decodeContactPaths.values()) {
      const visible = decodeContactPathRenderVisible(entry)
        && (!selectedMapQsoKey || entry.pathKey === selectedMapQsoKey);
      if (!visible) {
        clearDecodeContactPathRender(entry);
        continue;
      }
      ensureDecodeContactPathRendered(entry);
    }
    scheduleStatsRender();
    updateMapPathsAnimationClass();
  }

  function _resolveReceiverLocations(rigIds) {
    // Return all unique receiver locations for the given rig(s)
    const seen = new Set();
    const locations = [];
    if (rigIds && rigIds.size) {
      for (const rid of rigIds) {
        const rig = T.serverRigs.find(r => r.remote === rid);
        if (rig && rig.latitude != null && rig.longitude != null) {
          const key = _receiverLocationKey(rig.latitude, rig.longitude);
          if (!seen.has(key)) {
            seen.add(key);
            locations.push([rig.latitude, rig.longitude]);
          }
        }
      }
    }
    // Fall back to active rig location if no specific locations found
    if (locations.length === 0 && T.serverLat != null && T.serverLon != null) {
      locations.push([T.serverLat, T.serverLon]);
    }
    return locations;
  }

  function setMapRadioPathTo(lat, lon, color, className = "aprs-radio-path", rigIds) {
    clearMapRadioPath();
    if (!mapP2pRadioPathsEnabled || !Number.isFinite(lat) || !Number.isFinite(lon) || !aprsMap) {
      return;
    }
    const sources = _resolveReceiverLocations(rigIds);
    for (const src of sources) {
      aprsRadioPaths.push(
        L.polyline(
          [src, [lat, lon]],
          { color, opacity: 0.85, weight: 2, interactive: false, className }
        ).addTo(aprsMap)
      );
    }
  }

  function locatorMarkerCenter(marker) {
    if (!marker) return null;
    if (typeof marker.getBounds === "function") {
      const bounds = marker.getBounds();
      if (bounds && typeof bounds.getCenter === "function") {
        const center = bounds.getCenter();
        if (Number.isFinite(center?.lat) && Number.isFinite(center?.lng)) {
          return { lat: center.lat, lon: center.lng };
        }
      }
    }
    if (typeof marker.getLatLng === "function") {
      const ll = marker.getLatLng();
      if (Number.isFinite(ll?.lat) && Number.isFinite(ll?.lng)) {
        return { lat: ll.lat, lon: ll.lng };
      }
    }
    return null;
  }

  function setLocatorMarkerHighlight(marker, enabled) {
    const element = typeof marker?.getElement === "function" ? marker.getElement() : marker?._path;
    if (!element) return;
    element.classList.toggle("trx-locator-selected", !!enabled);
  }

  function setSelectedLocatorMarker(marker) {
    if (selectedLocatorMarker && selectedLocatorMarker !== marker) {
      setLocatorMarkerHighlight(selectedLocatorMarker, false);
      const prevEntry = locatorEntryForMarker(selectedLocatorMarker);
      if (prevEntry?.marker) {
        prevEntry.marker.setStyle(locatorStyleForEntry(prevEntry, locatorEntryCount(prevEntry)));
      }
    }
    stopSelectedLocatorPulse();
    selectedLocatorMarker = marker || null;
    if (selectedLocatorMarker) {
      setLocatorMarkerHighlight(selectedLocatorMarker, true);
      startSelectedLocatorPulse(selectedLocatorMarker);
    }
  }

  function isLocatorOverlay(marker) {
    const type = marker?.__trxType;
    return type === "bookmark" || type === "ft8" || type === "ft4" || type === "ft2" || type === "wspr";
  }

  function sendLocatorOverlayToBack(marker) {
    if (!isLocatorOverlay(marker) || typeof marker?.bringToBack !== "function") return;
    marker.bringToBack();
  }

  function renderMapLocatorChipRow(container, items, selectedSet, kind) {
    if (!container) return;
    container.replaceChildren();
    if (!Array.isArray(items) || items.length === 0) {
      container.innerHTML = `<span class="map-locator-empty">No ${kind === "band" ? "bands" : "sources"} available</span>`;
      return;
    }
    let helperText = "";
    const sourceKeys = kind === "source" ? Object.keys(DEFAULT_MAP_SOURCE_FILTER) : [];
    const noneSelected = kind === "source" && sourceKeys.every((k) => !mapFilter[k]);
    if (kind === "source") {
      if (noneSelected) {
        helperText = "All sources visible \u2014 click to filter";
      }
    } else if (!(selectedSet instanceof Set) || selectedSet.size === 0) {
      helperText = `All ${kind === "band" ? "bands" : "sources"} visible by default`;
    }
    for (const item of items) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "map-locator-chip";
      const isActive = kind === "source" ? !!mapFilter[item.key] : selectedSet.has(item.key);
      if (kind === "source" && noneSelected) {
        btn.classList.add("is-default");
      } else if (!isActive) {
        btn.classList.add("is-inactive");
      }
      btn.dataset.filterKind = kind;
      btn.dataset.filterKey = item.key;
      btn.style.setProperty("--chip-color", item.color);
      btn.innerHTML = `<span class="map-locator-chip-text">${escapeMapHtml(item.label)}</span>`;
      container.appendChild(btn);
    }
    if (helperText) {
      const hint = document.createElement("span");
      hint.className = "map-locator-empty";
      hint.textContent = helperText;
      container.appendChild(hint);
    }
  }

  function renderMapLocatorPhaseRow(container, phase) {
    if (!container) return;
    container.replaceChildren();
    const phases = [
      { key: "type", label: "Source" },
      { key: "band", label: "Band" },
    ];
    for (const item of phases) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "map-locator-phase-btn";
      if (phase === item.key) btn.classList.add("is-active");
      btn.dataset.phase = item.key;
      btn.textContent = item.label;
      container.appendChild(btn);
    }
  }

  function renderMapLocatorLegend(phase, sourceItems, bandItems) {
    const legendEl = document.getElementById("map-band-legend");
    if (!legendEl) return;
    const isSourcePhase = phase === "type";
    const items = Array.isArray(isSourcePhase ? sourceItems : bandItems)
      ? (isSourcePhase ? sourceItems : bandItems)
      : [];
    if (items.length === 0) {
      legendEl.classList.add("is-empty");
      legendEl.replaceChildren();
      return;
    }
    legendEl.classList.remove("is-empty");
    const rows = items
      .map((item) => {
        const label = escapeMapHtml(item.label);
        const color = escapeMapHtml(item.color);
        return `<span class="map-band-legend-item"><span class="map-band-legend-swatch" style="--legend-color:${color};"></span><span class="map-band-legend-text">${label}</span></span>`;
      })
      .join("");
    const title = isSourcePhase ? "Source Colors" : "Band Colors";
    legendEl.innerHTML = `<div class="map-band-legend-title">${title}</div><div class="map-band-legend-list">${rows}</div>`;
  }

  window.enableMapSourceFilter = function(key) {
    if (Object.prototype.hasOwnProperty.call(mapFilter, key) && !mapFilter[key]) {
      mapFilter[key] = true;
      rebuildMapLocatorFilters();
      applyMapFilter();
    }
  };

  function rebuildMapLocatorFilters() {
    const phaseEl = document.getElementById("map-locator-phase");
    const choiceEl = document.getElementById("map-locator-choice-filter");
    const choiceLabelEl = document.getElementById("map-locator-choice-label");

    const availableSources = new Set();
    for (const entry of aisMarkers.values()) {
      if (entry?.visibleInHistoryWindow) {
        availableSources.add("ais");
        break;
      }
    }
    for (const entry of vdesMarkers.values()) {
      if (entry?.visibleInHistoryWindow) {
        availableSources.add("vdes");
        break;
      }
    }
    for (const entry of stationMarkers.values()) {
      if (entry?.type === "aprs" && entry?.visibleInHistoryWindow) {
        availableSources.add("aprs");
        break;
      }
    }
    const bandMap = new Map();
    for (const entry of locatorMarkers.values()) {
      const sourceType = entry?.sourceType;
      if (!sourceType) continue;
      if ((sourceType === "ft8" || sourceType === "ft4" || sourceType === "ft2" || sourceType === "wspr") && !entry?.visibleInHistoryWindow) continue;
      availableSources.add(sourceType);
      const meta = entry?.bandMeta instanceof Map ? entry.bandMeta : new Map();
      for (const [label, hz] of meta.entries()) {
        if (!bandMap.has(label)) {
          bandMap.set(label, {
            key: label,
            label,
            color: locatorBandChipColor(label),
            kind: "band",
            sortHz: Number.isFinite(hz) ? hz : 0,
          });
          continue;
        }
        const existing = bandMap.get(label);
        if (existing && Number.isFinite(hz) && (!Number.isFinite(existing.sortHz) || hz > existing.sortHz)) {
          existing.sortHz = hz;
        }
        if (existing && !existing.color) {
          existing.color = locatorBandChipColor(label);
        }
      }
    }

    for (const key of Array.from(mapLocatorFilter.bands)) {
      if (!bandMap.has(key)) mapLocatorFilter.bands.delete(key);
    }

    const sourceItems = ["ais", "vdes", "aprs", "bookmark", "ft8", "ft4", "ft2", "wspr"]
      .filter((key) => availableSources.has(key))
      .map((key) => ({
        key,
        label: mapSourceLabel(key),
        color: mapSourceColor(key),
        kind: "source",
      }));
    const bandItems = Array.from(bandMap.values())
      .sort((a, b) => (b.sortHz - a.sortHz) || a.label.localeCompare(b.label));

    renderMapLocatorLegend(mapLocatorFilter.phase, sourceItems, bandItems);
    if (!phaseEl || !choiceEl || !choiceLabelEl) return;

    renderMapLocatorPhaseRow(phaseEl, mapLocatorFilter.phase);
    if (mapLocatorFilter.phase === "band") {
      choiceLabelEl.textContent = "Visible Bands";
      renderMapLocatorChipRow(choiceEl, bandItems, mapLocatorFilter.bands, "band");
    } else {
      choiceLabelEl.textContent = "Visible Sources";
      renderMapLocatorChipRow(choiceEl, sourceItems, null, "source");
    }
    syncLocatorMarkerStyles();
    syncDecodeContactPathVisibility();
  }

  function markerPassesLocatorFilters(marker) {
    const meta = marker?._locatorFilterMeta;
    if (!meta) return true;
    if (mapLocatorFilter.phase === "band") {
      if (mapLocatorFilter.bands.size === 0) return true;
      if (!(meta.bands instanceof Set)) return false;
      for (const label of mapLocatorFilter.bands) {
        if (meta.bands.has(label)) return true;
      }
      return false;
    }
    return true;
  }

  function markerSearchText(marker) {
    const type = marker?.__trxType;
    if (type === "bookmark" || type === "ft8" || type === "ft4" || type === "ft2" || type === "wspr") {
      const entry = locatorEntryForMarker(marker);
      const parts = [];
      if (entry?.grid) parts.push(entry.grid);
      if (entry?.sourceType) parts.push(locatorSourceLabel(entry.sourceType));
      if (entry?.bandMeta instanceof Map) parts.push(...Array.from(entry.bandMeta.keys()));
      if (Array.isArray(entry?.bookmarks)) {
        for (const bm of entry.bookmarks) {
          if (bm?.name) parts.push(String(bm.name));
          if (bm?.locator) parts.push(String(bm.locator));
          if (bm?.mode) parts.push(String(bm.mode));
          if (bm?.category) parts.push(String(bm.category));
          if (bm?.comment) parts.push(String(bm.comment));
          if (Number.isFinite(bm?.freq_hz)) parts.push(String(Math.round(Number(bm.freq_hz))));
        }
      }
      if (entry?.stations instanceof Set) {
        parts.push(...Array.from(entry.stations.values()).map((v) => String(v)));
      }
      if (entry?.stationDetails instanceof Map) {
        for (const detail of entry.stationDetails.values()) {
          if (detail?.station) parts.push(String(detail.station));
          if (detail?.message) parts.push(String(detail.message));
          if (Number.isFinite(detail?.freq_hz)) parts.push(String(Math.round(Number(detail.freq_hz))));
        }
      }
      return parts.join(" ").toLowerCase();
    }
    if (type === "aprs") {
      const call = marker?._aprsCall ? String(marker._aprsCall) : "";
      const entry = stationMarkers.get(call);
      const info = entry?.info ? String(entry.info) : "";
      const pktRaw = entry?.pkt?.raw ? String(entry.pkt.raw) : "";
      return `${call} ${info} ${pktRaw}`.toLowerCase();
    }
    if (type === "ais") {
      const key = marker?._aisMmsi ? String(marker._aisMmsi) : "";
      const msg = aisMarkers.get(key)?.msg;
      return [
        key,
        msg?.name,
        msg?.callsign,
        msg?.destination,
        Number.isFinite(msg?.mmsi) ? String(msg.mmsi) : "",
        Number.isFinite(msg?.lat) ? String(msg.lat) : "",
        Number.isFinite(msg?.lon) ? String(msg.lon) : "",
      ].join(" ").toLowerCase();
    }
    if (type === "vdes") {
      const key = marker?._vdesKey ? String(marker._vdesKey) : "";
      const msg = vdesMarkers.get(key)?.msg;
      return [
        key,
        msg?.name,
        msg?.mmsi,
        msg?.message,
        msg?.raw,
        Number.isFinite(msg?.lat) ? String(msg.lat) : "",
        Number.isFinite(msg?.lon) ? String(msg.lon) : "",
      ].join(" ").toLowerCase();
    }
    return "";
  }

  function markerPassesSearchFilter(marker) {
    const query = String(mapSearchFilter || "").trim().toLowerCase();
    if (!query) return true;
    const terms = query.split(/\s+/).filter(Boolean);
    if (terms.length === 0) return true;
    const haystack = markerSearchText(marker);
    if (!haystack) return false;
    return terms.every((term) => haystack.includes(term));
  }

  function _receiverLocationKey(lat, lon) {
    return lat.toFixed(6) + "," + lon.toFixed(6);
  }

  function syncAprsReceiverMarker() {
    if (!aprsMap) return;
    // Build unique locations from all rigs
    const locGroups = {}; // key -> { lat, lon, rigs: [...] }
    const activeId = T.lastActiveRigId || T.serverActiveRigId || null;
    for (const rig of T.serverRigs) {
      if (!rig || !rig.remote) continue;
      const lat = rig.latitude, lon = rig.longitude;
      if (lat == null || lon == null || !Number.isFinite(lat) || !Number.isFinite(lon)) continue;
      const key = _receiverLocationKey(lat, lon);
      if (!locGroups[key]) locGroups[key] = { lat, lon, rigs: [], hasActive: false };
      locGroups[key].rigs.push(rig.remote);
      if (rig.remote === activeId) locGroups[key].hasActive = true;
    }
    // Fallback: if active rig has SSE location but isn't in T.serverRigs yet
    if (T.serverLat != null && T.serverLon != null) {
      const key = _receiverLocationKey(T.serverLat, T.serverLon);
      if (!locGroups[key]) locGroups[key] = { lat: T.serverLat, lon: T.serverLon, rigs: [], hasActive: true };
      if (!locGroups[key].hasActive) locGroups[key].hasActive = true;
    }

    const seen = new Set();
    let didInitialView = false;
    for (const [key, group] of Object.entries(locGroups)) {
      seen.add(key);
      const latLng = [group.lat, group.lon];
      const isActive = group.hasActive;
      let m = aprsMapReceiverMarkers[key];
      if (!m) {
        m = L.circleMarker(latLng, {
          radius: isActive ? 8 : 6,
          className: "trx-receiver-marker" + (isActive ? "" : " trx-receiver-marker-secondary"),
          fillOpacity: isActive ? 0.8 : 0.6,
        }).addTo(aprsMap).bindPopup("");
        m._receiverLocKey = key;
        m._receiverRigs = group.rigs;
        aprsMapReceiverMarkers[key] = m;
        if (isActive && !didInitialView) {
          aprsMap.setView(latLng, Math.max(1, T.initialMapZoom));
          didInitialView = true;
        }
      } else {
        m.setLatLng(latLng);
        m._receiverRigs = group.rigs;
        m.setRadius(isActive ? 8 : 6);
        if (!aprsMap.hasLayer(m)) m.addTo(aprsMap);
      }
      // Keep legacy reference for the active-rig location marker
      if (isActive) aprsMapReceiverMarker = m;
    }
    // Remove markers for locations no longer present
    for (const key of Object.keys(aprsMapReceiverMarkers)) {
      if (!seen.has(key)) {
        const m = aprsMapReceiverMarkers[key];
        if (m && aprsMap.hasLayer(m)) m.removeFrom(aprsMap);
        delete aprsMapReceiverMarkers[key];
      }
    }
    if (!seen.size) aprsMapReceiverMarker = null;
  }

  // ---------------------------------------------------------------------------
  // Weather satellite image overlays on the map
  // ---------------------------------------------------------------------------

  const satOverlays = new Map(); // key -> { overlay, track, msg }
  let satOverlaySeq = 0;

  window.addSatMapOverlay = function(msg) {
    if (!msg || !msg.geo_bounds || !msg.path) return;
    const bounds = msg.geo_bounds;
    // bounds = [south, west, north, east]
    if (!Array.isArray(bounds) || bounds.length !== 4) return;
    const latLngBounds = L.latLngBounds(
      [bounds[0], bounds[1]], // SW
      [bounds[2], bounds[3]]  // NE
    );
    const key = "sat-" + (++satOverlaySeq);
    const overlay = L.imageOverlay(msg.path, latLngBounds, {
      opacity: 0.55,
      interactive: true,
      zIndex: 300,
    });
    overlay.__trxType = "sat";
    overlay.__trxSatKey = key;
    overlay.__trxRigIds = msg.rig_id ? new Set([msg.rig_id]) : new Set();
    overlay.__trxHistoryVisible = true;
    mapMarkers.add(overlay);

    // Build a popup for the overlay
    const decoder = "Meteor LRPT";
    const satellite = msg.satellite || "Unknown";
    const ts = msg.ts_ms ? new Date(msg.ts_ms).toLocaleString() : "";
    overlay.bindPopup(
      `<div style="font-size:0.82rem;max-width:200px;">` +
      `<strong>${escapeMapHtml(decoder)}</strong><br>` +
      `${escapeMapHtml(satellite)}<br>` +
      `${escapeMapHtml(ts)}<br>` +
      (msg.path ? `<a href="${escapeMapHtml(msg.path)}" target="_blank" style="color:var(--accent);">Download PNG</a>` : "") +
      `</div>`
    );

    // Add ground track polyline if available
    let track = null;
    if (msg.ground_track && Array.isArray(msg.ground_track) && msg.ground_track.length >= 2) {
      const latlngs = msg.ground_track.map(function(pt) { return [pt[0], pt[1]]; });
      track = L.polyline(latlngs, {
        color: mapSourceColor("sat"),
        weight: 2,
        opacity: 0.7,
        dashArray: "6, 4",
      });
      track.__trxType = "sat";
      track.__trxSatKey = key;
      track.__trxRigIds = overlay.__trxRigIds;
      track.__trxHistoryVisible = true;
      mapMarkers.add(track);
      if (aprsMap) {
        track.addTo(aprsMap);
      }
    }

    satOverlays.set(key, { overlay: overlay, track: track, msg: msg });

    if (aprsMap) {
      overlay.addTo(aprsMap);
    }
    applyMapFilter();
  };

  window.removeSatMapOverlay = function(key) {
    const entry = satOverlays.get(key);
    if (!entry) return;
    if (entry.overlay) {
      mapMarkers.delete(entry.overlay);
      if (aprsMap && aprsMap.hasLayer(entry.overlay)) entry.overlay.removeFrom(aprsMap);
    }
    if (entry.track) {
      mapMarkers.delete(entry.track);
      if (aprsMap && aprsMap.hasLayer(entry.track)) entry.track.removeFrom(aprsMap);
    }
    satOverlays.delete(key);
  };

  window.clearSatMapOverlays = function() {
    for (const [key] of satOverlays) {
      window.removeSatMapOverlay(key);
    }
  };

  window.clearMapMarkersByType = function(type) {
    if (type === "aprs") {
      selectedAprsTrackCall = null;
      stationMarkers.forEach((entry) => {
        if (entry && entry.marker) {
          if (aprsMap && aprsMap.hasLayer(entry.marker)) entry.marker.removeFrom(aprsMap);
          mapMarkers.delete(entry.marker);
        }
        if (entry && entry.track) {
          if (aprsMap && aprsMap.hasLayer(entry.track)) entry.track.removeFrom(aprsMap);
          mapMarkers.delete(entry.track);
        }
      });
      stationMarkers.clear();
      return;
    }

    if (type === "ais") {
      aisMarkers.forEach((entry) => {
        if (entry && entry.marker) {
          if (aprsMap && aprsMap.hasLayer(entry.marker)) entry.marker.removeFrom(aprsMap);
          mapMarkers.delete(entry.marker);
        }
        if (entry && entry.track) {
          if (aprsMap && aprsMap.hasLayer(entry.track)) entry.track.removeFrom(aprsMap);
          mapMarkers.delete(entry.track);
        }
      });
      selectedAisTrackMmsi = null;
      aisMarkers.clear();
      return;
    }

    if (type === "vdes") {
      vdesMarkers.forEach((entry) => {
        if (entry && entry.marker) {
          if (aprsMap && aprsMap.hasLayer(entry.marker)) entry.marker.removeFrom(aprsMap);
          mapMarkers.delete(entry.marker);
        }
      });
      vdesMarkers.clear();
      return;
    }

    if (type === "sat") {
      window.clearSatMapOverlays();
      return;
    }

    if (type === "ft8" || type === "ft4" || type === "ft2" || type === "wspr") {
      const prefix = `${type}:`;
      for (const [key, entry] of locatorMarkers.entries()) {
        if (!key.startsWith(prefix)) continue;
        if (entry && entry.marker) {
          if (entry.marker === selectedLocatorMarker) {
            setSelectedLocatorMarker(null);
            clearMapRadioPath();
          }
          if (aprsMap && aprsMap.hasLayer(entry.marker)) entry.marker.removeFrom(aprsMap);
          mapMarkers.delete(entry.marker);
        }
        locatorMarkers.delete(key);
      }
      rebuildMapLocatorFilters();
      rebuildDecodeContactPaths();
    }

    if (type === "bookmark") {
      for (const [key, entry] of locatorMarkers.entries()) {
        if (!key.startsWith("bookmark:")) continue;
        if (entry && entry.marker) {
          if (entry.marker === selectedLocatorMarker) {
            setSelectedLocatorMarker(null);
            clearMapRadioPath();
          }
          if (aprsMap && aprsMap.hasLayer(entry.marker)) entry.marker.removeFrom(aprsMap);
          mapMarkers.delete(entry.marker);
        }
        locatorMarkers.delete(key);
      }
      rebuildMapLocatorFilters();
    }
  };

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

  function mapStageEl() {
    return document.getElementById("map-stage");
  }

  function mapIsFullscreen() {
    const stage = mapStageEl();
    if (!stage) return false;
    return document.fullscreenElement === stage
      || document.webkitFullscreenElement === stage
      || stage.classList.contains("map-fake-fullscreen");
  }

  function mapExitFakeFullscreen() {
    const stage = mapStageEl();
    if (!stage) return;
    stage.classList.remove("map-fake-fullscreen");
    document.body.classList.remove("map-fake-fullscreen-active");
  }

  function mapEnterFakeFullscreen() {
    const stage = mapStageEl();
    if (!stage) return;
    stage.classList.add("map-fake-fullscreen");
    document.body.classList.add("map-fake-fullscreen-active");
  }

  function updateMapFullscreenButton() {
    const btn = document.getElementById("map-fullscreen-btn");
    if (!btn) return;
    btn.textContent = mapIsFullscreen() ? "Exit Fullscreen" : "Fullscreen";
  }

  function applyMapOverlayPanelVisibility() {
    const panel = document.querySelector("#map-stage .map-overlay-panel");
    if (!panel) return;
    panel.classList.toggle("is-hidden", !mapOverlayPanelVisible);
  }

  function updateMapOverlayToggleButton() {
    const btn = document.getElementById("map-overlay-toggle-btn");
    if (!btn) return;
    btn.textContent = mapOverlayPanelVisible ? "Hide Filters" : "Show Filters";
  }

  async function toggleMapFullscreen() {
    const stage = mapStageEl();
    if (!stage) return;
    try {
      const isNative = document.fullscreenElement === stage || document.webkitFullscreenElement === stage;
      const isFake   = stage.classList.contains("map-fake-fullscreen");
      if (isNative) {
        if (document.exitFullscreen) await document.exitFullscreen();
        else if (document.webkitExitFullscreen) await document.webkitExitFullscreen();
      } else if (isFake) {
        mapExitFakeFullscreen();
      } else {
        // Try native fullscreen; fall back to CSS fake fullscreen when the
        // API is unavailable or blocked (e.g. mobile Safari).
        const nativeFn = stage.requestFullscreen || stage.webkitRequestFullscreen;
        if (nativeFn) {
          try {
            await nativeFn.call(stage);
          } catch (_) {
            mapEnterFakeFullscreen();
          }
        } else {
          mapEnterFakeFullscreen();
        }
      }
    } catch (err) {
      console.error("Map fullscreen toggle failed", err);
    } finally {
      updateMapFullscreenButton();
      requestAnimationFrame(() => sizeAprsMapToViewport());
    }
  }

  // Allow Escape to exit CSS fake fullscreen (native fullscreen handles its own Escape).
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      const stage = mapStageEl();
      if (stage && stage.classList.contains("map-fake-fullscreen")) {
        mapExitFakeFullscreen();
        updateMapFullscreenButton();
        requestAnimationFrame(() => sizeAprsMapToViewport());
      }
    }
  });

  function materializeBufferedMapLayers() {
    if (!aprsMap) return;
    for (const [key, entry] of locatorMarkers) {
      if (!key.startsWith("bookmark:") || entry?.marker || !entry?.grid) continue;
      const bounds = maidenheadToBounds(entry.grid);
      if (!bounds) continue;
      entry.sourceType = "bookmark";
      entry.bandMeta = collectBandMeta((entry.bookmarks || []).map((bm) => Number(bm?.freq_hz)));
      entry.marker = L.rectangle(bounds, locatorStyleForEntry(entry, entry.bookmarks?.length || 1))
        .addTo(aprsMap)
        .bindPopup(buildBookmarkLocatorPopupHtml(entry.grid, entry.bookmarks || []));
      entry.marker.__trxType = "bookmark";
      sendLocatorOverlayToBack(entry.marker);
      assignLocatorMarkerMeta(entry.marker, entry.sourceType, entry.bandMeta);
      mapMarkers.add(entry.marker);
    }
    pruneMapHistory();
  }

  function initAprsMap() {
    if (typeof L === "undefined") return;
    const mapEl = document.getElementById("aprs-map");
    if (!mapEl) return;
    sizeAprsMapToViewport();
    if (aprsMap) return;

    const hasLocation = T.serverLat != null && T.serverLon != null;
    const center = hasLocation ? [T.serverLat, T.serverLon] : [20, 0];
    const zoom = hasLocation ? T.initialMapZoom : 2;

    aprsMap = L.map("aprs-map").setView(center, zoom);
    updateMapBaseLayerForTheme(T.currentTheme());
    syncAprsReceiverMarker();

    // Rebuild popup content on open (keeps age/distance/rig list fresh)
    aprsMap.on("popupopen", function(e) {
      const marker = e.popup._source;
      clearMapRadioPath();
      setSelectedLocatorMarker(null);
      if (selectedAprsTrackCall) {
        const prevEntry = stationMarkers.get(String(selectedAprsTrackCall));
        if (prevEntry && prevEntry.track && aprsMap && aprsMap.hasLayer(prevEntry.track)) {
          prevEntry.track.removeFrom(aprsMap);
        }
        selectedAprsTrackCall = null;
      }
      if (selectedAisTrackMmsi) {
        selectedAisTrackMmsi = null;
        syncSelectedAisTrackVisibility();
      }

      if (marker._receiverLocKey) {
        e.popup.setContent(buildReceiverPopupHtml(marker._receiverRigs || []));
        return;
      }

      if (!marker) return;
      const ll = typeof marker.getLatLng === "function" ? marker.getLatLng() : null;

      if (marker._aprsCall) {
        if (!ll) return;
        const entry = stationMarkers.get(marker._aprsCall);
        if (!entry) return;
        e.popup.setContent(buildAprsPopupHtml(marker._aprsCall, ll.lat, ll.lng, entry.info || "", entry.pkt));
        refreshAprsTrack(String(marker._aprsCall), entry);
        if (entry.track && aprsMap && mapFilter.aprs && !aprsMap.hasLayer(entry.track)) {
          entry.track.addTo(aprsMap);
        }
        selectedAprsTrackCall = String(marker._aprsCall);
        setMapRadioPathTo(ll.lat, ll.lng, mapSourceColor("aprs"), "aprs-radio-path", marker.__trxRigIds);
        return;
      }

      if (marker._aisMmsi) {
        if (!ll) return;
        const entry = aisMarkers.get(String(marker._aisMmsi));
        if (!entry || !entry.msg) return;
        e.popup.setContent(buildAisPopupHtml(entry.msg));
        refreshAisTrack(String(marker._aisMmsi), entry);
        selectedAisTrackMmsi = String(marker._aisMmsi);
        syncSelectedAisTrackVisibility();
        setMapRadioPathTo(ll.lat, ll.lng, mapSourceColor("ais"), "aprs-radio-path", marker.__trxRigIds);
        return;
      }

      if (marker._vdesKey) {
        if (!ll) return;
        const entry = vdesMarkers.get(String(marker._vdesKey));
        if (!entry || !entry.msg) return;
        e.popup.setContent(buildVdesPopupHtml(entry.msg));
        setMapRadioPathTo(ll.lat, ll.lng, mapSourceColor("vdes"), "aprs-radio-path", marker.__trxRigIds);
        return;
      }

      if (marker.__trxType === "ft8" || marker.__trxType === "ft4" || marker.__trxType === "ft2" || marker.__trxType === "wspr") {
        const center = locatorMarkerCenter(marker);
        if (center) {
          setSelectedLocatorMarker(marker);
          const lEntry = locatorEntryForMarker(marker);
          const lColor = lEntry ? locatorStyleForEntry(lEntry, locatorEntryCount(lEntry)).color : locatorFilterColor(marker.__trxType);
          setMapRadioPathTo(center.lat, center.lon, lColor, "locator-radio-path", marker.__trxRigIds);
        }
      } else if (marker.__trxType === "bookmark") {
        setSelectedLocatorMarker(marker);
      }
    });

    aprsMap.on("popupclose", function() {
      clearMapRadioPath();
      setSelectedLocatorMarker(null);
      if (selectedAprsTrackCall) {
        const entry = stationMarkers.get(String(selectedAprsTrackCall));
        if (entry && entry.track && aprsMap && aprsMap.hasLayer(entry.track)) {
          entry.track.removeFrom(aprsMap);
        }
        selectedAprsTrackCall = null;
      }
      if (selectedAisTrackMmsi) {
        selectedAisTrackMmsi = null;
        syncSelectedAisTrackVisibility();
      }
    });

    materializeBufferedMapLayers();

    const locatorPhaseEl = document.getElementById("map-locator-phase");
    const locatorChoiceEl = document.getElementById("map-locator-choice-filter");
    const mapSearchEl = document.getElementById("map-search-filter");
    const mapHistoryLimitEl = document.getElementById("map-history-limit");
    const mapP2pPathsToggleEl = document.getElementById("map-p2p-paths-toggle");
    const mapContactPathsToggleEl = document.getElementById("map-contact-paths-toggle");
    const fullscreenBtn = document.getElementById("map-fullscreen-btn");
    const overlayToggleBtn = document.getElementById("map-overlay-toggle-btn");
    if (locatorPhaseEl) {
      locatorPhaseEl.addEventListener("click", (e) => {
        const btn = e.target.closest(".map-locator-phase-btn[data-phase]");
        if (!btn) return;
        const phase = String(btn.dataset.phase || "");
        if (phase !== "type" && phase !== "band") return;
        if (mapLocatorFilter.phase === phase) return;
        mapLocatorFilter.phase = phase;
        rebuildMapLocatorFilters();
        applyMapFilter();
      });
    }
    if (locatorChoiceEl) {
      locatorChoiceEl.addEventListener("click", (e) => {
        const chip = e.target.closest(".map-locator-chip[data-filter-kind]");
        if (!chip) return;
        const kind = String(chip.dataset.filterKind || "");
        const key = String(chip.dataset.filterKey || "");
        if (!key) return;
        if (kind === "source" && Object.prototype.hasOwnProperty.call(mapFilter, key)) {
          // toggle the clicked source; when none are selected everything is shown
          mapFilter[key] = !mapFilter[key];
          const srcKeys = Object.keys(DEFAULT_MAP_SOURCE_FILTER);
          const anySelected = srcKeys.some((k) => mapFilter[k]);
          if (anySelected && !mapFilter.aprs && selectedAprsTrackCall) {
            const entry = stationMarkers.get(String(selectedAprsTrackCall));
            if (entry && entry.track && aprsMap && aprsMap.hasLayer(entry.track)) {
              entry.track.removeFrom(aprsMap);
            }
            selectedAprsTrackCall = null;
          }
          if (anySelected && !mapFilter.ais && selectedAisTrackMmsi) {
            const entry = aisMarkers.get(String(selectedAisTrackMmsi));
            if (entry && entry.track && aprsMap && aprsMap.hasLayer(entry.track)) {
              entry.track.removeFrom(aprsMap);
            }
            selectedAisTrackMmsi = null;
          }
        } else if (kind === "band") {
          if (mapLocatorFilter.bands.has(key)) {
            mapLocatorFilter.bands.delete(key);
          } else {
            mapLocatorFilter.bands.add(key);
          }
        }
        rebuildMapLocatorFilters();
        applyMapFilter();
      });
    }
    const mapRigFilterEl = document.getElementById("map-rig-filter");
    if (mapRigFilterEl) {
      mapRigFilterEl.addEventListener("change", () => {
        mapRigFilter = mapRigFilterEl.value;
        applyMapFilter();
      });
    }
    if (mapSearchEl) {
      mapSearchEl.value = mapSearchFilter;
      mapSearchEl.addEventListener("input", () => {
        mapSearchFilter = String(mapSearchEl.value || "").trim();
        applyMapFilter();
      });
    }
    if (mapHistoryLimitEl) {
      mapHistoryLimitEl.value = String(mapHistoryLimitMinutes);
      mapHistoryLimitEl.addEventListener("change", () => {
        mapHistoryLimitMinutes = normalizeMapHistoryLimitMinutes(Number(mapHistoryLimitEl.value));
        mapHistoryLimitEl.value = String(mapHistoryLimitMinutes);
        saveSetting("mapHistoryLimitMinutes", mapHistoryLimitMinutes);
        pruneMapHistory();
      });
    }
    if (mapP2pPathsToggleEl) {
      updateMapP2pPathsToggle();
      mapP2pPathsToggleEl.addEventListener("click", () => {
        mapP2pRadioPathsEnabled = !mapP2pRadioPathsEnabled;
        saveSetting("mapP2pRadioPathsEnabled", mapP2pRadioPathsEnabled);
        updateMapP2pPathsToggle();
        if (!mapP2pRadioPathsEnabled) clearMapRadioPath();
      });
    }
    if (mapContactPathsToggleEl) {
      updateMapContactPathsToggle();
      mapContactPathsToggleEl.addEventListener("click", () => {
        mapDecodeContactPathsEnabled = !mapDecodeContactPathsEnabled;
        saveSetting("mapDecodeContactPathsEnabled", mapDecodeContactPathsEnabled);
        updateMapContactPathsToggle();
        syncDecodeContactPathVisibility();
      });
    }
    if (fullscreenBtn) {
      fullscreenBtn.addEventListener("click", () => {
        toggleMapFullscreen();
      });
      updateMapFullscreenButton();
    }
    applyMapOverlayPanelVisibility();
    updateMapOverlayToggleButton();
    if (overlayToggleBtn) {
      overlayToggleBtn.addEventListener("click", () => {
        mapOverlayPanelVisible = !mapOverlayPanelVisible;
        saveSetting("mapOverlayPanelVisible", mapOverlayPanelVisible);
        applyMapOverlayPanelVisibility();
        updateMapOverlayToggleButton();
      });
    }
    if (!mapFullscreenListenerBound) {
      const onFullscreenChange = () => {
        updateMapFullscreenButton();
        sizeAprsMapToViewport();
      };
      document.addEventListener("fullscreenchange", onFullscreenChange);
      document.addEventListener("webkitfullscreenchange", onFullscreenChange);
      mapFullscreenListenerBound = true;
    }
    if (!mapHistoryPruneTimer) {
      mapHistoryPruneTimer = setInterval(() => {
        pruneMapHistory();
      }, 60 * 1000);
    }
    rebuildMapLocatorFilters();
  }

  function sizeAprsMapToViewport() {
    const mapEl = document.getElementById("aprs-map");
    if (!mapEl) return;
    const stage = mapStageEl();
    if (mapIsFullscreen() && stage) {
      // For CSS fake fullscreen use window.innerHeight directly — clientHeight
      // may not yet reflect the fixed layout when called synchronously after
      // adding the class.
      const isFake = stage.classList.contains("map-fake-fullscreen");
      const stageHeight = isFake
        ? window.innerHeight
        : (stage.clientHeight || stage.getBoundingClientRect().height);
      const target = Math.max(260, Math.floor(stageHeight));
      mapEl.style.height = `${target}px`;
      if (aprsMap) aprsMap.invalidateSize();
      return;
    }
    const mapRect = mapEl.getBoundingClientRect();
    const width = mapEl.clientWidth || mapRect.width;
    const footer = document.querySelector(".footer");
    let bottom = mapIsFullscreen() && stage
      ? stage.getBoundingClientRect().bottom
      : window.innerHeight;
    if (!mapIsFullscreen() && footer) {
      const fr = footer.getBoundingClientRect();
      if (fr.top > mapRect.top + 50) bottom = fr.top;
    }
    const available = Math.max(0, Math.floor(bottom - mapRect.top - 8));
    const widthDriven = width > 0 ? Math.floor(width / 1.55) : available;
    const viewportCap = mapIsFullscreen()
      ? Math.floor(window.innerHeight * 0.9)
      : Math.floor(window.innerHeight * 0.75);
    const minHeight = Math.min(260, available);
    const target = Math.max(minHeight, Math.min(available, viewportCap, widthDriven));
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

  window.navigateToAprsMap = function(lat, lon) {
    // Activate the map tab
    T._activeTab = "map";
    document.querySelectorAll(".tab-bar .tab").forEach((t) => t.classList.remove("active"));
    const mapTabBtn = document.querySelector(".tab-bar .tab[data-tab='map']");
    if (mapTabBtn) mapTabBtn.classList.add("active");
    document.querySelectorAll(".tab-panel").forEach((p) => (p.style.display = "none"));
    const mapPanel = document.getElementById("tab-map");
    if (mapPanel) mapPanel.style.display = "";
    initAprsMap();
    sizeAprsMapToViewport();
    if (aprsMap) {
      setTimeout(() => {
        aprsMap.invalidateSize();
        aprsMap.setView([lat, lon], 13);
      }, 50);
    }
  };

  window.navigateToMapLocator = function(grid, preferredType = null) {
    const normalizedGrid = String(grid || "").trim().toUpperCase();
    if (!/^[A-R]{2}\d{2}(?:[A-X]{2})?$/.test(normalizedGrid)) return false;

    T._activeTab = "map";
    document.querySelectorAll(".tab-bar .tab").forEach((t) => t.classList.remove("active"));
    const mapTabBtn = document.querySelector(".tab-bar .tab[data-tab='map']");
    if (mapTabBtn) mapTabBtn.classList.add("active");
    document.querySelectorAll(".tab-panel").forEach((p) => (p.style.display = "none"));
    const mapPanel = document.getElementById("tab-map");
    if (mapPanel) mapPanel.style.display = "";

    initAprsMap();
    sizeAprsMapToViewport();
    if (!aprsMap) return false;

    const pref = preferredType === "wspr" ? "wspr" : (preferredType === "ft4" ? "ft4" : (preferredType === "ft2" ? "ft2" : (preferredType === "ft8" ? "ft8" : null)));
    const keys = pref
      ? [`${pref}:${normalizedGrid}`, `ft8:${normalizedGrid}`, `ft4:${normalizedGrid}`, `ft2:${normalizedGrid}`, `wspr:${normalizedGrid}`, `bookmark:${normalizedGrid}`]
      : [`ft8:${normalizedGrid}`, `ft4:${normalizedGrid}`, `ft2:${normalizedGrid}`, `wspr:${normalizedGrid}`, `bookmark:${normalizedGrid}`];
    let entry = null;
    for (const key of keys) {
      entry = locatorMarkers.get(key);
      if (entry?.marker) break;
    }
    if (!entry?.marker) return false;

    if (pref && Object.prototype.hasOwnProperty.call(mapFilter, pref) && !mapFilter[pref]) {
      mapFilter[pref] = true;
      rebuildMapLocatorFilters();
      applyMapFilter();
    }

    const marker = entry.marker;
    if (!aprsMap.hasLayer(marker)) {
      marker.addTo(aprsMap);
      sendLocatorOverlayToBack(marker);
    }
    const center = locatorMarkerCenter(marker);
    const focusMarker = () => {
      if (!aprsMap || !marker) return;
      aprsMap.invalidateSize();
      if (center) {
        const targetZoom = Math.max(aprsMap.getZoom() || 0, 7);
        aprsMap.setView([center.lat, center.lon], targetZoom);
        if (marker.__trxType !== "bookmark") {
          const fEntry = locatorEntryForMarker(marker);
          const fColor = fEntry ? locatorStyleForEntry(fEntry, locatorEntryCount(fEntry)).color : locatorFilterColor(marker?.__trxType);
          setMapRadioPathTo(center.lat, center.lon, fColor, "locator-radio-path", marker.__trxRigIds);
        }
      }
      setSelectedLocatorMarker(marker);
      if (typeof marker.openPopup === "function") marker.openPopup();
    };
    focusMarker();
    setTimeout(focusMarker, 60);
    return true;
  };








  function buildReceiverPopupHtml(rigIds) {
    const call = T.serverCallsign || T.ownerCallsign || "Receiver";
    let meta = "";
    if (T.serverVersion) {
      meta = `trx-server v${escapeMapHtml(T.serverVersion)}`;
      if (T.serverBuildDate) meta += ` &middot; ${escapeMapHtml(T.serverBuildDate)}`;
    }
    let rows = "";
    if (T.ownerCallsign && T.ownerCallsign !== T.serverCallsign) {
      rows += `<tr><td class="aprs-popup-label">Owner</td><td>${escapeMapHtml(T.ownerCallsign)}</td></tr>`;
    }
    // Show location from first matching rig or active rig
    const rigSet = rigIds && rigIds.length ? new Set(rigIds) : null;
    const firstRig = rigSet ? T.serverRigs.find(r => rigSet.has(r.remote)) : null;
    const popupLat = firstRig ? firstRig.latitude : T.serverLat;
    const popupLon = firstRig ? firstRig.longitude : T.serverLon;
    if (popupLat != null && popupLon != null) {
      const grid = latLonToMaidenhead(popupLat, popupLon);
      rows += `<tr><td class="aprs-popup-label">QTH</td><td>${popupLat.toFixed(5)}, ${popupLon.toFixed(5)} (${escapeMapHtml(grid)})</td></tr>`;
    }
    // Show rigs at this location
    const rigsToShow = rigSet
      ? T.serverRigs.filter(r => rigSet.has(r.remote))
      : T.serverRigs;
    for (const rig of rigsToShow) {
      const name = rig.display_name || `${rig.manufacturer} ${rig.model}`.trim();
      const active = rig.remote === T.serverActiveRigId
        ? ` <span class="receiver-popup-active">active</span>` : "";
      rows += `<tr><td class="aprs-popup-label">Rig</td><td>${escapeMapHtml(name)}${active}</td></tr>`;
    }
    return `<div class="aprs-popup">` +
      `<div class="aprs-popup-call">${escapeMapHtml(call)}</div>` +
      (meta ? `<div class="aprs-popup-meta">${meta}</div>` : "") +
      (rows ? `<table class="aprs-popup-table">${rows}</table>` : "") +
      `</div>`;
  }

  function buildAprsPopupHtml(call, lat, lon, info, pkt) {
    const age = pkt?._tsMs ? formatTimeAgo(pkt._tsMs) : (pkt?._ts || null);
    const distKm = (T.serverLat != null && T.serverLon != null)
      ? haversineKm(T.serverLat, T.serverLon, lat, lon)
      : null;
    const distStr = distKm != null
      ? (distKm < 1 ? `${Math.round(distKm * 1000)} m` : `${distKm.toFixed(1)} km`)
      : null;
    const path = pkt?.path || null;
    const type = pkt?.type || null;

    let meta = [age, distStr].filter(Boolean).join(" &middot; ");
    let rows = "";
    if (type) rows += `<tr><td class="aprs-popup-label">Type</td><td>${escapeMapHtml(type)}</td></tr>`;
    if (path) rows += `<tr><td class="aprs-popup-label">Path</td><td>${escapeMapHtml(path)}</td></tr>`;
    if (lat != null && lon != null)
      rows += `<tr><td class="aprs-popup-label">Pos</td><td>${lat.toFixed(5)}, ${lon.toFixed(5)}</td></tr>`;

    return `<div class="aprs-popup">` +
      `<div class="aprs-popup-call">${escapeMapHtml(call)}</div>` +
      (meta ? `<div class="aprs-popup-meta">${meta}</div>` : "") +
      (rows ? `<table class="aprs-popup-table">${rows}</table>` : "") +
      (info ? `<div class="aprs-popup-info">${escapeMapHtml(info)}</div>` : "") +
      `</div>`;
  }

  function buildAisPopupHtml(msg) {
    const age = msg?._tsMs ? formatTimeAgo(msg._tsMs) : null;
    const distKm = (T.serverLat != null && T.serverLon != null && msg?.lat != null && msg?.lon != null)
      ? haversineKm(T.serverLat, T.serverLon, msg.lat, msg.lon)
      : null;
    const distStr = distKm != null
      ? (distKm < 1 ? `${Math.round(distKm * 1000)} m` : `${distKm.toFixed(1)} km`)
      : null;
    const meta = [age, distStr, msg?.channel ? `AIS ${escapeMapHtml(msg.channel)}` : null].filter(Boolean).join(" &middot; ");
    let rows = "";
    rows += `<tr><td class="aprs-popup-label">MMSI</td><td>${escapeMapHtml(String(msg.mmsi || "--"))}</td></tr>`;
    rows += `<tr><td class="aprs-popup-label">Type</td><td>${escapeMapHtml(String(msg.message_type || "--"))}</td></tr>`;
    if (distStr) rows += `<tr><td class="aprs-popup-label">Range</td><td>${distStr} from TRX</td></tr>`;
    if (msg?.sog_knots != null) rows += `<tr><td class="aprs-popup-label">SOG</td><td>${Number(msg.sog_knots).toFixed(1)} kn</td></tr>`;
    if (msg?.cog_deg != null) rows += `<tr><td class="aprs-popup-label">COG</td><td>${Number(msg.cog_deg).toFixed(1)}&deg;</td></tr>`;
    if (msg?.heading_deg != null) rows += `<tr><td class="aprs-popup-label">HDG</td><td>${Number(msg.heading_deg).toFixed(0)}&deg;</td></tr>`;
    if (msg?.nav_status != null) rows += `<tr><td class="aprs-popup-label">Nav</td><td>${escapeMapHtml(String(msg.nav_status))}</td></tr>`;
    if (msg?.lat != null && msg?.lon != null) rows += `<tr><td class="aprs-popup-label">Pos</td><td>${msg.lat.toFixed(5)}, ${msg.lon.toFixed(5)}</td></tr>`;
    const info = [msg?.vessel_name, msg?.callsign, msg?.destination].filter(Boolean).map(escapeMapHtml).join(" · ");
    const vesselLabel = escapeMapHtml(msg?.vessel_name || `MMSI ${msg?.mmsi || "--"}`);
    const vesselUrl = window.buildAisVesselUrl ? window.buildAisVesselUrl(msg?.mmsi) : null;
    const vesselTitle = vesselUrl
      ? `<a class="title-link" href="${escapeMapHtml(vesselUrl)}" target="_blank" rel="noopener">${vesselLabel}</a>`
      : vesselLabel;
    return `<div class="aprs-popup">` +
      `<div class="aprs-popup-call">${vesselTitle}</div>` +
      (meta ? `<div class="aprs-popup-meta">${meta}</div>` : "") +
      (rows ? `<table class="aprs-popup-table">${rows}</table>` : "") +
      (info ? `<div class="aprs-popup-info">${info}</div>` : "") +
      `</div>`;
  }

  function buildVdesPopupHtml(msg) {
    const age = formatTimeAgo(msg?.ts_ms);
    const distKm = (T.serverLat != null && T.serverLon != null && msg?.lat != null && msg?.lon != null)
      ? haversineKm(T.serverLat, T.serverLon, msg.lat, msg.lon)
      : null;
    const distStr = distKm != null
      ? (distKm < 1 ? `${Math.round(distKm * 1000)} m` : `${distKm.toFixed(1)} km`)
      : null;
    const meta = [
      age,
      distStr,
      msg?.message_label ? escapeMapHtml(msg.message_label) : null,
      Number.isFinite(msg?.link_id) ? `LID ${Number(msg.link_id)}` : null,
    ].filter(Boolean).join(" &middot; ");
    let rows = "";
    if (distStr) rows += `<tr><td class="aprs-popup-label">Range</td><td>${distStr} from TRX</td></tr>`;
    rows += `<tr><td class="aprs-popup-label">Type</td><td>${escapeMapHtml(String(msg?.message_type ?? "--"))}</td></tr>`;
    if (Number.isFinite(msg?.source_id)) rows += `<tr><td class="aprs-popup-label">Source</td><td>${escapeMapHtml(String(msg.source_id))}</td></tr>`;
    if (Number.isFinite(msg?.destination_id)) rows += `<tr><td class="aprs-popup-label">Dest</td><td>${escapeMapHtml(String(msg.destination_id))}</td></tr>`;
    if (msg?.lat != null && msg?.lon != null) rows += `<tr><td class="aprs-popup-label">Pos</td><td>${msg.lat.toFixed(5)}, ${msg.lon.toFixed(5)}</td></tr>`;
    if (Number.isFinite(msg?.sync_score)) rows += `<tr><td class="aprs-popup-label">Sync</td><td>${(Number(msg.sync_score) * 100).toFixed(0)}%</td></tr>`;
    if (msg?.fec_state) rows += `<tr><td class="aprs-popup-label">FEC</td><td>${escapeMapHtml(String(msg.fec_state))}</td></tr>`;
    const info = [
      msg?.vessel_name,
      msg?.callsign,
      msg?.destination,
      msg?.payload_preview,
    ].filter(Boolean).map(escapeMapHtml).join(" · ");
    const title = escapeMapHtml(msg?.vessel_name || msg?.callsign || "VDES Position");
    return `<div class="aprs-popup">` +
      `<div class="aprs-popup-call">${title}</div>` +
      (meta ? `<div class="aprs-popup-meta">${meta}</div>` : "") +
      (rows ? `<table class="aprs-popup-table">${rows}</table>` : "") +
      (info ? `<div class="aprs-popup-info">${info}</div>` : "") +
      `</div>`;
  }

  function aprsPositionsEqual(a, b) {
    if (!a || !b) return false;
    const aLat = Array.isArray(a) ? a[0] : a.lat;
    const aLon = Array.isArray(a) ? a[1] : a.lon;
    const bLat = Array.isArray(b) ? b[0] : b.lat;
    const bLon = Array.isArray(b) ? b[1] : b.lon;
    return Math.abs(aLat - bLat) < 0.000001 && Math.abs(aLon - bLon) < 0.000001;
  }

  function aisPositionsEqual(a, b) {
    if (!a || !b) return false;
    const aLat = Array.isArray(a) ? a[0] : a.lat;
    const aLon = Array.isArray(a) ? a[1] : a.lon;
    const bLat = Array.isArray(b) ? b[0] : b.lat;
    const bLon = Array.isArray(b) ? b[1] : b.lon;
    return Math.abs(aLat - bLat) < 0.000001 && Math.abs(aLon - bLon) < 0.000001;
  }

  function vdesMarkerKey(msg) {
    if (Number.isFinite(msg?.source_id)) return `src:${Number(msg.source_id)}`;
    if (Number.isFinite(msg?.mmsi) && Number(msg.mmsi) > 0) return `mmsi:${Number(msg.mmsi)}`;
    if (msg?.lat != null && msg?.lon != null) {
      return `pos:${Number(msg.lat).toFixed(4)}:${Number(msg.lon).toFixed(4)}:${Number(msg?.message_type ?? 0)}`;
    }
    return null;
  }

  function _aprsAddMarkerToMap(call, entry) {
    refreshAprsTrack(call, entry);
    const icon = aprsSymbolIcon(entry.symbolTable, entry.symbolCode);
    const popupContent = buildAprsPopupHtml(call, entry.lat, entry.lon, entry.info || "", entry.pkt);
    const marker = icon
      ? L.marker([entry.lat, entry.lon], { icon }).addTo(aprsMap).bindPopup(popupContent)
      : L.circleMarker([entry.lat, entry.lon], {
          radius: 6, color: "#00d17f", fillColor: "#00d17f", fillOpacity: 0.8
        }).addTo(aprsMap).bindPopup(popupContent);
    marker.__trxType = "aprs";
    marker.__trxRigIds = entry.rigIds || new Set();
    marker._aprsCall = call;
    entry.marker = marker;
    mapMarkers.add(marker);
  }

  window.aprsMapAddStation = function(call, lat, lon, info, symbolTable, symbolCode, pkt) {
    const nextPoint = [lat, lon];
    const tsMs = Number.isFinite(pkt?._tsMs) ? Number(pkt._tsMs) : Date.now();
    const msgRigId = pkt?.rig_id || T.lastActiveRigId;
    const existing = stationMarkers.get(call);
    if (existing) {
      existing.pkt = pkt;
      existing.lat = lat;
      existing.lon = lon;
      existing.info = info;
      existing.symbolTable = symbolTable;
      existing.symbolCode = symbolCode;
      if (msgRigId) {
        if (!existing.rigIds) existing.rigIds = new Set();
        existing.rigIds.add(msgRigId);
      }
      if (!Array.isArray(existing.trackHistory)) existing.trackHistory = [];
      const prevPoint = existing.trackHistory[existing.trackHistory.length - 1];
      if (!aprsPositionsEqual(prevPoint, nextPoint)) {
        existing.trackHistory.push({ lat, lon, tsMs });
      } else if (prevPoint) {
        prevPoint.tsMs = tsMs;
      }
      pruneAprsEntry(call, existing, mapHistoryCutoffMs());
      if (aprsMap && existing.marker && !T.decodeHistoryReplayActive) {
        existing.marker.setLatLng([lat, lon]);
        existing.marker.setPopupContent(buildAprsPopupHtml(call, lat, lon, info, pkt));
      }
    } else {
      const entry = {
        marker: null,
        track: null,
        trackHistory: [{ lat, lon, tsMs }],
        trackPoints: [nextPoint],
        type: "aprs",
        pkt,
        lat,
        lon,
        info,
        symbolTable,
        symbolCode,
        rigIds: new Set(msgRigId ? [msgRigId] : []),
      };
      stationMarkers.set(call, entry);
      pruneAprsEntry(call, entry, mapHistoryCutoffMs());
      if (entry.visibleInHistoryWindow) ensureAprsMarker(call, entry);
      if (aprsMap) scheduleDecodeMapMaintenance();
    }
  };

  function syncSelectedAisTrackVisibility() {
    if (!aprsMap) return;
    const selectedKey = selectedAisTrackMmsi ? String(selectedAisTrackMmsi) : null;
    aisMarkers.forEach((entry, key) => {
      const track = entry?.track;
      if (!track) return;
      const shouldShow = !!selectedKey && selectedKey === String(key) && !!mapFilter.ais;
      const onMap = aprsMap.hasLayer(track);
      if (shouldShow && !onMap) {
        track.addTo(aprsMap);
      }
      if (!shouldShow && onMap) {
        track.removeFrom(aprsMap);
      }
    });
  }

  function getAisAccentColor() {
    return getComputedStyle(document.documentElement).getPropertyValue("--accent-green").trim() || "#c24b1a";
  }

  function aisMarkerOptionsFromMessage(msg) {
    const color = getAisAccentColor();
    return {
      heading: msg?.heading_deg,
      course: msg?.cog_deg,
      speed: msg?.sog_knots,
      color,
      outline: "#00000055",
      size: 22,
    };
  }

  function createAisMarker(lat, lon, msg) {
    if (typeof L !== "undefined" && typeof L.trxAisTrackSymbol === "function") {
      return L.trxAisTrackSymbol([lat, lon], aisMarkerOptionsFromMessage(msg));
    }
    const color = getAisAccentColor();
    return L.circleMarker([lat, lon], {
      radius: 6,
      color,
      fillColor: color,
      fillOpacity: 0.82,
    });
  }

  function updateAisMarker(marker, msg, popupHtml) {
    if (!marker) return;
    marker.setLatLng([msg.lat, msg.lon]);
    if (typeof marker.setAisState === "function") {
      marker.setAisState(aisMarkerOptionsFromMessage(msg));
    }
    if (typeof marker.setStyle === "function" && typeof marker.setAisState !== "function") {
      const color = getAisAccentColor();
      marker.setStyle({
        radius: 6,
        color,
        fillColor: color,
        fillOpacity: 0.84,
      });
    }
    marker.setPopupContent(popupHtml);
  }

  function refreshAisMarkerColors() {
    const color = getAisAccentColor();
    aisMarkers.forEach((entry) => {
      if (entry.marker) {
        if (typeof entry.marker.setAisState === "function") {
          entry.marker.setAisState(aisMarkerOptionsFromMessage(entry.msg || {}));
        } else if (typeof entry.marker.setStyle === "function") {
          entry.marker.setStyle({ color, fillColor: color });
        }
      }
      if (entry.track && typeof entry.track.setStyle === "function") {
        entry.track.setStyle({ color });
      }
    });
  }

  window.aisMapAddVessel = function(msg) {
    if (msg == null || msg.lat == null || msg.lon == null || !Number.isFinite(msg.mmsi)) return;
    const key = String(msg.mmsi);
    const popupHtml = buildAisPopupHtml(msg);
    const nextPoint = [msg.lat, msg.lon];
    const tsMs = Number.isFinite(msg?._tsMs) ? Number(msg._tsMs) : Date.now();
    const msgRigId = msg?.rig_id || T.lastActiveRigId;
    const existing = aisMarkers.get(key);
    if (existing) {
      existing.msg = msg;
      if (msgRigId) {
        if (!existing.rigIds) existing.rigIds = new Set();
        existing.rigIds.add(msgRigId);
      }
      if (!Array.isArray(existing.trackHistory)) existing.trackHistory = [];
      const prevPoint = existing.trackHistory[existing.trackHistory.length - 1];
      if (!aisPositionsEqual(prevPoint, nextPoint)) {
        existing.trackHistory.push({ lat: msg.lat, lon: msg.lon, tsMs });
      } else if (prevPoint) {
        prevPoint.tsMs = tsMs;
      }
      pruneAisEntry(key, existing, mapHistoryCutoffMs());
      if (aprsMap && existing.marker && !T.decodeHistoryReplayActive) {
        updateAisMarker(existing.marker, msg, popupHtml);
      }
      return;
    }
    aisMarkers.set(key, {
      marker: null,
      track: null,
      trackHistory: [{ lat: msg.lat, lon: msg.lon, tsMs }],
      trackPoints: [nextPoint],
      msg,
      rigIds: new Set(msgRigId ? [msgRigId] : []),
    });
    pruneAisEntry(key, aisMarkers.get(key), mapHistoryCutoffMs());
    if (aisMarkers.get(key)?.visibleInHistoryWindow) ensureAisMarker(key, aisMarkers.get(key));
    scheduleDecodeMapMaintenance();
  };

  window.vdesMapAddPoint = function(msg) {
    if (msg == null || msg.lat == null || msg.lon == null) return;
    const key = vdesMarkerKey(msg);
    if (!key) return;
    const popupHtml = buildVdesPopupHtml(msg);
    const visible = Number.isFinite(Number(msg?._tsMs))
      && Number(msg._tsMs) >= mapHistoryCutoffMs();
    const msgRigId = msg?.rig_id || T.lastActiveRigId;
    const existing = vdesMarkers.get(key);
    if (existing) {
      existing.msg = msg;
      existing.visibleInHistoryWindow = visible;
      if (msgRigId) {
        if (!existing.rigIds) existing.rigIds = new Set();
        existing.rigIds.add(msgRigId);
      }
      if (!visible) {
        if (!T.decodeHistoryMapRenderingDeferred()) {
          setRetainedMapMarkerVisible(existing.marker, false);
        } else {
          T.markDecodeMapSyncPending();
        }
        return;
      }
      if (!T.decodeHistoryMapRenderingDeferred()) {
        ensureVdesMarker(key, existing);
        setRetainedMapMarkerVisible(existing.marker, true);
      } else {
        T.markDecodeMapSyncPending();
      }
      if (aprsMap && existing.marker && !T.decodeHistoryReplayActive) {
        existing.marker.setLatLng([msg.lat, msg.lon]);
        existing.marker.setPopupContent(popupHtml);
      }
      return;
    }
    const entry = {
      marker: null,
      msg,
      visibleInHistoryWindow: visible,
      rigIds: new Set(msgRigId ? [msgRigId] : []),
    };
    vdesMarkers.set(key, entry);
    if (!visible) return;
    if (!T.decodeHistoryMapRenderingDeferred()) {
      ensureVdesMarker(key, entry);
      setRetainedMapMarkerVisible(entry.marker, true);
    } else {
      T.markDecodeMapSyncPending();
    }
    if (aprsMap && entry.marker && !T.decodeHistoryReplayActive) {
      entry.marker.setPopupContent(popupHtml);
    }
    scheduleDecodeMapMaintenance();
  };

  let reverseGeocodeLastKey = null;
  function reverseGeocodeLocation(lat, lon, grid) {
    const key = `${lat.toFixed(4)},${lon.toFixed(4)}`;
    if (key === reverseGeocodeLastKey) return;
    reverseGeocodeLastKey = key;
    const url = `https://nominatim.openstreetmap.org/reverse?lat=${encodeURIComponent(lat)}&lon=${encodeURIComponent(lon)}&format=json&zoom=10&accept-language=en`;
    fetch(url, { headers: { "User-Agent": "trx-rs" } })
      .then((r) => r.ok ? r.json() : Promise.reject(r.status))
      .then((data) => {
        const addr = data?.address;
        if (!addr) return;
        const city = addr.city || addr.town || addr.village || addr.hamlet || addr.municipality || addr.county || "";
        const country = addr.country || "";
        if (!city && !country) return;
        const label = city && country ? `${city}, ${country}` : (city || country);
        T.lastCityLabel = label;
        if (T.locationSubtitle) {
          T.locationSubtitle.textContent = `Location: ${grid} · ${label}`;
        }
        T.updateDocumentTitle(T.activeChannelRds());
      })
      .catch(() => {});
  }


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
    const sourceKeys = Object.keys(DEFAULT_MAP_SOURCE_FILTER);
    const noneSelected = sourceKeys.every((k) => !mapFilter[k]);
    mapMarkers.forEach((marker) => {
      const type = marker.__trxType;
      const sourceVisible = noneSelected
        ? DEFAULT_MAP_SOURCE_FILTER[type] !== undefined ? DEFAULT_MAP_SOURCE_FILTER[type] : true
        : !!mapFilter[type];
      const rigVisible = !mapRigFilter
        || marker.__trxType === "bookmark"
        || (marker.__trxRigIds instanceof Set && marker.__trxRigIds.has(mapRigFilter));
      const visible = marker.__trxHistoryVisible !== false
        && markerPassesSearchFilter(marker)
        && markerPassesLocatorFilters(marker)
        && sourceVisible
        && rigVisible;
      const onMap = aprsMap.hasLayer(marker);
      if (visible && !onMap) {
        marker.addTo(aprsMap);
        sendLocatorOverlayToBack(marker);
      }
      if (!visible && onMap) marker.removeFrom(aprsMap);
    });
    syncSelectedAisTrackVisibility();
    syncDecodeContactPathVisibility();
  }

  function updateMapContactPathsToggle() {
    const btn = document.getElementById("map-contact-paths-toggle");
    if (!btn) return;
    btn.textContent = mapDecodeContactPathsEnabled ? "Contact Paths On" : "Contact Paths Off";
    btn.classList.toggle("is-active", mapDecodeContactPathsEnabled);
  }

  function updateMapP2pPathsToggle() {
    const btn = document.getElementById("map-p2p-paths-toggle");
    if (!btn) return;
    btn.textContent = mapP2pRadioPathsEnabled ? "TRX Paths On" : "TRX Paths Off";
    btn.classList.toggle("is-active", mapP2pRadioPathsEnabled);
  }

  function scheduleDecodeMapMaintenance() {
    if (T.decodeHistoryMapRenderingDeferred()) {
      T.markDecodeMapSyncPending();
      return;
    }
    scheduleUiFrameJob("decode-map-maintenance", () => {
      rebuildDecodeContactPaths();
      rebuildMapLocatorFilters();
      applyMapFilter();
    });
  }

  function escapeMapHtml(input) {
    return String(input)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll("\"", "&quot;");
  }

  function formatDecodeLocatorTime(tsMs) {
    if (!Number.isFinite(tsMs)) return "--:--:--";
    return new Date(tsMs).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }

  function formatMapPopupFreq(hz) {
    if (!Number.isFinite(hz)) return "--";
    const value = Number(hz);
    if (value >= 1_000_000_000) return `${(value / 1_000_000_000).toFixed(6).replace(/\.?0+$/, "")} GHz`;
    if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(6).replace(/\.?0+$/, "")} MHz`;
    if (value >= 1_000) return `${(value / 1_000).toFixed(3).replace(/\.?0+$/, "")} kHz`;
    return `${Math.round(value)} Hz`;
  }

  function buildDecodeLocatorTooltipHtml(grid, entry, type) {
    const details = entry?.stationDetails instanceof Map
      ? Array.from(entry.stationDetails.values())
      : [];
    details.sort((a, b) => Number(b?.ts_ms || 0) - Number(a?.ts_ms || 0));
    const title = type === "wspr" ? "WSPR" : "FT8";
    const rows = details
      .map((detail) => {
        const station = escapeMapHtml(String(detail?.source || detail?.station || detail?.target || "Unknown"));
        const freq = formatMapPopupFreq(Number(detail?.freq_hz));
        const meta = [
          detail?.target ? `to ${escapeMapHtml(String(detail.target))}` : null,
          Number.isFinite(detail?.snr_db) ? `${Number(detail.snr_db).toFixed(1)} dB` : null,
          Number.isFinite(detail?.dt_s) ? `dt ${Number(detail.dt_s).toFixed(2)}` : null,
          escapeMapHtml(freq),
        ].filter(Boolean).join(" · ");
        const remoteIds = detail?.remotes instanceof Set && detail.remotes.size > 0
          ? Array.from(detail.remotes)
          : (detail?.remote ? [detail.remote] : []);
        const rxHtml = remoteIds
          .map(rid => {
            const label = _receiverLabel(rid);
            return label ? `<div class="decode-locator-tip-rx">${escapeMapHtml(label)}</div>` : "";
          })
          .filter(Boolean)
          .join("");
        const message = detail?.message
          ? `<div class="decode-locator-tip-note">${escapeMapHtml(String(detail.message))}</div>`
          : "";
        return `<div class="decode-locator-tip-row">` +
          `<div class="decode-locator-tip-head">` +
            `<span class="decode-locator-tip-name">${station}</span>` +
            `<span class="decode-locator-tip-time">${escapeMapHtml(formatDecodeLocatorTime(Number(detail?.ts_ms)))}</span>` +
          `</div>` +
          (meta ? `<div class="decode-locator-tip-meta">${meta}</div>` : "") +
          rxHtml +
          message +
        `</div>`;
      })
      .join("");
    const count = Math.max(
      1,
      details.length,
      entry?.stations instanceof Set ? entry.stations.size : 0,
    );
    return `<div class="decode-locator-tip">` +
      `<div class="decode-locator-tip-title">${escapeMapHtml(grid)}</div>` +
      `<div class="decode-locator-tip-subtitle">${title} · ${count} station${count === 1 ? "" : "s"}</div>` +
      rows +
    `</div>`;
  }

  function rebuildDecodeContactPaths() {
    clearDecodeContactPaths();
    const stationLocators = new Map();
    const directedMessages = [];
    for (const entry of locatorMarkers.values()) {
      if (!entry || (entry.sourceType !== "ft8" && entry.sourceType !== "ft4" && entry.sourceType !== "ft2" && entry.sourceType !== "wspr")) continue;
      const grid = String(entry.grid || "").trim().toUpperCase();
      if (!grid || !(entry.stationDetails instanceof Map)) continue;
      for (const detail of entry.stationDetails.values()) {
        const source = String(detail?.source || detail?.station || "").trim().toUpperCase();
        const target = String(detail?.target || "").trim().toUpperCase();
        const tsMs = Number.isFinite(detail?.ts_ms) ? Number(detail.ts_ms) : 0;
        if (source) {
          const prev = stationLocators.get(source);
          if (!prev || tsMs >= prev.tsMs) {
            stationLocators.set(source, { grid, tsMs });
          }
        }
        if (source && target && source !== target) {
          const band = bandForHz(Number(detail?.freq_hz));
          directedMessages.push({
            source,
            target,
            sourceGrid: grid,
            sourceType: entry.sourceType,
            tsMs,
            bandLabel: band?.label || null,
            remote: detail?.remote || null,
          });
        }
      }
    }
    for (const msg of directedMessages) {
      const targetLocator = stationLocators.get(msg.target);
      if (!targetLocator) continue;
      if (msg.sourceGrid === targetLocator.grid) continue;
      const sourceCenter = locatorToLatLon(msg.sourceGrid);
      const targetCenter = locatorToLatLon(targetLocator.grid);
      if (!sourceCenter || !targetCenter) continue;
      const distanceKm = haversineKm(sourceCenter.lat, sourceCenter.lon, targetCenter.lat, targetCenter.lon);
      const key = [msg.source, msg.target].sort().join("::");
      const prev = decodeContactPaths.get(key);
      if (prev && prev.tsMs > msg.tsMs) continue;
      decodeContactPaths.set(key, {
        pathKey: key,
        source: msg.source,
        target: msg.target,
        sourceGrid: msg.sourceGrid,
        targetGrid: targetLocator.grid,
        sourceType: msg.sourceType,
        bandLabel: msg.bandLabel,
        from: sourceCenter,
        to: targetCenter,
        tsMs: msg.tsMs,
        distanceKm,
        distanceText: formatDecodeContactDistance(distanceKm),
        line: null,
        labelMarker: null,
        remote: msg.remote,
      });
    }
    syncDecodeContactPathVisibility();
  }

  function _receiverLabel(rigId) {
    if (!rigId) return null;
    const rig = T.serverRigs.find(r => r.remote === rigId);
    const name = T.lastRigDisplayNames[rigId] || rigId;
    if (rig && rig.latitude != null && rig.longitude != null) {
      const grid = latLonToMaidenhead(rig.latitude, rig.longitude);
      return `${name} (${grid})`;
    }
    return name;
  }

  function _locatorEntryVisibleOnMap(entry) {
    return entry?.marker && aprsMap && aprsMap.hasLayer(entry.marker);
  }

  function _detailPassesRigFilter(detail) {
    if (!mapRigFilter) return true;
    if (detail?.remotes instanceof Set) return detail.remotes.has(mapRigFilter);
    return detail?.remote === mapRigFilter;
  }

  function renderMapQsoSummary() {
    const listEl = document.getElementById("map-qso-summary-list");
    if (!listEl) return;

    const cutoff = _statsHistoryCutoffMs();
    const entries = Array.from(decodeContactPaths.values())
      .filter((entry) => entry
        && Number.isFinite(entry.distanceKm)
        && _statsDetailPassesRigFilter(entry)
        && (!entry.tsMs || entry.tsMs >= cutoff))
      .sort((a, b) => {
        const distanceDelta = Number(b.distanceKm) - Number(a.distanceKm);
        if (Math.abs(distanceDelta) > 0.001) return distanceDelta;
        return Number(b.tsMs || 0) - Number(a.tsMs || 0);
      })
      .slice(0, MAP_QSO_SUMMARY_LIMIT);

    if (selectedMapQsoKey && !entries.some((entry) => entry.pathKey === selectedMapQsoKey)) {
      selectedMapQsoKey = null;
    }

    if (entries.length === 0) {
      const empty = document.createElement("div");
      empty.className = "map-qso-summary-empty";
      empty.textContent = "No directed FT8 or WSPR contacts match the current map history and filters.";
      listEl.replaceChildren(empty);
      return;
    }

    const fragment = document.createDocumentFragment();
    entries.forEach((entry, index) => {
      const card = document.createElement("button");
      card.type = "button";
      card.className = "map-qso-card";
      card.classList.toggle("is-selected", entry.pathKey === selectedMapQsoKey);
      card.setAttribute("aria-pressed", entry.pathKey === selectedMapQsoKey ? "true" : "false");
      card.addEventListener("click", () => {
        selectedMapQsoKey = selectedMapQsoKey === entry.pathKey ? null : entry.pathKey;
        syncDecodeContactPathVisibility();
        if (selectedMapQsoKey && entry.sourceGrid) {
          navigateToMapLocator(entry.sourceGrid, entry.sourceType);
        }
      });

      const head = document.createElement("div");
      head.className = "map-qso-card-head";

      const rank = document.createElement("span");
      rank.className = "map-qso-card-rank";
      rank.textContent = `#${index + 1}`;
      head.appendChild(rank);

      const distance = document.createElement("span");
      distance.className = "map-qso-card-distance";
      distance.textContent = entry.distanceText || "--";
      head.appendChild(distance);

      const body = document.createElement("div");
      body.className = "map-qso-card-body";

      const pair = document.createElement("div");
      pair.className = "map-qso-card-pair";
      pair.textContent = `${entry.source || "Unknown"} <-> ${entry.target || "Unknown"}`;
      body.appendChild(pair);

      const meta = document.createElement("div");
      meta.className = "map-qso-card-meta";

      const sourceType = document.createElement("span");
      sourceType.className = "map-qso-card-pill";
      sourceType.textContent = String(entry.sourceType || "ft8").toUpperCase();
      meta.appendChild(sourceType);

      if (entry.bandLabel) {
        const band = document.createElement("span");
        band.className = "map-qso-card-pill map-qso-card-band";
        band.style.setProperty("--band-color", locatorBandChipColor(entry.bandLabel));
        band.textContent = entry.bandLabel;
        meta.appendChild(band);
      }

      const ageText = formatTimeAgo(Number(entry.tsMs));
      if (ageText) {
        const age = document.createElement("span");
        age.className = "map-qso-card-pill";
        age.textContent = ageText;
        meta.appendChild(age);
      }

      const rxLabel = _receiverLabel(entry.remote);
      if (rxLabel) {
        const rx = document.createElement("span");
        rx.className = "map-qso-card-pill map-qso-card-rx";
        rx.textContent = rxLabel;
        meta.appendChild(rx);
      }

      body.appendChild(meta);

      const grids = document.createElement("div");
      grids.className = "map-qso-card-grids";
      grids.textContent = `${entry.sourceGrid || "--"} <-> ${entry.targetGrid || "--"}`;
      body.appendChild(grids);

      card.appendChild(head);
      card.appendChild(body);
      fragment.appendChild(card);
    });

    listEl.replaceChildren(fragment);
  }

  function renderMapSignalSummary() {
    const listEl = document.getElementById("map-signal-summary-list");
    if (!listEl) return;

    const cutoff = _statsHistoryCutoffMs();
    const bestByStation = new Map();
    for (const entry of locatorMarkers.values()) {
      if (!entry || (entry.sourceType !== "ft8" && entry.sourceType !== "ft4" && entry.sourceType !== "ft2" && entry.sourceType !== "wspr")) continue;
      if (!(entry.stationDetails instanceof Map)) continue;
      for (const detail of entry.stationDetails.values()) {
        if (!Number.isFinite(detail?.snr_db)) continue;
        if (!_statsDetailPassesRigFilter(detail)) continue;
        if (detail.ts_ms && detail.ts_ms < cutoff) continue;
        const station = String(detail?.source || detail?.station || "").trim().toUpperCase();
        if (!station) continue;
        const snrDb = Number(detail.snr_db);
        const tsMs = Number.isFinite(detail?.ts_ms) ? Number(detail.ts_ms) : 0;
        const prev = bestByStation.get(station);
        if (!prev || snrDb > prev.snrDb || (snrDb === prev.snrDb && tsMs > prev.tsMs)) {
          bestByStation.set(station, {
            station,
            snrDb,
            tsMs,
            grid: entry.grid,
            sourceType: entry.sourceType,
            bandLabel: bandForHz(Number(detail?.freq_hz))?.label || null,
            remote: detail?.remote || null,
          });
        }
      }
    }

    const entries = Array.from(bestByStation.values())
      .sort((a, b) => {
        const delta = b.snrDb - a.snrDb;
        if (Math.abs(delta) > 0.001) return delta;
        return b.tsMs - a.tsMs;
      })
      .slice(0, MAP_QSO_SUMMARY_LIMIT);

    if (entries.length === 0) {
      const empty = document.createElement("div");
      empty.className = "map-qso-summary-empty";
      empty.textContent = "No decoded signals with SNR data in the current map history.";
      listEl.replaceChildren(empty);
      return;
    }

    const fragment = document.createDocumentFragment();
    entries.forEach((entry, index) => {
      const card = document.createElement("button");
      card.type = "button";
      card.className = "map-qso-card";
      if (entry.grid) {
        card.addEventListener("click", () => {
          navigateToMapLocator(entry.grid, entry.sourceType);
        });
      }

      const head = document.createElement("div");
      head.className = "map-qso-card-head";

      const rank = document.createElement("span");
      rank.className = "map-qso-card-rank";
      rank.textContent = `#${index + 1}`;
      head.appendChild(rank);

      const snr = document.createElement("span");
      snr.className = "map-qso-card-distance";
      snr.textContent = `${entry.snrDb >= 0 ? "+" : ""}${entry.snrDb.toFixed(0)} dB`;
      head.appendChild(snr);

      const body = document.createElement("div");
      body.className = "map-qso-card-body";

      const pair = document.createElement("div");
      pair.className = "map-qso-card-pair";
      pair.textContent = entry.station;
      body.appendChild(pair);

      const meta = document.createElement("div");
      meta.className = "map-qso-card-meta";

      const sourceType = document.createElement("span");
      sourceType.className = "map-qso-card-pill";
      sourceType.textContent = String(entry.sourceType || "ft8").toUpperCase();
      meta.appendChild(sourceType);

      if (entry.bandLabel) {
        const band = document.createElement("span");
        band.className = "map-qso-card-pill map-qso-card-band";
        band.style.setProperty("--band-color", locatorBandChipColor(entry.bandLabel));
        band.textContent = entry.bandLabel;
        meta.appendChild(band);
      }

      const ageText = formatTimeAgo(Number(entry.tsMs));
      if (ageText) {
        const age = document.createElement("span");
        age.className = "map-qso-card-pill";
        age.textContent = ageText;
        meta.appendChild(age);
      }

      const rxLabel = _receiverLabel(entry.remote);
      if (rxLabel) {
        const rx = document.createElement("span");
        rx.className = "map-qso-card-pill map-qso-card-rx";
        rx.textContent = rxLabel;
        meta.appendChild(rx);
      }

      body.appendChild(meta);

      const grids = document.createElement("div");
      grids.className = "map-qso-card-grids";
      grids.textContent = entry.grid || "--";
      body.appendChild(grids);

      card.appendChild(head);
      card.appendChild(body);
      fragment.appendChild(card);
    });
    listEl.replaceChildren(fragment);
  }

  function renderMapWeakSignalSummary() {
    const listEl = document.getElementById("map-weak-signal-summary-list");
    if (!listEl) return;

    const cutoff = _statsHistoryCutoffMs();
    const worstByStation = new Map();
    for (const entry of locatorMarkers.values()) {
      if (!entry || (entry.sourceType !== "ft8" && entry.sourceType !== "ft4" && entry.sourceType !== "ft2" && entry.sourceType !== "wspr")) continue;
      if (!(entry.stationDetails instanceof Map)) continue;
      for (const detail of entry.stationDetails.values()) {
        if (!Number.isFinite(detail?.snr_db)) continue;
        if (!_statsDetailPassesRigFilter(detail)) continue;
        if (detail.ts_ms && detail.ts_ms < cutoff) continue;
        const station = String(detail?.source || detail?.station || "").trim().toUpperCase();
        if (!station) continue;
        const snrDb = Number(detail.snr_db);
        const tsMs = Number.isFinite(detail?.ts_ms) ? Number(detail.ts_ms) : 0;
        const prev = worstByStation.get(station);
        if (!prev || snrDb < prev.snrDb || (snrDb === prev.snrDb && tsMs > prev.tsMs)) {
          worstByStation.set(station, {
            station,
            snrDb,
            tsMs,
            grid: entry.grid,
            sourceType: entry.sourceType,
            bandLabel: bandForHz(Number(detail?.freq_hz))?.label || null,
            remote: detail?.remote || null,
          });
        }
      }
    }

    const entries = Array.from(worstByStation.values())
      .sort((a, b) => {
        const delta = a.snrDb - b.snrDb;
        if (Math.abs(delta) > 0.001) return delta;
        return b.tsMs - a.tsMs;
      })
      .slice(0, MAP_QSO_SUMMARY_LIMIT);

    if (entries.length === 0) {
      const empty = document.createElement("div");
      empty.className = "map-qso-summary-empty";
      empty.textContent = "No decoded signals with SNR data in the current map history.";
      listEl.replaceChildren(empty);
      return;
    }

    const fragment = document.createDocumentFragment();
    entries.forEach((entry, index) => {
      const card = document.createElement("button");
      card.type = "button";
      card.className = "map-qso-card";
      if (entry.grid) {
        card.addEventListener("click", () => {
          navigateToMapLocator(entry.grid, entry.sourceType);
        });
      }

      const head = document.createElement("div");
      head.className = "map-qso-card-head";

      const rank = document.createElement("span");
      rank.className = "map-qso-card-rank";
      rank.textContent = `#${index + 1}`;
      head.appendChild(rank);

      const snr = document.createElement("span");
      snr.className = "map-qso-card-distance";
      snr.textContent = `${entry.snrDb >= 0 ? "+" : ""}${entry.snrDb.toFixed(0)} dB`;
      head.appendChild(snr);

      const body = document.createElement("div");
      body.className = "map-qso-card-body";

      const pair = document.createElement("div");
      pair.className = "map-qso-card-pair";
      pair.textContent = entry.station;
      body.appendChild(pair);

      const meta = document.createElement("div");
      meta.className = "map-qso-card-meta";

      const sourceType = document.createElement("span");
      sourceType.className = "map-qso-card-pill";
      sourceType.textContent = String(entry.sourceType || "ft8").toUpperCase();
      meta.appendChild(sourceType);

      if (entry.bandLabel) {
        const band = document.createElement("span");
        band.className = "map-qso-card-pill map-qso-card-band";
        band.style.setProperty("--band-color", locatorBandChipColor(entry.bandLabel));
        band.textContent = entry.bandLabel;
        meta.appendChild(band);
      }

      const ageText = formatTimeAgo(Number(entry.tsMs));
      if (ageText) {
        const age = document.createElement("span");
        age.className = "map-qso-card-pill";
        age.textContent = ageText;
        meta.appendChild(age);
      }

      const rxLabel = _receiverLabel(entry.remote);
      if (rxLabel) {
        const rx = document.createElement("span");
        rx.className = "map-qso-card-pill map-qso-card-rx";
        rx.textContent = rxLabel;
        meta.appendChild(rx);
      }

      body.appendChild(meta);

      const grids = document.createElement("div");
      grids.className = "map-qso-card-grids";
      grids.textContent = entry.grid || "--";
      body.appendChild(grids);

      card.appendChild(head);
      card.appendChild(body);
      fragment.appendChild(card);
    });
    listEl.replaceChildren(fragment);
  }

  // ── Statistics panel ─────────────────────────────────────────────────
  let statsRigFilter = "";
  let statsHistoryLimitMinutes = 1440;
  const statsDecodeLog = []; // {type, ts_ms, remote}
  const STATS_LOG_MAX = 50000;
  const STATS_TYPE_COLORS = {
    ft8: "#4fc3f7", ft4: "#81c784", ft2: "#aed581", wspr: "#ffb74d",
    aprs: "#ce93d8", hf_aprs: "#ba68c8", ais: "#90a4ae", vdes: "#78909c",
    cw: "#fff176",
  };
  const STATS_DX_BUCKETS = [
    { label: "0–500 km", min: 0, max: 500 },
    { label: "500–1k", min: 500, max: 1000 },
    { label: "1k–2k", min: 1000, max: 2000 },
    { label: "2k–5k", min: 2000, max: 5000 },
    { label: "5k–10k", min: 5000, max: 10000 },
    { label: "10k+ km", min: 10000, max: Infinity },
  ];

  function _statsHistoryCutoffMs() {
    return Date.now() - (statsHistoryLimitMinutes * 60 * 1000);
  }

  function _statsDetailPassesRigFilter(detail) {
    if (!statsRigFilter) return true;
    if (detail?.remotes instanceof Set) return detail.remotes.has(statsRigFilter);
    return detail?.remote === statsRigFilter;
  }

  function updateStatsRigFilter() {
    const el = document.getElementById("stats-rig-filter");
    if (!el) return;
    const prev = el.value;
    while (el.options.length > 1) el.remove(1);
    for (const id of T.lastRigIds) {
      const opt = document.createElement("option");
      opt.value = id;
      opt.textContent = T.lastRigDisplayNames[id] || id;
      el.appendChild(opt);
    }
    if (prev && T.lastRigIds.includes(prev)) {
      el.value = prev;
    } else {
      el.value = "";
      statsRigFilter = "";
    }
  }

  function statsRecordDecode(type, remote, tsMs) {
    statsDecodeLog.push({ type: String(type || "unknown"), ts_ms: tsMs || Date.now(), remote: remote || null });
    if (statsDecodeLog.length > STATS_LOG_MAX) {
      statsDecodeLog.splice(0, statsDecodeLog.length - STATS_LOG_MAX);
    }
  }

  function _statsFilteredLog() {
    const cutoff = _statsHistoryCutoffMs();
    return statsDecodeLog.filter((e) => {
      if (e.ts_ms < cutoff) return false;
      if (statsRigFilter && e.remote && e.remote !== statsRigFilter) return false;
      return true;
    });
  }

  function renderStatsCounters() {
    const cutoff = _statsHistoryCutoffMs();
    const log = _statsFilteredLog();
    const totalDecodes = log.length;

    const uniqueStations = new Set();
    const uniqueGrids = new Set();
    for (const entry of locatorMarkers.values()) {
      if (!entry || !(entry.stationDetails instanceof Map)) continue;
      for (const detail of entry.stationDetails.values()) {
        if (detail?.ts_ms && detail.ts_ms < cutoff) continue;
        if (!_statsDetailPassesRigFilter(detail)) continue;
        const station = String(detail?.source || detail?.station || "").trim().toUpperCase();
        if (station) uniqueStations.add(station);
      }
      if (entry.grid) {
        const hasVisible = entry.stationDetails instanceof Map && Array.from(entry.stationDetails.values()).some(
          (d) => (!d.ts_ms || d.ts_ms >= cutoff) && _statsDetailPassesRigFilter(d)
        );
        if (hasVisible) uniqueGrids.add(entry.grid);
      }
    }

    // Decode rate: decodes in last 60 seconds → per minute
    const rateWindow = Date.now() - 60000;
    const recentCount = log.filter((e) => e.ts_ms >= rateWindow).length;

    const setEl = (id, val) => {
      const el = document.getElementById(id);
      if (el) el.textContent = String(val);
    };
    setEl("stats-total-decodes", totalDecodes.toLocaleString());
    setEl("stats-unique-stations", uniqueStations.size.toLocaleString());
    setEl("stats-unique-grids", uniqueGrids.size.toLocaleString());
    setEl("stats-decode-rate", recentCount.toLocaleString());
  }

  function _renderBarChart(containerId, data, emptyMsg) {
    const el = document.getElementById(containerId);
    if (!el) return;
    if (!data || data.length === 0 || data.every((d) => d.count === 0)) {
      el.replaceChildren();
      const empty = document.createElement("div");
      empty.className = "stats-bar-empty";
      empty.textContent = emptyMsg || "No data available.";
      el.appendChild(empty);
      return;
    }
    const maxVal = Math.max(1, ...data.map((d) => d.count));
    const fragment = document.createDocumentFragment();
    for (const item of data) {
      const row = document.createElement("div");
      row.className = "stats-bar-row";

      const label = document.createElement("span");
      label.className = "stats-bar-label";
      label.textContent = item.label;
      row.appendChild(label);

      const track = document.createElement("div");
      track.className = "stats-bar-track";
      const fill = document.createElement("div");
      fill.className = "stats-bar-fill";
      fill.style.width = `${(item.count / maxVal) * 100}%`;
      fill.style.background = item.color || "var(--accent-green)";
      track.appendChild(fill);
      row.appendChild(track);

      const count = document.createElement("span");
      count.className = "stats-bar-count";
      count.textContent = item.count.toLocaleString();
      row.appendChild(count);

      fragment.appendChild(row);
    }
    el.replaceChildren(fragment);
  }

  function renderStatsDecodeTypes() {
    const log = _statsFilteredLog();
    const counts = {};
    for (const e of log) {
      counts[e.type] = (counts[e.type] || 0) + 1;
    }
    const data = Object.entries(counts)
      .map(([type, count]) => ({
        label: type.toUpperCase(),
        count,
        color: STATS_TYPE_COLORS[type] || "#aaa",
      }))
      .sort((a, b) => b.count - a.count);
    _renderBarChart("stats-decode-type-bars", data, "No decoded signals in the current history.");
  }

  function renderStatsBandActivity() {
    const cutoff = _statsHistoryCutoffMs();
    const bandCounts = {};
    for (const entry of locatorMarkers.values()) {
      if (!entry || !(entry.stationDetails instanceof Map)) continue;
      for (const detail of entry.stationDetails.values()) {
        if (detail?.ts_ms && detail.ts_ms < cutoff) continue;
        if (!_statsDetailPassesRigFilter(detail)) continue;
        if (!Number.isFinite(detail?.freq_hz)) continue;
        const band = bandForHz(Number(detail.freq_hz));
        if (band) {
          bandCounts[band.label] = (bandCounts[band.label] || 0) + 1;
        }
      }
    }
    const data = Object.entries(bandCounts)
      .map(([label, count]) => ({
        label,
        count,
        color: locatorBandChipColor(label),
      }))
      .sort((a, b) => b.count - a.count);
    _renderBarChart("stats-band-activity-bars", data, "No band activity data in the current history.");
  }

  function renderStatsRigCompare() {
    const section = document.getElementById("stats-rig-compare-section");
    if (!section) return;
    if (T.lastRigIds.length < 2) {
      section.style.display = "none";
      return;
    }
    section.style.display = "";
    const cutoff = _statsHistoryCutoffMs();
    const rigCounts = {};
    for (const e of statsDecodeLog) {
      if (e.ts_ms < cutoff) continue;
      const rid = e.remote || "unknown";
      rigCounts[rid] = (rigCounts[rid] || 0) + 1;
    }
    const data = Object.entries(rigCounts)
      .map(([rid, count]) => ({
        label: T.lastRigDisplayNames[rid] || rid,
        count,
        color: "var(--accent-green)",
      }))
      .sort((a, b) => b.count - a.count);
    _renderBarChart("stats-rig-compare-bars", data, "No decode data per receiver.");
  }

  function renderStatsDxHistogram() {
    const cutoff = _statsHistoryCutoffMs();
    const buckets = STATS_DX_BUCKETS.map((b) => ({ ...b, count: 0 }));
    for (const entry of decodeContactPaths.values()) {
      if (!entry || !Number.isFinite(entry.distanceKm)) continue;
      if (entry.tsMs && entry.tsMs < cutoff) continue;
      if (!_statsDetailPassesRigFilter(entry)) continue;
      const km = entry.distanceKm;
      for (const b of buckets) {
        if (km >= b.min && km < b.max) { b.count++; break; }
      }
    }
    const data = buckets.map((b) => ({
      label: b.label,
      count: b.count,
      color: "#4fc3f7",
    }));
    _renderBarChart("stats-dx-histogram-bars", data, "No directed contact paths in the current history.");
  }

  let _statsRenderPending = false;
  function scheduleStatsRender() {
    if (_statsRenderPending) return;
    _statsRenderPending = true;
    requestAnimationFrame(() => {
      _statsRenderPending = false;
      renderStatsCounters();
      renderStatsDecodeTypes();
      renderStatsBandActivity();
      renderStatsRigCompare();
      renderStatsDxHistogram();
      renderMapQsoSummary();
      renderMapSignalSummary();
      renderMapWeakSignalSummary();
    });
  }

  // Wire up statistics panel controls
  (function() {
    const rigEl = document.getElementById("stats-rig-filter");
    if (rigEl) {
      rigEl.addEventListener("change", () => {
        statsRigFilter = rigEl.value;
        scheduleStatsRender();
      });
    }
    const histEl = document.getElementById("stats-history-limit");
    if (histEl) {
      histEl.value = String(statsHistoryLimitMinutes);
      histEl.addEventListener("change", () => {
        statsHistoryLimitMinutes = Number(histEl.value) || 1440;
        scheduleStatsRender();
      });
    }
  })();

  function buildBookmarkLocatorPopupHtml(grid, bookmarks) {
    const list = Array.isArray(bookmarks) ? bookmarks : [];
    const rows = list
      .map((bm) => {
        const title = escapeMapHtml(String(bm.name || "Bookmark"));
        const freq = typeof bmFmtFreq === "function"
          ? escapeMapHtml(bmFmtFreq(bm.freq_hz))
          : escapeMapHtml(String(bm.freq_hz || "--"));
        const mode = bm.mode ? ` · ${escapeMapHtml(String(bm.mode))}` : "";
        return `${title} <span style="opacity:0.75">${freq}${mode}</span>`;
      })
      .join("<br>");
    return `<b>${escapeMapHtml(grid)}</b><br>Bookmarks: ${list.length || 1}` + (rows ? `<br>${rows}` : "");
  }

  window.syncBookmarkMapLocators = function(bookmarks) {
    const list = Array.isArray(bookmarks) ? bookmarks : [];
    const grouped = new Map();
    for (const bm of list) {
      const grid = String(bm?.locator || "").trim().toUpperCase();
      if (!grid) continue;
      const bounds = maidenheadToBounds(grid);
      if (!bounds) continue;
      const key = `bookmark:${grid}`;
      const bucket = grouped.get(key);
      if (bucket) {
        bucket.bookmarks.push(bm);
      } else {
        grouped.set(key, { grid, bounds, bookmarks: [bm] });
      }
    }

    for (const [key, entry] of locatorMarkers.entries()) {
      if (!key.startsWith("bookmark:")) continue;
      if (!grouped.has(key)) {
        if (entry && entry.marker) {
          if (entry.marker === selectedLocatorMarker) {
            setSelectedLocatorMarker(null);
            clearMapRadioPath();
          }
          if (aprsMap && aprsMap.hasLayer(entry.marker)) entry.marker.removeFrom(aprsMap);
          mapMarkers.delete(entry.marker);
        }
        locatorMarkers.delete(key);
      }
    }

    for (const [key, next] of grouped.entries()) {
      const existing = locatorMarkers.get(key);
      const popupHtml = buildBookmarkLocatorPopupHtml(next.grid, next.bookmarks);
      const bandMeta = collectBandMeta(next.bookmarks.map((bm) => Number(bm?.freq_hz)));
      if (existing) {
        existing.grid = next.grid;
        existing.bounds = next.bounds;
        existing.bookmarks = next.bookmarks;
        existing.sourceType = "bookmark";
        existing.bandMeta = bandMeta;
        if (existing.marker) {
          existing.marker.setBounds(next.bounds);
          existing.marker.setStyle(locatorStyleForEntry(existing, next.bookmarks.length));
          existing.marker.setPopupContent(popupHtml);
          sendLocatorOverlayToBack(existing.marker);
          assignLocatorMarkerMeta(existing.marker, existing.sourceType, existing.bandMeta);
        }
        continue;
      }

      const entry = {
        marker: null,
        grid: next.grid,
        bounds: next.bounds,
        bookmarks: next.bookmarks,
        sourceType: "bookmark",
        bandMeta,
      };
      locatorMarkers.set(key, entry);
      if (aprsMap) {
        entry.marker = L.rectangle(next.bounds, locatorStyleForEntry(entry, next.bookmarks.length))
          .addTo(aprsMap)
          .bindPopup(popupHtml);
        entry.marker.__trxType = "bookmark";
        sendLocatorOverlayToBack(entry.marker);
        assignLocatorMarkerMeta(entry.marker, entry.sourceType, entry.bandMeta);
        mapMarkers.add(entry.marker);
      }
    }

    rebuildMapLocatorFilters();
    applyMapFilter();
  };

  window.mapAddLocator = function(message, grids, type = "ft8", station = null, details = null) {
    if (!Array.isArray(grids) || grids.length === 0) return;
    const markerType = type === "wspr" ? "wspr" : (type === "ft4" ? "ft4" : (type === "ft2" ? "ft2" : "ft8"));
    const msgRigId = details?.rig_id || T.lastActiveRigId;
    const unique = [...new Set(grids.map((g) => String(g).toUpperCase()))];
    const stationId = station && String(station).trim() ? String(station).trim().toUpperCase() : "";
    const locatorDetails = new Map();
    if (Array.isArray(details?.locator_details)) {
      for (const locatorDetail of details.locator_details) {
        const grid = String(locatorDetail?.grid || "").trim().toUpperCase();
        if (!grid) continue;
        locatorDetails.set(grid, locatorDetail);
      }
    }
    for (const grid of unique) {
      const bounds = maidenheadToBounds(grid);
      if (!bounds) continue;
      const locatorDetail = locatorDetails.get(grid);
      const sourceId = locatorDetail?.source && String(locatorDetail.source).trim()
        ? String(locatorDetail.source).trim().toUpperCase()
        : "";
      const targetId = locatorDetail?.target && String(locatorDetail.target).trim()
        ? String(locatorDetail.target).trim().toUpperCase()
        : "";
      const detailStationId = sourceId || stationId;
      const detailEntry = {
        station: detailStationId || null,
        source: sourceId || null,
        target: targetId || null,
        ts_ms: Number.isFinite(details?.ts_ms) ? Number(details.ts_ms) : null,
        snr_db: Number.isFinite(details?.snr_db) ? Number(details.snr_db) : null,
        dt_s: Number.isFinite(details?.dt_s) ? Number(details.dt_s) : null,
        freq_hz: Number.isFinite(details?.freq_hz) ? Number(details.freq_hz) : null,
        message: String(details?.message || message || "").trim() || null,
        remote: msgRigId || null,
        remotes: new Set(msgRigId ? [msgRigId] : []),
      };
      const detailKey = detailStationId || `${targetId || "decode"}:${detailEntry.message || "decode"}:${detailEntry.ts_ms || Date.now()}`;
      const key = `${markerType}:${grid}`;
      const existing = locatorMarkers.get(key);
      if (existing) {
        existing.grid = grid;
        if (!(existing.allStationDetails instanceof Map)) {
          existing.allStationDetails = existing.stationDetails instanceof Map
            ? new Map(existing.stationDetails)
            : new Map();
        }
        const prevDetail = existing.allStationDetails.get(detailKey);
        const mergedRemotes = prevDetail?.remotes instanceof Set ? new Set(prevDetail.remotes) : new Set();
        if (msgRigId) mergedRemotes.add(msgRigId);
        existing.allStationDetails.set(detailKey, { ...detailEntry, remotes: mergedRemotes });
        existing.sourceType = markerType;
        if (msgRigId) {
          if (!existing.rigIds) existing.rigIds = new Set();
          existing.rigIds.add(msgRigId);
        }
        pruneLocatorEntry(key, existing, mapHistoryCutoffMs());
        if (existing.marker) sendLocatorOverlayToBack(existing.marker);
        scheduleDecodeMapMaintenance();
        continue;
      }

      const allStationDetails = new Map();
      allStationDetails.set(detailKey, { ...detailEntry });
      const entry = {
        marker: null,
        grid,
        stations: new Set(),
        stationDetails: new Map(),
        allStationDetails,
        sourceType: markerType,
        bandMeta: new Map(),
        rigIds: new Set(msgRigId ? [msgRigId] : []),
      };
      locatorMarkers.set(key, entry);
      pruneLocatorEntry(key, entry, mapHistoryCutoffMs());
      if (entry.marker) sendLocatorOverlayToBack(entry.marker);
    }
    scheduleDecodeMapMaintenance();
  };

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
    sizeAprsMapToViewport();
  });


  function flushDeferredDecodeMapSync() {
    if (!T.decodeMapSyncPending || T.decodeHistoryReplayActive || !aprsMap) return;
    T.decodeMapSyncPending = false;
    scheduleUiFrameJob("decode-map-maintenance", () => {
      pruneMapHistory();
    });
  }


  // Register module API for core to call
  window.trx.map = {
    initAprsMap,
    sizeAprsMapToViewport,
    syncAprsReceiverMarker,
    updateMapRigFilter,
    updateStatsRigFilter,
    statsRecordDecode,
    scheduleStatsRender,
    get aprsMap() { return aprsMap; },
    get stationMarkers() { return stationMarkers; },
    get locatorMarkers() { return locatorMarkers; },
    get aisMarkers() { return aisMarkers; },
    get vdesMarkers() { return vdesMarkers; },
    get decodeContactPaths() { return decodeContactPaths; },
    pruneMapHistory,
    aprsSymbolIcon,
    buildAprsPopupHtml,
    buildAisPopupHtml,
    buildVdesPopupHtml,
    ensureAprsMarker,
    ensureAisMarker,
    ensureVdesMarker,
    ensureDecodeLocatorMarker,
    aprsPositionsEqual,
    aisPositionsEqual,
    refreshAprsTrack,
    refreshAisTrack,
    updateAisMarker,
    createAisMarker,
    getAisAccentColor,
    refreshAisMarkerColors,
    setMapRadioPathTo,
    buildReceiverPopupHtml,
    rebuildDecodeContactPaths,
    syncDecodeContactPathVisibility,
    scheduleDecodeMapMaintenance,
    renderMapLocatorLegend,
    rebuildMapLocatorFilters,
    renderMapQsoSummary,
    renderMapSignalSummary,
    renderMapWeakSignalSummary,
    vdesMarkerKey,
    aisMarkerOptionsFromMessage,
    materializeBufferedMapLayers,
    flushDeferredDecodeMapSync,
    bandForHz,
  };
})();
