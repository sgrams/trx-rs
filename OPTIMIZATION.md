# DSP Chain Performance Optimization Guidelines

This document captures lessons learned and best practices for optimizing
the real-time DSP pipelines in trx-rs, particularly the WFM stereo decoder
and audio encoding paths.

## General Principles

1. **Measure first.** Profile with real workloads before optimizing.
   Synthetic benchmarks miss cache effects, branch prediction patterns,
   and real signal statistics.

2. **Eliminate transcendentals from inner loops.** A single `sin_cos` or
   `atan2` per sample at 200 kHz composite rate costs millions of calls
   per second. Replace with:
   - **Quadrature NCO** for oscillators: maintain `(cos, sin)` state and
     rotate by a precomputed `(cos_inc, sin_inc)` each sample. Cost:
     4 muls + 2 adds. Renormalize every ~1024 samples to prevent drift.
   - **Double-angle identities** to derive `sin(2θ), cos(2θ)` from
     `sin(θ), cos(θ)`: `sin2 = 2·sin·cos`, `cos2 = 2·cos²−1`.
   - **I/Q arm extraction** for PLL phase error: if you have
     `i = lp(signal * cos)` and `q = lp(signal * -sin)`, then
     `sin(err) = q/mag`, `cos(err) = i/mag` — no `atan2` or `sin_cos`
     needed for the rotation.

3. **Batch operations for SIMD.** Separate data-parallel work (e.g. FM
   discriminator: conjugate-multiply + atan2) from sequential-state work
   (PLL, biquads). Process the parallel part in batches of 8 using AVX2,
   then feed scalar results into the sequential pipeline.

4. **Power-of-2 sizes for circular buffers.** Use `& (N-1)` bitmask
   instead of `% N` modulo. Ensure buffer lengths (e.g. `WFM_RESAMP_TAPS`)
   are powers of two.

5. **Circular buffers over shift registers.** Writing one sample at a
   ring-buffer position is O(1); `rotate_left(1)` is O(N). For a 32-tap
   FIR called 3× per composite sample, this eliminates ~200 byte-moves
   per sample.

6. **Decimate slow-changing metrics.** Stereo detection (pilot coherence,
   lock, drive) changes over tens of milliseconds. Running it every 16th
   sample instead of every sample saves ~94% of that work with no audible
   effect. Accumulate values over the window and process the average.

## Filter Design

- **Match filter cutoffs** across parallel paths (sum and diff) to ensure
  identical group delay. Mismatched cutoffs cause frequency-dependent
  phase errors that directly degrade stereo separation.

- **4th-order Butterworth** (two cascaded biquads) is generally sufficient
  when the polyphase resampler provides additional stopband rejection.
  6th-order adds 50% more biquad evaluations per sample for diminishing
  returns.

- **Q values for Butterworth cascades:**
  - 4th-order: Q₁ = 0.5412, Q₂ = 1.3066
  - 6th-order: Q₁ = 0.5176, Q₂ = 0.7071, Q₃ = 1.9319

## Polyphase Resampler

- **Compute cutoff from actual rate ratio:** `cutoff = output_rate / input_rate`.
  A fixed cutoff (e.g. 0.94) can be catastrophically wrong — at 200 kHz
  composite to 48 kHz audio, it passes everything up to 94 kHz while the
  output Nyquist is only 24 kHz. The 38 kHz stereo subcarrier residuals
  alias directly into the treble range.

- **Blackman-Harris window** gives ~92 dB stopband rejection vs ~43 dB
  for Hamming, at the same tap count. Use it for the windowed-sinc
  coefficients:
  ```
  w(n) = 0.35875 − 0.48829·cos(2πn/N) + 0.14128·cos(4πn/N) − 0.01168·cos(6πn/N)
  ```

- **32 taps** with Blackman-Harris and a proper cutoff gives >60 dB
  stopband rejection — more than enough. 64 taps doubles the MAC count
  for marginal improvement.

