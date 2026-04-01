(function initTrxWebGl(global) {
  "use strict";

  const cssColorCache = new Map();
  let cssColorProbe = null;

  function clearCssColorCache() {
    cssColorCache.clear();
  }

  function ensureCssColorProbe() {
    if (cssColorProbe) return cssColorProbe;
    const el = document.createElement("span");
    el.style.position = "absolute";
    el.style.left = "-9999px";
    el.style.top = "-9999px";
    el.style.pointerEvents = "none";
    el.style.opacity = "0";
    document.body.appendChild(el);
    cssColorProbe = el;
    return cssColorProbe;
  }

  function parseRgbString(value) {
    const m = /^rgba?\(([^)]+)\)$/.exec(String(value || "").trim());
    if (!m) return null;
    const parts = m[1].split(",").map((p) => p.trim());
    if (parts.length < 3) return null;
    const r = Number(parts[0]);
    const g = Number(parts[1]);
    const b = Number(parts[2]);
    const a = parts.length > 3 ? Number(parts[3]) : 1;
    if (![r, g, b, a].every(Number.isFinite)) return null;
    return [
      Math.max(0, Math.min(1, r / 255)),
      Math.max(0, Math.min(1, g / 255)),
      Math.max(0, Math.min(1, b / 255)),
      Math.max(0, Math.min(1, a)),
    ];
  }

  function parseHexColor(value) {
    const raw = String(value || "").trim();
    if (!/^#([0-9a-f]{3,8})$/i.test(raw)) return null;
    let hex = raw.slice(1);
    if (hex.length === 3 || hex.length === 4) {
      hex = hex.split("").map((ch) => ch + ch).join("");
    }
    if (!(hex.length === 6 || hex.length === 8)) return null;
    const r = parseInt(hex.slice(0, 2), 16) / 255;
    const g = parseInt(hex.slice(2, 4), 16) / 255;
    const b = parseInt(hex.slice(4, 6), 16) / 255;
    const a = hex.length === 8 ? parseInt(hex.slice(6, 8), 16) / 255 : 1;
    return [r, g, b, a];
  }

  function parseCssColor(value) {
    const key = String(value ?? "");
    if (cssColorCache.has(key)) return cssColorCache.get(key).slice();

    let parsed = parseHexColor(key) || parseRgbString(key);
    if (!parsed) {
      const probe = ensureCssColorProbe();
      probe.style.color = "";
      probe.style.color = key;
      const computed = getComputedStyle(probe).color;
      parsed = parseRgbString(computed) || [0, 0, 0, 1];
    }
    cssColorCache.set(key, parsed.slice());
    return parsed.slice();
  }

  function hslToRgba(h, s, l, a = 1) {
    const hue = ((((Number(h) || 0) % 360) + 360) % 360) / 360;
    const sat = Math.max(0, Math.min(1, (Number(s) || 0) / 100));
    const lig = Math.max(0, Math.min(1, (Number(l) || 0) / 100));

    const q = lig < 0.5 ? lig * (1 + sat) : lig + sat - lig * sat;
    const p = 2 * lig - q;
    const hueToRgb = (t) => {
      let tt = t;
      if (tt < 0) tt += 1;
      if (tt > 1) tt -= 1;
      if (tt < 1 / 6) return p + (q - p) * 6 * tt;
      if (tt < 1 / 2) return q;
      if (tt < 2 / 3) return p + (q - p) * (2 / 3 - tt) * 6;
      return p;
    };

    const r = sat === 0 ? lig : hueToRgb(hue + 1 / 3);
    const g = sat === 0 ? lig : hueToRgb(hue);
    const b = sat === 0 ? lig : hueToRgb(hue - 1 / 3);
    return [r, g, b, Math.max(0, Math.min(1, Number(a)))];
  }

  function normalizeColor(input, alphaMul = 1) {
    let rgba;
    if (Array.isArray(input)) {
      const arr = input.map((v) => Number(v));
      if (arr.length >= 4) {
        rgba = [arr[0], arr[1], arr[2], arr[3]];
      } else {
        rgba = [0, 0, 0, 1];
      }
    } else if (typeof input === "string") {
      rgba = parseCssColor(input);
    } else if (input && typeof input === "object") {
      rgba = [
        Number(input.r) || 0,
        Number(input.g) || 0,
        Number(input.b) || 0,
        Number(input.a ?? 1),
      ];
    } else {
      rgba = [0, 0, 0, 1];
    }
    const out = [
      Math.max(0, Math.min(1, rgba[0])),
      Math.max(0, Math.min(1, rgba[1])),
      Math.max(0, Math.min(1, rgba[2])),
      Math.max(0, Math.min(1, rgba[3] * alphaMul)),
    ];
    return out;
  }

  function compileShader(gl, type, source) {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      const log = gl.getShaderInfoLog(shader) || "shader compile error";
      gl.deleteShader(shader);
      throw new Error(log);
    }
    return shader;
  }

  function createProgram(gl, vertexSrc, fragmentSrc) {
    const vs = compileShader(gl, gl.VERTEX_SHADER, vertexSrc);
    const fs = compileShader(gl, gl.FRAGMENT_SHADER, fragmentSrc);
    const program = gl.createProgram();
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    gl.deleteShader(vs);
    gl.deleteShader(fs);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      const log = gl.getProgramInfoLog(program) || "program link error";
      gl.deleteProgram(program);
      throw new Error(log);
    }
    return program;
  }

  function pushColoredVertex(target, x, y, rgba) {
    target.push(x, y, rgba[0], rgba[1], rgba[2], rgba[3]);
  }

  function segmentToQuadVertices(out, x0, y0, x1, y1, halfW, rgba) {
    const dx = x1 - x0;
    const dy = y1 - y0;
    const len = Math.hypot(dx, dy);
    if (!(len > 0.0001)) return;
    const nx = (-dy / len) * halfW;
    const ny = (dx / len) * halfW;

    const ax = x0 - nx, ay = y0 - ny;
    const bx = x0 + nx, by = y0 + ny;
    const cx = x1 + nx, cy = y1 + ny;
    const dx2 = x1 - nx, dy2 = y1 - ny;

    pushColoredVertex(out, ax, ay, rgba);
    pushColoredVertex(out, bx, by, rgba);
    pushColoredVertex(out, cx, cy, rgba);

    pushColoredVertex(out, ax, ay, rgba);
    pushColoredVertex(out, cx, cy, rgba);
    pushColoredVertex(out, dx2, dy2, rgba);
  }

  class TrxWebGlRenderer {
    constructor(canvas, options = {}) {
      this.canvas = canvas;
      this.options = { alpha: true, premultipliedAlpha: false, ...options };
      this.gl =
        canvas?.getContext("webgl", this.options) ||
        canvas?.getContext("experimental-webgl", this.options) ||
        null;
      this.ready = !!this.gl;
      this.textures = new Map();
      // Reusable scratch buffers — avoids per-draw-call Float32Array allocation
      // and lets us use bufferSubData instead of bufferData (no GPU realloc).
      this._colorScratch = new Float32Array(4096 * 6); // grows as needed
      this._colorGpuSize = 0;                          // current GPU buffer size (floats)
      this._texScratch = new Float32Array(6 * 4);      // fixed: 6 verts × (xy+uv)
      if (!this.ready) return;

      const gl = this.gl;
      gl.disable(gl.DEPTH_TEST);
      gl.disable(gl.CULL_FACE);
      gl.enable(gl.BLEND);
      gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

      const colorVertexSrc =
        "attribute vec2 a_pos;\n" +
        "attribute vec4 a_color;\n" +
        "uniform vec2 u_resolution;\n" +
        "varying vec4 v_color;\n" +
        "void main() {\n" +
        "  vec2 zeroToOne = a_pos / u_resolution;\n" +
        "  vec2 clip = zeroToOne * 2.0 - 1.0;\n" +
        "  gl_Position = vec4(clip * vec2(1.0, -1.0), 0.0, 1.0);\n" +
        "  v_color = a_color;\n" +
        "}\n";
      const colorFragmentSrc =
        "precision mediump float;\n" +
        "varying vec4 v_color;\n" +
        "void main() {\n" +
        "  gl_FragColor = v_color;\n" +
        "}\n";

      const textureVertexSrc =
        "attribute vec2 a_pos;\n" +
        "attribute vec2 a_uv;\n" +
        "uniform vec2 u_resolution;\n" +
        "varying vec2 v_uv;\n" +
        "void main() {\n" +
        "  vec2 zeroToOne = a_pos / u_resolution;\n" +
        "  vec2 clip = zeroToOne * 2.0 - 1.0;\n" +
        "  gl_Position = vec4(clip * vec2(1.0, -1.0), 0.0, 1.0);\n" +
        "  v_uv = a_uv;\n" +
        "}\n";
      const textureFragmentSrc =
        "precision mediump float;\n" +
        "varying vec2 v_uv;\n" +
        "uniform sampler2D u_tex;\n" +
        "uniform float u_alpha;\n" +
        "void main() {\n" +
        "  vec4 c = texture2D(u_tex, v_uv);\n" +
        "  gl_FragColor = vec4(c.rgb, c.a * u_alpha);\n" +
        "}\n";

      this.colorProgram = createProgram(gl, colorVertexSrc, colorFragmentSrc);
      this.colorBuffer = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, this.colorBuffer);
      gl.bufferData(gl.ARRAY_BUFFER, this._colorScratch, gl.DYNAMIC_DRAW);
      this._colorGpuSize = this._colorScratch.length;
      this.colorLoc = {
        pos: gl.getAttribLocation(this.colorProgram, "a_pos"),
        color: gl.getAttribLocation(this.colorProgram, "a_color"),
        resolution: gl.getUniformLocation(this.colorProgram, "u_resolution"),
      };

      this.textureProgram = createProgram(gl, textureVertexSrc, textureFragmentSrc);
      this.textureBuffer = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, this.textureBuffer);
      gl.bufferData(gl.ARRAY_BUFFER, this._texScratch, gl.DYNAMIC_DRAW);
      this.textureLoc = {
        pos: gl.getAttribLocation(this.textureProgram, "a_pos"),
        uv: gl.getAttribLocation(this.textureProgram, "a_uv"),
        resolution: gl.getUniformLocation(this.textureProgram, "u_resolution"),
        alpha: gl.getUniformLocation(this.textureProgram, "u_alpha"),
        tex: gl.getUniformLocation(this.textureProgram, "u_tex"),
      };
    }

    ensureSize(cssWidth, cssHeight, dpr = (window.devicePixelRatio || 1)) {
      if (!this.ready) return false;
      const nextW = Math.max(1, Math.round(cssWidth * dpr));
      const nextH = Math.max(1, Math.round(cssHeight * dpr));
      const changed = this.canvas.width !== nextW || this.canvas.height !== nextH;
      if (changed) {
        this.canvas.width = nextW;
        this.canvas.height = nextH;
      }
      this.gl.viewport(0, 0, this.canvas.width, this.canvas.height);
      return changed;
    }

    clear(color) {
      if (!this.ready) return;
      const gl = this.gl;
      const rgba = normalizeColor(color);
      gl.clearColor(rgba[0], rgba[1], rgba[2], rgba[3]);
      gl.clear(gl.COLOR_BUFFER_BIT);
    }

    drawTriangles(vertices) {
      this._drawColorGeometry(vertices, this.gl.TRIANGLES);
    }

    drawTriangleStrip(vertices) {
      this._drawColorGeometry(vertices, this.gl.TRIANGLE_STRIP);
    }

    _drawColorGeometry(vertices, mode) {
      if (!this.ready || !vertices || vertices.length === 0) return;
      const gl = this.gl;
      const count = vertices.length;

      // Grow scratch buffer if needed (doubles each time to amortise copies).
      if (count > this._colorScratch.length) {
        let newLen = this._colorScratch.length;
        while (newLen < count) newLen *= 2;
        this._colorScratch = new Float32Array(newLen);
      }

      // Copy into scratch (set() is a fast typed memcpy; avoids new allocation).
      this._colorScratch.set(vertices);
      const view = this._colorScratch.subarray(0, count);

      gl.useProgram(this.colorProgram);
      gl.bindBuffer(gl.ARRAY_BUFFER, this.colorBuffer);

      // Only reallocate the GPU buffer when it is too small; otherwise use
      // bufferSubData which avoids a GPU reallocation (Safari is sensitive to this).
      if (count > this._colorGpuSize) {
        gl.bufferData(gl.ARRAY_BUFFER, this._colorScratch, gl.DYNAMIC_DRAW);
        this._colorGpuSize = this._colorScratch.length;
      } else {
        gl.bufferSubData(gl.ARRAY_BUFFER, 0, view);
      }

      gl.enableVertexAttribArray(this.colorLoc.pos);
      gl.vertexAttribPointer(this.colorLoc.pos, 2, gl.FLOAT, false, 24, 0);
      gl.enableVertexAttribArray(this.colorLoc.color);
      gl.vertexAttribPointer(this.colorLoc.color, 4, gl.FLOAT, false, 24, 8);
      gl.uniform2f(this.colorLoc.resolution, this.canvas.width, this.canvas.height);
      gl.drawArrays(mode, 0, count / 6);
    }

    fillRect(x, y, w, h, color) {
      if (w <= 0 || h <= 0) return;
      const rgba = normalizeColor(color);
      const v = [];
      pushColoredVertex(v, x, y, rgba);
      pushColoredVertex(v, x + w, y, rgba);
      pushColoredVertex(v, x + w, y + h, rgba);
      pushColoredVertex(v, x, y, rgba);
      pushColoredVertex(v, x + w, y + h, rgba);
      pushColoredVertex(v, x, y + h, rgba);
      this._drawColorGeometry(v, this.gl.TRIANGLES);
    }

    fillGradientRect(x, y, w, h, colorTL, colorTR, colorBR, colorBL) {
      if (w <= 0 || h <= 0) return;
      const tl = normalizeColor(colorTL);
      const tr = normalizeColor(colorTR);
      const br = normalizeColor(colorBR);
      const bl = normalizeColor(colorBL);
      const v = [];
      pushColoredVertex(v, x, y, tl);
      pushColoredVertex(v, x + w, y, tr);
      pushColoredVertex(v, x + w, y + h, br);
      pushColoredVertex(v, x, y, tl);
      pushColoredVertex(v, x + w, y + h, br);
      pushColoredVertex(v, x, y + h, bl);
      this._drawColorGeometry(v, this.gl.TRIANGLES);
    }

    drawPolyline(points, color, width = 1) {
      if (!Array.isArray(points) || points.length < 4) return;
      const rgba = normalizeColor(color);
      const halfW = Math.max(0.5, Number(width) || 1) / 2;
      const verts = [];
      for (let i = 0; i < points.length - 2; i += 2) {
        segmentToQuadVertices(
          verts,
          points[i], points[i + 1],
          points[i + 2], points[i + 3],
          halfW,
          rgba,
        );
      }
      this._drawColorGeometry(verts, this.gl.TRIANGLES);
    }

    drawSegments(segments, color, width = 1) {
      if (!Array.isArray(segments) || segments.length < 4) return;
      const rgba = normalizeColor(color);
      const halfW = Math.max(0.5, Number(width) || 1) / 2;
      const verts = [];
      for (let i = 0; i < segments.length - 3; i += 4) {
        segmentToQuadVertices(
          verts,
          segments[i], segments[i + 1],
          segments[i + 2], segments[i + 3],
          halfW,
          rgba,
        );
      }
      this._drawColorGeometry(verts, this.gl.TRIANGLES);
    }

    drawFilledArea(points, baselineY, color) {
      if (!Array.isArray(points) || points.length < 4) return;
      const rgba = normalizeColor(color);
      const verts = [];
      for (let i = 0; i < points.length; i += 2) {
        pushColoredVertex(verts, points[i], baselineY, rgba);
        pushColoredVertex(verts, points[i], points[i + 1], rgba);
      }
      this._drawColorGeometry(verts, this.gl.TRIANGLE_STRIP);
    }

    drawPoints(points, size, color) {
      if (!Array.isArray(points) || points.length < 2) return;
      const radius = Math.max(1, Number(size) || 1);
      const rgba = normalizeColor(color);
      const verts = [];
      for (let i = 0; i < points.length; i += 2) {
        const x = points[i] - radius;
        const y = points[i + 1] - radius;
        const w = radius * 2;
        const h = radius * 2;
        pushColoredVertex(verts, x, y, rgba);
        pushColoredVertex(verts, x + w, y, rgba);
        pushColoredVertex(verts, x + w, y + h, rgba);
        pushColoredVertex(verts, x, y, rgba);
        pushColoredVertex(verts, x + w, y + h, rgba);
        pushColoredVertex(verts, x, y + h, rgba);
      }
      this._drawColorGeometry(verts, this.gl.TRIANGLES);
    }

    drawDashedVerticalLine(x, y0, y1, dashLen, gapLen, color, width = 1) {
      const dash = Math.max(1, Number(dashLen) || 1);
      const gap = Math.max(1, Number(gapLen) || 1);
      const top = Math.min(y0, y1);
      const bottom = Math.max(y0, y1);
      const segments = [];
      for (let y = top; y < bottom; y += dash + gap) {
        const segEnd = Math.min(bottom, y + dash);
        segments.push(x, y, x, segEnd);
      }
      this.drawSegments(segments, color, width);
    }

    uploadRgbaTexture(name, width, height, data, filter = "linear") {
      if (!this.ready || !name || !data) return null;
      const gl = this.gl;
      let entry = this.textures.get(name);
      if (!entry) {
        const texture = gl.createTexture();
        entry = { texture, width: 0, height: 0 };
        this.textures.set(name, entry);
      }
      gl.bindTexture(gl.TEXTURE_2D, entry.texture);
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
      const mode = filter === "nearest" ? gl.NEAREST : gl.LINEAR;
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, mode);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, mode);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      if (entry.width !== width || entry.height !== height) {
        gl.texImage2D(
          gl.TEXTURE_2D,
          0,
          gl.RGBA,
          width,
          height,
          0,
          gl.RGBA,
          gl.UNSIGNED_BYTE,
          data,
        );
        entry.width = width;
        entry.height = height;
      } else {
        gl.texSubImage2D(
          gl.TEXTURE_2D,
          0,
          0,
          0,
          width,
          height,
          gl.RGBA,
          gl.UNSIGNED_BYTE,
          data,
        );
      }
      return entry.texture;
    }

    drawTexture(name, x, y, w, h, alpha = 1, flipY = true) {
      if (!this.ready || !name || w <= 0 || h <= 0) return;
      const entry = this.textures.get(name);
      if (!entry) return;
      const gl = this.gl;
      const s = this._texScratch;
      const x2 = x + w, y2 = y + h;
      if (flipY) {
        s[0]=x;  s[1]=y;  s[2]=0; s[3]=1;
        s[4]=x2; s[5]=y;  s[6]=1; s[7]=1;
        s[8]=x2; s[9]=y2; s[10]=1;s[11]=0;
        s[12]=x; s[13]=y; s[14]=0;s[15]=1;
        s[16]=x2;s[17]=y2;s[18]=1;s[19]=0;
        s[20]=x; s[21]=y2;s[22]=0;s[23]=0;
      } else {
        s[0]=x;  s[1]=y;  s[2]=0; s[3]=0;
        s[4]=x2; s[5]=y;  s[6]=1; s[7]=0;
        s[8]=x2; s[9]=y2; s[10]=1;s[11]=1;
        s[12]=x; s[13]=y; s[14]=0;s[15]=0;
        s[16]=x2;s[17]=y2;s[18]=1;s[19]=1;
        s[20]=x; s[21]=y2;s[22]=0;s[23]=1;
      }
      gl.useProgram(this.textureProgram);
      gl.bindBuffer(gl.ARRAY_BUFFER, this.textureBuffer);
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, s);
      gl.enableVertexAttribArray(this.textureLoc.pos);
      gl.vertexAttribPointer(this.textureLoc.pos, 2, gl.FLOAT, false, 16, 0);
      gl.enableVertexAttribArray(this.textureLoc.uv);
      gl.vertexAttribPointer(this.textureLoc.uv, 2, gl.FLOAT, false, 16, 8);
      gl.uniform2f(this.textureLoc.resolution, this.canvas.width, this.canvas.height);
      gl.uniform1f(this.textureLoc.alpha, Math.max(0, Math.min(1, Number(alpha) || 0)));
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, entry.texture);
      gl.uniform1i(this.textureLoc.tex, 0);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
    }
  }

  function createRenderer(canvas, options) {
    return new TrxWebGlRenderer(canvas, options);
  }

  global.trxParseCssColor = parseCssColor;
  global.trxHslToRgba = hslToRgba;
  global.createTrxWebGlRenderer = createRenderer;
  global.trxClearCssColorCache = clearCssColorCache;
})(window);
