# RDS Parameter Tuning — Work in Progress

## Goal
Maximum sensitivity (weak-signal decode) with zero false positive PI decodes.

## Changes Made

### `src/decoders/trx-rds/src/lib.rs`

#### Constants tuned
- `RRC_ALPHA = 0.50` (was 0.75) — narrower noise bandwidth, ~0.6 dB SNR gain
- `COSTAS_KI = 3.5e-7` — loop damping ζ≈0.68, well-damped (1e-6 caused instability)
- `PI_ACC_THRESHOLD = 2` — accumulate 2 Block A observations before committing PI

#### Soft confidence fix
In `Candidate::process_sample`, the soft confidence passed to `push_bit_soft` is now
`biphase_i.abs()` (was full vector magnitude). This aligns confidence with the bit
decision sign and prevents OSD(2) from false-decoding noise when the Costas loop
has residual phase error.

#### OSD(2) in locked mode (kept)
`decode_block_soft` performs OSD(2): hard decode → all 26 single-bit flips → all
325 two-bit flip pairs. Only active in locked mode; sequential B→C→D block-type
gating limits false positives.

#### Search mode: hard decode only
Removed OSD(1) from Block A acquisition (search mode). With OSD(1), ~13% of
random 26-bit words would falsely pass the Block A test per bit, allowing wrong
clock-phase candidates to accumulate false groups as fast as the correct candidate
accumulates real ones. Hard decode reduces the false Block A rate to ~0.5%.

#### Candidate selection: incumbent tracking
Added `best_candidate_idx: Option<usize>` to `RdsDecoder`. The incumbent (winning)
candidate can always update `best_state` at equal score (its `ps_seen`/`rt_seen`
arrays accumulate coherently). A challenger must achieve a strictly higher score to
take over. The incumbent's `best_score` is also updated when it returns `None`
(no state change) so challengers cannot leapfrog with a single false group.

#### Test fixes
- `blocks_to_chips`: added NRZI (NRZ-Mark) pre-encoding. The differential biphase
  decoder computes `bit = input_bit XOR prev_input_bit`; without NRZI the recovered
  bits were XOR-of-consecutive-bits, not the original data.
- `decode_block_soft_rejects_three_bit_error`: removed (OSD(2) legitimately finds
  distance-2 codewords; `pure_noise_produces_zero_pi_decodes` is the real guard).
- New test: `blocks_to_chips_round_trips_all_groups` — verifies round-trip decode
  of all 16 blocks across all 4 PS segments without BPSK modulation.

### `src/trx-server/trx-backend/trx-backend-soapysdr/src/demod/wfm.rs`

- `PILOT_LOCK_THRESHOLD = 0.20` (was 0.25) — pilot reference enabled at lower coherence
- Added `PILOT_LOCK_ONSET = 0.30` constant (was hardcoded 0.4)
- `pilot_lock` ramp: `((pilot_coherence - PILOT_LOCK_ONSET) / 0.2).clamp(0.0, 1.0)`
  — pilot reference engages at coherence ≥ 0.36 instead of ≥ 0.45

## Test Status

```
cargo test -p trx-rds
```

13/15 passing:
- ✅ decode_block_recognizes_valid_offsets
- ✅ decode_block_soft_corrects_single_bit_error
- ✅ decode_block_soft_corrects_two_bit_error_osd2
- ✅ block_decode_rate_osd1_vs_osd2
- ✅ decode_block_soft_prefers_least_costly_flip
- ✅ full_group_with_two_bit_errors_in_each_locked_block
- ✅ pi_accumulation_corrects_weak_pi_after_threshold
- ✅ decoder_emits_ps_and_pty_from_group_0a
- ✅ rrc_tap_dc_gain
- ✅ pure_noise_produces_zero_pi_decodes
- ✅ end_to_end_with_pilot_reference_decodes_pi
- ✅ end_to_end_noisy_signal_snr_10db_decodes_pi
- ✅ costas_tracks_without_diverging_on_clean_signal
- ✅ blocks_to_chips_round_trips_all_groups  ← new, proves chip encoding correct
- ❌ end_to_end_clean_signal_decodes_ps     ← remaining failure

## Remaining Bug: `end_to_end_clean_signal_decodes_ps`

### Symptom
The decoder sees segments 0 (×8 candidates) and 1 (×1), then jumps to segment 3,
skipping segment 2. `ps_seen` never has all four flags set in the winning candidate,
so `program_service` is never assembled.

### Diagnosis (from temporary `eprintln!` in `process_group`)
```
[DBG] process_group pi=0x9801 seg=0   (×8 — all 8 clock candidates decode seg 0)
[DBG] process_group pi=0x9801 seg=1   (×1)
[DBG] process_group pi=0x9801 seg=3   (×1 — seg 2 skipped!)
[DBG] process_group pi=0x9BB2 seg=3   (false positive)
```

Segment 2 is consistently skipped. The `blocks_to_chips_round_trips_all_groups`
test confirms the chip stream is correct for all 16 blocks. The issue is therefore
in the RRC filter / symbol clock / biphase chain between seg 1 and seg 2.

### Key observation
- `blocks_to_chips_round_trips_all_groups` passes — chip encoding is correct
- The FIR block size is 256 samples, introducing a 255-sample startup delay where
  the filter returns `(0.0, 0.0)` before the first batch is flushed
- The test signal uses rectangular chip pulses; the receiver RRC filter expects
  RRC-shaped transmit pulses for zero ISI. Rectangular × RRC ≠ raised cosine.

### Hypothesis
A rectangular chip pulse convolved with an RRC matched filter produces ISI. Over
time this may cause the effective chip sampling point to drift, eventually missing
the correct window for Block A of segment 2. `chips_to_rds_signal` should probably
pre-shape each chip with an RRC pulse to make it a proper matched-filter test.

### Next steps
1. Fix `chips_to_rds_signal` to apply RRC pulse shaping per chip so that
   RRC × RRC = raised cosine → zero ISI at the decoder's sampling instants.
2. Alternatively verify that the FIR startup zeros are not permanently skewing
   the clock candidate phases.