- **64 polyphase phases** balances fractional sample resolution against
  coefficient bank size (64 × 32 × 4 = 8 KB fits comfortably in L1
  cache). 128 phases offer diminishing returns for double the memory.

## FM Discriminator

- **Batch with AVX2:** The conjugate-multiply + atan2 pattern is
  data-parallel (each output depends only on two adjacent input samples).
  Process 8 samples at a time using 256-bit SIMD.

- **Use a high-precision atan2 polynomial** for AVX2. A 7th-order minimax
  polynomial (max error ~2.4e-7 rad) avoids the treble distortion that
  cheap 1st-order approximations (e.g. `0.273*(1−|z|)`) introduce on
  strong signals. Coefficients:
  ```
  c0 =  0.999_999_5
  c1 = −0.333_326_1
  c2 =  0.199_777_1
  c3 = −0.138_776_8
  ```

- **Branchless argument reduction** for atan2: swap `|y|` and `|x|` using
  masks rather than branches, apply quadrant correction via arithmetic
  shift and copysign.

## WFM Stereo Specifics

- **Pilot notch before diff demod:** The 19 kHz pilot leaks into the
  38 kHz multiplication and creates intermod products. Notch it from the
  composite signal before `x * cos(2θ)`. This notch is separate from the
  mono-path pilot notch (which sits after the sum LPF).

- **IQ hard limiter before FM discriminator:** For WFM, only the phase
  carries information. Normalizing IQ magnitude to 1.0 prevents
  overdeviation artifacts and clipping. Guard against zero magnitude.

- **Binary stereo blend:** A smooth blend function (e.g. smoothstep)
  sounds good in theory but reduces real-world separation. Use
  `blend = 1.0` when pilot is detected, `0.0` otherwise.

- **STEREO_MATRIX_GAIN = 0.50:** The correct unity factor for
  `L = (S+D)/2`, `R = (S−D)/2`. Lower values waste headroom; higher
  values clip.

## Opus Encoding

- **Complexity 5** (down from default 9-10) saves significant CPU with
  minimal quality impact at bitrates ≥128 kbps. The higher complexity
  levels run expensive psychoacoustic search algorithms that produce
  negligible improvement at high bitrates.

- **256 kbps** is transparent for stereo FM broadcast audio. Going higher
  wastes bandwidth; going below 128 kbps may introduce artifacts on
  complex program material.

- **`Application::Audio`** (not VoIP) — uses the MDCT-based CELT mode
  optimized for music and broadband audio rather than speech.

## AVX2 Guidelines

- Gate all AVX2 code behind `#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]`
  and runtime `is_x86_feature_detected!("avx2")` checks.

- Mark unsafe SIMD functions with `#[target_feature(enable = "avx2")]`
  so the compiler generates AVX2 code for the function body.

- Provide scalar fallbacks for non-x86 targets and CPUs without AVX2.

- Add epsilon guards (e.g. `1e-12`) to denominators in SIMD paths where
  both numerator and denominator can be zero simultaneously.

## What NOT to Optimize

- **Biquad filters** — already minimal (5 muls + 4 adds per sample).
  The sequential state dependency prevents SIMD vectorization within a
  single stream.

- **One-pole lowpass filters** — single multiply-accumulate, cannot be
  made faster.

- **DC blockers** — trivial per-sample cost.

- **Deemphasis** — single biquad, runs at audio rate (not composite rate).

## Profiling Tips

- Use `cargo build --release` — debug builds are 10-50x slower and
  misleading for DSP profiling.

- `perf stat` / `Instruments` on the inner loop to check IPC, cache
  misses, and branch mispredictions.

- Compare CPU% with stereo enabled vs disabled to isolate stereo-specific
  costs (diff path biquads, pilot PLL, 38 kHz demod, resampler channels).

- Watch for unexpected `libm` calls in disassembly — the compiler may
  not inline `f32::atan2` or `f32::sin_cos` even in release mode.
