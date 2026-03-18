# trx-ftx: Pure Rust FTx Decoder

## Goal

`trx-ftx` is the pure Rust replacement for the old `trx-ft8` C FFI wrapper.
It provides the same public API (`Ft8Decoder`, `Ft8DecodeResult`) so it can
serve as a drop-in decoder implementation.

## Why

- Eliminates `cc`/`libc` build dependencies and `unsafe` FFI
- Better tooling integration (rust-analyzer, clippy, miri)
- Easier maintenance: one language, one build system
- Estimated ~5,350 lines of Rust vs ~7,900 lines of C

## Crate Structure

```
src/decoders/trx-ftx/
  Cargo.toml
  FTX_CRATE.md          # This file
  src/
    lib.rs              # Re-exports Ft8Decoder, Ft8DecodeResult
    protocol.rs         # FtxProtocol enum, timing constants, LDPC params
    constants.rs        # Costas arrays, LDPC matrices, Gray maps, XOR sequence
    crc.rs              # CRC-14 (poly 0x2757)
    text.rs             # Character tables, string utilities
    ldpc.rs             # Belief propagation + sum-product LDPC decoders
    encode.rs           # LDPC encoding, Gray mapping, Costas sync insertion
    message.rs          # Message pack/unpack (all FTx message types)
    callsign_hash.rs    # Open-addressing hash table for callsign lookup
    monitor.rs          # Windowed FFT waterfall/spectrogram (Hann window)
    decode.rs           # Candidate search, sync scoring, likelihood extraction
    decoder.rs          # Top-level Ft8Decoder (public API)
    ft2/
      mod.rs            # FT2 pipeline orchestration
      downsample.rs     # Freq-domain downsampling via IFFT
      sync.rs           # 2D sync scoring with complex reference waveforms
      bitmetrics.rs     # Per-symbol FFT, multi-scale bit metrics
      osd.rs            # OSD-1/OSD-2 CRC-guided decoder
  tests/
    decode_ft8_wav.rs   # Integration tests with WAV fixtures
    block_size.rs       # Block size compatibility test
```

## Dependencies

```toml
[dependencies]
rustfft = "6"           # SIMD-optimized FFT
realfft = "3"           # Real-to-complex FFT wrapper
num-complex = "0.4"     # Complex32 type

[dev-dependencies]
hound = "3"             # WAV file reading for integration tests
```

## Implementation Phases

### Phase 1: Foundation (no FFT, no inter-module deps)
1. `protocol.rs` - FtxProtocol enum with timing/parameter methods
2. `constants.rs` - All lookup tables as const arrays
3. `crc.rs` - CRC-14 compute/extract/add
4. `text.rs` - Character tables, string utilities
5. `ldpc.rs` - BP + sum-product decoders with fast tanh/atanh
6. `encode.rs` - LDPC encoding + tone generation
7. `message.rs` - Pack/unpack for all FTx message types
8. `callsign_hash.rs` - Hash table for callsign dedup/lookup

### Phase 2: DSP (FFT-dependent)
9. `monitor.rs` - Waterfall engine using realfft/rustfft

### Phase 3: Decode Pipeline
10. `decode.rs` - Candidate search + FT8/FT4/FT2 likelihood extraction
11. `ft2/` - FT2-specific multi-pass pipeline:
    - `downsample.rs` - Freq-domain bandpass + IFFT
    - `sync.rs` - 2D sync scoring with Costas waveforms
    - `bitmetrics.rs` - Multi-scale bit metrics (1/2/4-symbol)
    - `osd.rs` - OSD-1/OSD-2 bit-flip search

### Phase 4: Public API
12. `decoder.rs` - Ft8Decoder struct (matches trx-ft8 API exactly)
13. `lib.rs` - Re-exports

### Phase 5: Migration
14. Convert `trx-ft8` to thin re-export of `trx-ftx`
15. Delete C sources: `ft8_wrapper.c`, `ft2_ldpc.c`, `build.rs`
16. Remove the vendored `ft8_lib` checkout after the port is complete

## Historical C Sources Ported

| C Source | Rust Target | Lines |
|----------|-------------|-------|
| `ft8_lib/ft8/message.c` | `message.rs` | 1156 |
| `src/decoders/trx-ft8/src/ft8_wrapper.c` | `decoder.rs` + `ft2/` | 1800 |
| `ft8_lib/ft8/decode.c` | `decode.rs` | 773 |
| `ft8_lib/ft8/constants.c` | `constants.rs` | 391 |
| `ft8_lib/ft8/text.c` | `text.rs` | 303 |
| `ft8_lib/common/monitor.c` | `monitor.rs` | 261 |
| `ft8_lib/ft8/ldpc.c` | `ldpc.rs` | 251 |
| `ft8_lib/ft8/encode.c` | `encode.rs` | 200 |
| `ft8_lib/ft8/crc.c` | `crc.rs` | 63 |
| `ft8_lib/fft/*.c` | replaced by `rustfft` | 555 |

## Public API (matches trx-ft8 exactly)

```rust
pub struct Ft8DecodeResult {
    pub text: String,
    pub snr_db: f32,
    pub dt_s: f32,
    pub freq_hz: f32,
}

pub struct Ft8Decoder { .. }

impl Ft8Decoder {
    pub fn new(sample_rate: u32) -> Result<Self, String>;
    pub fn new_ft4(sample_rate: u32) -> Result<Self, String>;
    pub fn new_ft2(sample_rate: u32) -> Result<Self, String>;
    pub fn block_size(&self) -> usize;
    pub fn sample_rate(&self) -> u32;
    pub fn window_samples(&self) -> usize;
    pub fn reset(&mut self);
    pub fn process_block(&mut self, block: &[f32]);
    pub fn decode_if_ready(&mut self, max_results: usize) -> Vec<Ft8DecodeResult>;
}
```

## Testing Strategy

- Unit tests per module: CRC round-trip, LDPC recovery, message pack/unpack
- Integration tests: decode reference WAV fixtures when available
- Compatibility test: `ft2_uses_distinct_block_size` (FT4=576, FT2=288, window=45000)
