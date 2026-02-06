# Qt QML Frontend Requirements

## Scope
- Provide a Qt Quick (QML) GUI frontend for trx-rs.
- Linux-only support for the initial implementation.
- Use system-wide Qt6 (no vendored Qt).
- Frontend must be optional and feature-gated; default build should not require Qt.
  - Feature name in `trx-bin`: `qt-frontend`.

## Functional Requirements
- Show rig status: frequency, mode, PTT state, VFO info, lock state, power state.
- Show basic meters when available: RX signal, TX power/limit/SWR/ALC (as provided by state).
- Allow commands: set frequency, set mode, toggle PTT, power on/off, toggle VFO, lock/unlock, set TX limit (if supported).
- Reflect live updates pushed from the rig task (watch updates).

## Non-Functional Requirements
- Linux-only for now.
- Build relies on Qt6 libraries/headers installed on the system.
- GUI must be responsive and not block the rig task or frontend thread.
- Minimal but clear UI; no advanced theming or custom widgets required yet.

## Configuration & Integration
- Expose as a new frontend crate: `trx-frontend-qt`.
- Register via frontend registry under name: `qt`.
- Optional via feature flag (e.g., `qt`) and not part of default workspace features.
- Provide config toggles under `[frontends.qt]` for enable/listen if needed.
  - Remote client mode uses JSON TCP with bearer token via `frontends.qt.remote.*`.

## Packaging/Build
- Document required packages (Qt6 base + QML modules + qmetaobject-rs build prereqs).
- Provide build/run instructions in README/OVERVIEW updates.

## Out of Scope (for v1)
- Windows/macOS support.
- Offline themes or custom QML assets.
- Advanced settings editor or multi-rig management.
