# Canvas2D to WebGL Transition Plan

## Goal
- Replace all runtime Canvas2D rendering in the frontend with WebGL.
- Remove Canvas2D code paths after feature parity is reached.
- Keep existing interaction behavior (zoom/pan/tune/BW drag/tooltips/overlays) intact.

## Scope
- `src/trx-client/trx-frontend/trx-frontend-http/assets/web/app.js`
  - `overview-canvas`
  - `spectrum-canvas`
  - `signal-overlay-canvas`
- `src/trx-client/trx-frontend/trx-frontend-http/assets/web/plugins/cw.js`
  - `cw-tone-waterfall`
- New shared WebGL utility module:
  - `assets/web/webgl-renderer.js`

## Non-Goals
- No Canvas2D fallback path.
- No feature redesign outside rendering internals.

## Constraints
- Must preserve existing data flow and event wiring.
- Must keep map/decoder/bookmark integrations unchanged.
- Must remain dependency-free (no external rendering libraries).

## 2-Phase Migration
1. Phase 1 (Rendering engine insertion)
- Add shared WebGL renderer utility (primitives + textures + color parsing).
- Keep existing business logic and interaction handlers untouched.
- Swap draw targets from 2D contexts to WebGL primitives.

2. Phase 2 (Canvas2D removal and parity closure)
- Remove `getContext("2d")` usage from app and plugins.
- Remove obsolete 2D-specific cache paths.
- Validate behavior on resize/theme/style/stream reconnect/decoder mode changes.

## Parallel Workstreams ("Agents")
1. Agent A: Shared WebGL core
- Build `webgl-renderer.js` with:
  - HiDPI resize handling
  - Solid/gradient rects
  - Polyline/segment/fill primitives
  - RGBA texture upload + blit
  - CSS color parser helpers

2. Agent B: Main spectrum/overview migration
- Port `drawSpectrum`, `drawHeaderSignalGraph`, `drawSignalOverlay`, and clear paths.
- Replace 2D offscreen waterfall cache with WebGL texture updates.
- Keep frequency axis/bookmark axis DOM behavior unchanged.

3. Agent C: CW tone picker migration
- Port `drawCwTonePicker` primitives to WebGL.
- Preserve auto/manual tone interactions and mode gating.

## Acceptance Criteria
- No frontend `getContext("2d")` usage remains.
- All four canvases render using WebGL and respond to resize/DPR changes.
- Spectrum interactions still work:
  - wheel zoom
  - drag pan
  - BW edge drag
  - click tune
- Overview strip continues showing waterfall/history.
- CW tone picker remains interactive and reflects current spectrum/tone.

## Verification Checklist
- Static:
  - `rg -n 'getContext\\("2d"\\)' src/trx-client/trx-frontend/trx-frontend-http/assets/web`
- Runtime smoke:
  - Open main tab: verify overview + spectrum + overlay.
  - Toggle theme/style.
  - Resize window and spectrum grip.
  - Enable CW decoder and validate tone picker updates/click-to-set.
  - Confirm no rendering exceptions in browser console.

## Rollout Notes
- Initial rollout is WebGL-only.
- If a browser lacks WebGL, canvases remain blank by design until a dedicated fallback policy is defined.
