const textDecoder = typeof TextDecoder === "function" ? new TextDecoder() : null;
const HISTORY_GROUP_KEYS = ["ais", "vdes", "aprs", "hf_aprs", "cw", "ft8", "ft4", "wspr"];

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
    return textDecoder ? textDecoder.decode(chunk) : String.fromCharCode(...chunk);
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
    throw new Error("Unexpected trailing bytes in decode history payload");
  }
  return value;
}

async function fetchAndDecodeHistory(url, batchLimit) {
  self.postMessage({ type: "status", phase: "fetching" });
  const resp = await fetch(url, { credentials: "same-origin" });
  if (!resp.ok) throw new Error(`History fetch failed: ${resp.status}`);
  const payload = await resp.arrayBuffer();
  if (!payload || payload.byteLength === 0) {
    self.postMessage({ type: "start", total: 0 });
    self.postMessage({ type: "done", total: 0 });
    return;
  }

  self.postMessage({ type: "status", phase: "decoding" });
  const history = decodeCborPayload(payload);
  const total = HISTORY_GROUP_KEYS.reduce((sum, key) => {
    const items = history && Array.isArray(history[key]) ? history[key] : [];
    return sum + items.length;
  }, 0);
  self.postMessage({ type: "start", total });

  let processed = 0;
  const safeLimit = Math.max(1, Math.min(2048, Number(batchLimit) || 512));

  for (const kind of HISTORY_GROUP_KEYS) {
    const items = history && Array.isArray(history[kind]) ? history[kind] : [];
    if (items.length === 0) continue;
    for (let index = 0; index < items.length; index += safeLimit) {
      const messages = items.slice(index, index + safeLimit);
      processed += messages.length;
      self.postMessage({
        type: "group",
        kind,
        messages,
        processed,
        total,
      });
    }
  }
  self.postMessage({ type: "done", total });
}

self.onmessage = (event) => {
  const data = event?.data || {};
  if (data?.type !== "fetch-history") return;
  fetchAndDecodeHistory(data.url || "/decode/history", data.batchLimit)
    .catch((err) => {
      self.postMessage({
        type: "error",
        message: err && err.message ? err.message : String(err || "unknown worker failure"),
      });
    });
};
