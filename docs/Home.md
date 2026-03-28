# trx-rs

`trx-rs` is a modular amateur radio control stack written in Rust. It splits
hardware access, DSP, transport, and user-facing interfaces into separate
components so a radio or SDR can be controlled locally while audio, decoding,
and remote control are exposed elsewhere on the network.

## Documentation

- [User Manual](User-Manual) — configuration, features, and usage
- [Architecture](Architecture) — system design, crate layout, data flow, and internals
- [Optimization Guidelines](Optimization-Guidelines) — performance guidelines for the real-time DSP pipeline
- [Planned Features](Planned-Features) — planned features and design notes
- [Improvement Areas](Improvement-Areas) — codebase audit: quality, architecture, security, and performance
