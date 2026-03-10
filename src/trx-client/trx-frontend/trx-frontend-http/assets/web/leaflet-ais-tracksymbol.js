(function() {
  if (typeof L === "undefined") return;

  function clamp(value, min, max) {
    return Math.max(min, Math.min(max, value));
  }

  function finiteAngle(value) {
    if (!Number.isFinite(value)) return null;
    const normalized = ((Number(value) % 360) + 360) % 360;
    return normalized;
  }

  function svgColor(value, fallback) {
    const text = String(value || fallback || "");
    return text.replace(/"/g, "&quot;");
  }

  function buildSymbolHtml(options, zoom) {
    const heading = finiteAngle(options.heading);
    const course = finiteAngle(options.course);
    const angle = heading != null ? heading : course;
    const speed = Number.isFinite(options.speed) ? Math.max(0, Number(options.speed)) : 0;
    const sizeBase = Number.isFinite(options.size) ? Number(options.size) : 22;
    const zoomBoost = zoom >= 12 ? 4 : zoom >= 9 ? 2 : 0;
    const size = clamp(sizeBase + zoomBoost, 16, 32);
    const courseLen = course != null ? clamp(size * (0.55 + Math.min(speed, 30) / 30), size * 0.55, size * 1.2) : 0;
    const color = svgColor(options.color, "#ff7559");
    const outline = svgColor(options.outline, "#6b2118");

    const body = angle != null
      ? `<g transform="translate(${size / 2} ${size / 2}) rotate(${angle}) translate(${-size / 2} ${-size / 2})">` +
          `<path d="M ${size * 0.5} ${size * 0.06} L ${size * 0.82} ${size * 0.78} L ${size * 0.5} ${size * 0.62} L ${size * 0.18} ${size * 0.78} Z" fill="${color}" stroke="${outline}" stroke-width="1.2" stroke-linejoin="round" />` +
        `</g>`
      : `<path d="M ${size * 0.5} ${size * 0.12} L ${size * 0.88} ${size * 0.5} L ${size * 0.5} ${size * 0.88} L ${size * 0.12} ${size * 0.5} Z" fill="${color}" stroke="${outline}" stroke-width="1.2" stroke-linejoin="round" />`;

    const courseLine = course != null
      ? `<g transform="translate(${size / 2} ${size / 2}) rotate(${course})">` +
          `<line x1="0" y1="${-size * 0.22}" x2="0" y2="${-(size * 0.22 + courseLen)}" stroke="${color}" stroke-width="1.4" stroke-linecap="round" opacity="0.75" />` +
        `</g>`
      : "";

    return (
      `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}" aria-hidden="true">` +
        courseLine +
        body +
      `</svg>`
    );
  }

  L.TrxAisTrackSymbol = L.Marker.extend({
    options: {
      heading: null,
      course: null,
      speed: null,
      color: "#ff7559",
      outline: "#6b2118",
      size: 22,
      interactive: true,
      keyboard: true,
      riseOnHover: true,
    },

    initialize: function(latlng, options) {
      const merged = L.Util.extend({}, this.options, options || {});
      merged.icon = L.divIcon({
        className: "trx-ais-track-symbol-icon",
        html: "",
        iconSize: [merged.size, merged.size],
        iconAnchor: [merged.size / 2, merged.size / 2],
      });
      L.Marker.prototype.initialize.call(this, latlng, merged);
    },

    onAdd: function(map) {
      L.Marker.prototype.onAdd.call(this, map);
      this._refreshIcon();
      this._boundZoomRefresh = this._refreshIcon.bind(this);
      map.on("zoomend", this._boundZoomRefresh);
    },

    onRemove: function(map) {
      if (this._boundZoomRefresh) {
        map.off("zoomend", this._boundZoomRefresh);
        this._boundZoomRefresh = null;
      }
      L.Marker.prototype.onRemove.call(this, map);
    },

    setAisState: function(next) {
      if (next && typeof next === "object") {
        if ("heading" in next) this.options.heading = next.heading;
        if ("course" in next) this.options.course = next.course;
        if ("speed" in next) this.options.speed = next.speed;
        if ("color" in next) this.options.color = next.color;
        if ("outline" in next) this.options.outline = next.outline;
      }
      this._refreshIcon();
      return this;
    },

    _refreshIcon: function() {
      if (!this._icon) return;
      const zoom = this._map && typeof this._map.getZoom === "function" ? this._map.getZoom() : 0;
      const html = buildSymbolHtml(this.options, zoom);
      this._icon.innerHTML = html;
      const sizeBase = Number.isFinite(this.options.size) ? Number(this.options.size) : 22;
      const zoomBoost = zoom >= 12 ? 4 : zoom >= 9 ? 2 : 0;
      const size = clamp(sizeBase + zoomBoost, 16, 32);
      this._icon.style.width = `${size}px`;
      this._icon.style.height = `${size}px`;
      this._icon.style.marginLeft = `${-size / 2}px`;
      this._icon.style.marginTop = `${-size / 2}px`;
    },
  });

  L.trxAisTrackSymbol = function(latlng, options) {
    return new L.TrxAisTrackSymbol(latlng, options);
  };
})();
