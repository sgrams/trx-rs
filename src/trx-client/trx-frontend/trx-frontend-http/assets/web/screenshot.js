// Spectrum screenshot module (loaded on demand when user triggers screenshot).
// Communicates with app.js core via window.trx namespace.
(function () {
  "use strict";
  const T = window.trx;

  function isVisibleForSnapshot(el) {
    if (!el) return false;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden") return false;
    const opacity = Number(style.opacity);
    if (Number.isFinite(opacity) && opacity <= 0) return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  }

  function drawRoundedRectPath(ctx, x, y, w, h, r) {
    const radius = Math.max(0, Math.min(r, Math.min(w, h) / 2));
    ctx.beginPath();
    ctx.moveTo(x + radius, y);
    ctx.lineTo(x + w - radius, y);
    ctx.quadraticCurveTo(x + w, y, x + w, y + radius);
    ctx.lineTo(x + w, y + h - radius);
    ctx.quadraticCurveTo(x + w, y + h, x + w - radius, y + h);
    ctx.lineTo(x + radius, y + h);
    ctx.quadraticCurveTo(x, y + h, x, y + h - radius);
    ctx.lineTo(x, y + radius);
    ctx.quadraticCurveTo(x, y, x + radius, y);
    ctx.closePath();
  }

  function drawElementChrome(ctx, el, rootRect, maxAlpha = 1) {
    if (!isVisibleForSnapshot(el)) return null;
    const rect = el.getBoundingClientRect();
    const style = getComputedStyle(el);
    const x = rect.left - rootRect.left;
    const y = rect.top - rootRect.top;
    const w = rect.width;
    const h = rect.height;
    const radius = parseFloat(style.borderTopLeftRadius) || 0;
    const bg = T.cssColorToRgba(style.backgroundColor || "rgba(0,0,0,0)");
    const borderWidth = Math.max(0, parseFloat(style.borderTopWidth) || 0);
    const border = T.cssColorToRgba(style.borderTopColor || "rgba(0,0,0,0)");

    const bgAlpha = Math.min(bg[3], maxAlpha);
    if (bgAlpha > 0.01) {
      drawRoundedRectPath(ctx, x, y, w, h, radius);
      ctx.fillStyle = `rgba(${Math.round(bg[0])}, ${Math.round(bg[1])}, ${Math.round(bg[2])}, ${bgAlpha})`;
      ctx.fill();
    }
    const borderAlpha = Math.min(border[3], maxAlpha);
    if (borderWidth > 0 && borderAlpha > 0.01) {
      drawRoundedRectPath(ctx, x + borderWidth * 0.5, y + borderWidth * 0.5, w - borderWidth, h - borderWidth, Math.max(0, radius - borderWidth * 0.5));
      ctx.lineWidth = borderWidth;
      ctx.strokeStyle = `rgba(${Math.round(border[0])}, ${Math.round(border[1])}, ${Math.round(border[2])}, ${borderAlpha})`;
      ctx.stroke();
    }
    return { x, y, w, h, style };
  }

  function drawWrappedText(ctx, text, x, y, maxWidth, lineHeight, maxLines) {
    const words = String(text || "").split(/\s+/).filter(Boolean);
    if (!words.length) return;
    let line = "";
    let lineIdx = 0;
    for (let i = 0; i < words.length; i += 1) {
      const candidate = line ? `${line} ${words[i]}` : words[i];
      if (ctx.measureText(candidate).width <= maxWidth || !line) {
        line = candidate;
        continue;
      }
      ctx.fillText(line, x, y + lineIdx * lineHeight);
      lineIdx += 1;
      if (lineIdx >= maxLines) return;
      line = words[i];
    }
    if (line && lineIdx < maxLines) {
      ctx.fillText(line, x, y + lineIdx * lineHeight);
    }
  }

  function drawElementTextBlock(ctx, el, rootRect, fallbackText = null, maxAlpha = 1) {
    const chrome = drawElementChrome(ctx, el, rootRect, maxAlpha);
    if (!chrome) return;
    const text = (fallbackText == null ? el.innerText : fallbackText) || "";
    const clean = text.replace(/\s+\n/g, "\n").replace(/\n\s+/g, "\n").trim();
    if (!clean) return;
    const style = chrome.style;
    const fontSize = parseFloat(style.fontSize) || 12;
    const lineHeight = (parseFloat(style.lineHeight) || fontSize * 1.25);
    const padX = 6;
    const padY = 4;
    const maxWidth = Math.max(20, chrome.w - padX * 2);
    const maxLines = Math.max(1, Math.floor((chrome.h - padY * 2) / lineHeight));
    ctx.fillStyle = style.color || "#ffffff";
    ctx.font = `${style.fontStyle || "normal"} ${style.fontWeight || "400"} ${style.fontSize || "12px"} ${style.fontFamily || "sans-serif"}`;
    ctx.textBaseline = "top";
    const lines = clean.split(/\n+/);
    let lineCursor = 0;
    for (const line of lines) {
      if (lineCursor >= maxLines) break;
      drawWrappedText(
        ctx,
        line,
        chrome.x + padX,
        chrome.y + padY + lineCursor * lineHeight,
        maxWidth,
        lineHeight,
        maxLines - lineCursor,
      );
      lineCursor += 1;
    }
  }

  function drawAxisLabels(ctx, axisEl, rootRect) {
    if (!isVisibleForSnapshot(axisEl)) return;
    for (const node of axisEl.children) {
      if (!(node instanceof HTMLElement)) continue;
      if (!(node.matches("span") || node.matches("button"))) continue;
      if (!isVisibleForSnapshot(node)) continue;
      const chrome = drawElementChrome(ctx, node, rootRect);
      const text = (node.textContent || "").trim();
      if (!chrome || !text) continue;
      const style = chrome.style;
      ctx.fillStyle = style.color || "#ffffff";
      ctx.font = `${style.fontStyle || "normal"} ${style.fontWeight || "400"} ${style.fontSize || "12px"} ${style.fontFamily || "sans-serif"}`;
      ctx.textBaseline = "middle";
      ctx.fillText(text, chrome.x + 4, chrome.y + chrome.h / 2);
    }
  }

  function buildSpectrumSnapshotCanvas() {
    const rootEl = document.querySelector(".signal-visual-block");
    const spectrumPanelEl = document.getElementById("spectrum-panel");
    if (!rootEl || !isVisibleForSnapshot(rootEl) || !isVisibleForSnapshot(spectrumPanelEl)) {
      return null;
    }
    for (const renderer of [T.overviewGl, T.spectrumGl, T.signalOverlayGl]) {
      const gl = renderer?.gl;
      if (!gl) continue;
      try {
        if (typeof gl.flush === "function") gl.flush();
        if (typeof gl.finish === "function") gl.finish();
      } catch (_) {
        // Ignore transient WebGL state errors and capture the last good frame.
      }
    }
    const rootRect = rootEl.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const out = document.createElement("canvas");
    out.width = Math.max(1, Math.round(rootRect.width * dpr));
    out.height = Math.max(1, Math.round(rootRect.height * dpr));
    const ctx = out.getContext("2d");
    if (!ctx) return null;
    ctx.scale(dpr, dpr);

    const bg = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim() || getComputedStyle(document.body).backgroundColor || "#000";
    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, rootRect.width, rootRect.height);

    const signalOverlayCanvas = document.getElementById("signal-overlay-canvas");
    const canvases = [T.overviewCanvas, T.spectrumCanvas, signalOverlayCanvas];
    for (const canvas of canvases) {
      if (!canvas || !isVisibleForSnapshot(canvas)) continue;
      const rect = canvas.getBoundingClientRect();
      ctx.drawImage(
        canvas,
        rect.left - rootRect.left,
        rect.top - rootRect.top,
        rect.width,
        rect.height,
      );
    }

    // Decoder overlays over the signal view.
    // Cap background alpha to avoid opaque blocks (backdrop-filter can't be
    // replicated on canvas, so frosted-glass overlays would otherwise obscure
    // the spectrum).
    const decoderOverlayIds = [
      "ais-bar-overlay",
      "vdes-bar-overlay",
      "ft8-bar-overlay",
      "aprs-bar-overlay",
      "rds-ps-overlay",
    ];
    for (const id of decoderOverlayIds) {
      const overlayEl = document.getElementById(id);
      if (!overlayEl || !isVisibleForSnapshot(overlayEl)) continue;
      drawElementTextBlock(ctx, overlayEl, rootRect, null, 0.35);
    }

    // Spectrum axis labels and bookmark chips (includes freq bar).
    const spectrumFreqAxis = document.getElementById("spectrum-freq-axis");
    const spectrumDbAxis = document.getElementById("spectrum-db-axis");
    drawAxisLabels(ctx, spectrumFreqAxis, rootRect);
    drawAxisLabels(ctx, spectrumDbAxis, rootRect);
    drawAxisLabels(ctx, document.getElementById("spectrum-bookmark-axis"), rootRect);
    drawAxisLabels(ctx, document.getElementById("spectrum-bookmark-side-left"), rootRect);
    drawAxisLabels(ctx, document.getElementById("spectrum-bookmark-side-right"), rootRect);

    return out;
  }

  function clickCanvasDownload(href, fileName) {
    const a = document.createElement("a");
    a.href = href;
    a.download = fileName;
    a.rel = "noopener";
    a.style.display = "none";
    document.body.appendChild(a);
    a.click();
    requestAnimationFrame(() => a.remove());
  }

  function saveCanvasAsPng(canvas, fileName) {
    if (!canvas) return Promise.resolve(false);
    if (typeof canvas.toBlob === "function") {
      return new Promise((resolve) => {
        try {
          canvas.toBlob((blob) => {
            if (!blob) {
              resolve(false);
              return;
            }
            const url = URL.createObjectURL(blob);
            clickCanvasDownload(url, fileName);
            setTimeout(() => URL.revokeObjectURL(url), 1000);
            resolve(true);
          }, "image/png");
        } catch (_) {
          resolve(false);
        }
      });
    }
    try {
      clickCanvasDownload(canvas.toDataURL("image/png"), fileName);
      return Promise.resolve(true);
    } catch (_) {
      return Promise.resolve(false);
    }
  }

  async function captureSpectrumScreenshot() {
    const snapshotCanvas = buildSpectrumSnapshotCanvas();
    if (!snapshotCanvas) {
      T.showHint("Spectrum view not ready", 1300);
      return false;
    }
    const stamp = new Date().toISOString().replace(/[:.]/g, "-");
    const saved = await saveCanvasAsPng(snapshotCanvas, `trx-spectrum-${stamp}.png`);
    T.showHint(saved ? "Spectrum screenshot saved" : "Spectrum screenshot failed", saved ? 1500 : 1800);
    return saved;
  }

  // Register module API
  window.trx.screenshot = {
    captureSpectrumScreenshot,
    buildSpectrumSnapshotCanvas,
    saveCanvasAsPng,
  };
})();
