# RDS Reception Improvements — WIP

Research into improving RDS demodulation robustness for two scenarios:
adjacent channel interference (ACI) and weak signal (low SNR).

## Signal Pipeline

```
Antenna → IF filter → FM discriminator → 57 kHz BPF → Costas PLL →
  Symbol timing → Manchester decode → Biphase decode →
  Block sync → (26,16) FEC → Group assembly
```

## Prioritized Implementation Plan

| # | Technique | Scenario | Complexity | Expected Gain |
|---|---|---|---|---|
| 1 | RRC matched filter | ACI | Low | Largest measured (32 vs 18/38 stations in empirical test) |
| 2 | 19 kHz pilot ×3 → 57 kHz carrier reference | Weak signal | Medium | 3–6 dB carrier phase noise reduction |
| 3 | Erasure declaration + erasure decoding | Weak signal | Low | 1–3 dB |
| 4 | 8th-order 57 kHz IIR bandpass filter | ACI | Low | Pre-filters ACI energy before Costas loop |
| 5 | Costas `tanh` soft phase detector | Weak signal | Trivial | ~1 dB |
| 6 | LLR accumulation across repeated groups | Weak signal | Low | √N SNR per repetition |
| 7 | Chase-II soft-decision block decoder | Both | Medium | 1–2 dB |
| 8 | OSD(2) block decoder | Both | Medium | 2–3 dB over hard-decision |
| 9 | IF CMA blind equalizer (pre-demodulation) | ACI | High | 7–10 dB ACI protection ratio improvement |

---

## Technique Notes

### 1. RRC Matched Filter

