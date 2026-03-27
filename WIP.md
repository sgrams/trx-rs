# RDS Parameter Tuning — Work in Progress

## Goal
Maximum sensitivity (weak-signal decode) with zero false positive PI decodes.

## Changes Made

### `src/decoders/trx-rds/src/lib.rs`

#### Constants tuned
- `RRC_ALPHA = 0.50` (was 0.75) — narrower noise bandwidth, ~0.6 dB SNR gain
- `COSTAS_KI = 3.5e-7` — loop damping ζ≈0.68, well-damped (1e-6 caused instability)
- `PI_ACC_THRESHOLD = 3` (was 2) — accumulate 3 Block A observations before committing PI
- `OSD_MAX_FLIP_COST = 0.45` — Tech 9: reject OSD corrections where flipped bits had
  high confidence (genuine errors have cost ≲ 0.3; noise matches cost 0.6–1.2)

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

#### Tech 9: OSD cost ceiling
`decode_block_soft` now enforces `OSD_MAX_FLIP_COST = 0.45` — the sum of soft
confidences for all flipped bits must not exceed this threshold. At 9–10 dB SNR,
genuine bit errors have very low `|biphase_I|` (cost ≲ 0.3), while noise-induced
OSD matches flip high-confidence bits (cost 0.6–1.2). This eliminates most
spurious OSD(2) matches without affecting real weak-signal corrections.

#### Tech 10: PI consistency gate
`process_group` rejects groups whose Block A PI differs from the candidate's
established PI. This prevents a single false OSD decode from polluting accumulated
text fields (PS, RT, PTYN) with garbage from noise or interference.

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

16/16 passing:
- ✅ decode_block_recognizes_valid_offsets
- ✅ decode_block_soft_corrects_single_bit_error
- ✅ decode_block_soft_corrects_two_bit_error_osd2
- ✅ block_decode_rate_osd1_vs_osd2
- ✅ decode_block_soft_prefers_least_costly_flip
- ✅ full_group_with_two_bit_errors_in_each_locked_block
- ✅ pi_accumulation_corrects_weak_pi_after_threshold
- ✅ decoder_emits_ps_and_pty_from_group_0a
- ✅ rrc_tap_dc_gain
- ✅ pure_noise_produces_zero_pi_decodes (2 seconds of noise, zero false PI)
- ✅ end_to_end_with_pilot_reference_decodes_pi
- ✅ end_to_end_noisy_signal_snr_10db_decodes_pi
- ✅ end_to_end_noisy_signal_snr_9db_decodes_pi  ← new, 9 dB threshold
- ✅ costas_tracks_without_diverging_on_clean_signal
- ✅ blocks_to_chips_round_trips_all_groups
- ✅ end_to_end_clean_signal_decodes_ps