RDS uses known BPSK pulse shaping per IEC 62106. The receiver should apply a Root Raised
Cosine (RRC) matched filter rather than a generic FIR lowpass. The 2024 GNU Radio demodulator
comparison showed RRC-based decoders (Bloessl's gr-rds) decoded 32/38 real stations vs. 18/38
for plain FIR lowpass filter decoders — the single largest measured improvement.

- Roll-off factor: 1.0 per standard; experiment with 0.35–0.8 for sharper stopband
- Operate at 2375 Hz chip rate (Manchester doubles the symbol rate from 1187.5 baud)

### 2. 19 kHz Pilot ×3 Carrier Reference

Instead of running a Costas loop autonomously on the noisy 57 kHz subcarrier, multiply the
clean 19 kHz stereo pilot tone by 3 to produce a phase-coherent 57 kHz reference. The pilot
sits at much higher SNR than the RDS subcarrier. `redsea`'s own issue tracker identifies this
as the dominant weak-signal failure mode; patent CN113132039A addresses the same problem.

Without pilot lock, fall back to a Costas loop with a `tanh` soft phase detector (see #5).

### 3. Erasure Decoding

When `|LLR|` for a bit is below a confidence threshold, declare it an **erasure** (known-position
error) rather than a hard ±1 decision. The (26,16) RDS block code corrects:

- 5 random bit errors (hard decision)
- **10 erasures** — 2× improvement at no added decoder complexity

Requires propagating LLRs from the symbol detector to the syndrome decoder instead of hard
bits. The Group 0B PI cross-check (PI appears in both Block A and Block C) gives free
erasure resolution on the most critical field.

### 4. 8th-Order 57 kHz IIR Bandpass Filter

Hardware RDS chips (e.g. SAA6579) place an 8th-order bandpass filter at 57 kHz before the
carrier recovery stage. In software, an equivalent high-order IIR or long FIR centered at
57 kHz with ±4 kHz passband and steep roll-off attenuates adjacent-channel energy that bleeds
into the MPX spectrum after FM discrimination. Insert this stage immediately after FM
demodulation and before the Costas loop.

### 5. Costas `tanh` Soft Phase Detector

Replace the hard-slicer phase error `Re(z) * Im(z)` in the Costas loop with
`tanh(Re(z)/σ) * Im(z)`. This is the ML-derived phase error estimator and approaches the
Cramér-Rao bound at low SNR. Trivial code change, useful whenever the pilot reference (#2)
is unavailable.

### 6. LLR Accumulation Across Repeated Groups

RDS transmits the same data repeatedly (PI: every group every ~87.6 ms; PS name: ≥5 groups/sec).
Accumulate per-bit LLRs across N repetitions of the same field before decoding:

```
LLR_acc[i] += LLR_n[i]    // for known-repeated bit positions
```

SNR improves as √N (3 dB per 4× accumulations). With ~11 PI observations per second, 1 second
of accumulation is feasible before display latency becomes noticeable.

### 7. Chase-II Soft-Decision Block Decoder

The Chase-II algorithm generates `2^(2t)` hard-decision decoder trials by flipping the
`2t` least-reliable bit positions (identified by smallest `|LLR|`), then picks the trial with
minimum Euclidean metric. For the RDS (26,16) code with t=2: only 4 trials. The 26-bit block
size makes this extremely fast.

Expected gain: ~1–2 dB over hard-decision decoding.

### 8. OSD(2) Block Decoder

Ordered Statistics Decoding at order 2 approaches ML performance for short linear block codes.
Procedure for the (26,16) code:

1. Sort all 26 bit positions by `|LLR|` descending (most to least reliable).
2. Gaussian-eliminate the 10×26 parity-check matrix to bring the most reliable positions into
   systematic form.
3. Enumerate order-2 test patterns (all pairs from the least-reliable positions).
4. Select the minimum Euclidean-metric codeword.

The matrix is only 10×26 — Gaussian elimination is trivial. Expected gain: ~2–3 dB over
hard-decision decoding, ~0.5–1.5 dB over Chase-II.

Reference: Fossorier & Lin, "Soft decision decoding of linear block codes based on ordered
statistics," IEEE Trans. Inf. Theory, 1995.

### 9. IF CMA Blind Equalizer

FM signals are constant-envelope, so the Constant Modulus Algorithm can equalize the IF signal
without training data, driven by the cost `||s(t)|² - 1|`. A 2004 JSSC paper reports
**7–10 dB improvement in adjacent channel protection ratio** using a 6th-order blind equalizer
applied to the digitized IF signal before FM demodulation. This is the most powerful ACI
technique but requires operating at IF sample rates and handling the full pre-demodulation
signal. Implement last.

---

## Key References

- [site2241.net 2024 demodulator comparison](https://www.site2241.net/january2024.htm) — empirical RRC vs. FIR data
- [PySDR RDS end-to-end example](https://pysdr.org/content/rds.html) — complete Costas + M&M + block sync pipeline
- [gr-rds / Bloessl](https://github.com/alexmrqt/fm-rds) — best-performing open source implementation
- [redsea CHANGES.md](https://github.com/windytan/redsea/blob/master/CHANGES.md) — real-world weak signal bug history
- [Fossorier & Lin OSD](https://www.semanticscholar.org/paper/Soft-decision-decoding-of-linear-block-codes-based-Fossorier-Lin/2fde1414cd33dacfb96b7b0d5bbbe74b803704da) — foundational soft decoding for cyclic codes
- [IEEE: Digital RDS demodulation in FM subcarrier systems (2004)](https://ieeexplore.ieee.org/abstract/document/1412732)
- [ResearchGate: DSP-based digital IF AM/FM car radio receiver (JSSC 2004)](https://www.researchgate.net/publication/4050364_A_DSP-based_digital_if_AMFM_car-radio_receiver) — CMA equalizer, 7–10 dB ACI improvement
- [Information-Reduced Carrier Synchronization of BPSK/QPSK](https://www.researchgate.net/publication/254651395_Information-Reduced_Carrier_Synchronization_of_BPSK_and_QPSK_Using_Soft_Decision_Feedback)
- IEC 62106 (RDS standard), IEC 62634 (receiver measurements)
