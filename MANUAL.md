# trx-rs Manual

## SDR Noise Blanker

The noise blanker suppresses impulse noise (clicks, pops, ignition interference)
on raw IQ samples before any mixing or filtering takes place.  It works by
tracking a running RMS level of the signal and replacing any sample whose
magnitude exceeds **threshold x RMS** with the last known clean sample.

### Configuration (server-side)

The noise blanker is configured per rig.  In a multi-rig setup each
`[[rigs]]` entry has its own `[rigs.sdr.noise_blanker]` section:

```toml
[[rigs]]
id = "hf"

[rigs.rig]
type = "sdr"

[rigs.sdr.noise_blanker]
enabled = true
threshold = 10.0     # 1 – 100; lower = more aggressive blanking
```

For the legacy single-rig (flat) config the path is `[sdr.noise_blanker]`:

```toml
[sdr.noise_blanker]
enabled = true
threshold = 10.0
```

| Field       | Type  | Default | Range   | Description |
|-------------|-------|---------|---------|-------------|
| `enabled`   | bool  | false   | —       | Turn the noise blanker on or off. |
| `threshold` | float | 10.0    | 1 – 100 | Multiplier applied to the running RMS. A sample whose magnitude exceeds this multiple is replaced. Lower values blank more aggressively; higher values only catch strong impulses. |

The noise blanker is off by default.

### Choosing a threshold

The threshold controls how aggressively the blanker suppresses impulses.
A value of **N** means: blank any sample whose magnitude exceeds **N times**
the running average signal level.

| Threshold | Behavior | Use case |
|-----------|----------|----------|
| 3 – 5    | Very aggressive — blanks frequently | Dense impulse noise (motors, power lines, LED drivers nearby) |
| 8 – 12   | Moderate — catches clear spikes without touching normal signals | Typical HF conditions with occasional ignition or switching noise |
| 15 – 25  | Conservative — only blanks strong impulses well above the noise floor | Light interference, or when you want minimal artifacts on weak signals |
| 30 – 100 | Very light — rarely triggers | Faint, infrequent clicks; mostly a safety net |

**Start at 10** (the default) and adjust while listening:

- If impulse noise is still audible, lower the threshold.
- If weak signals sound choppy or distorted, raise it — the blanker may be
  mistaking signal peaks for noise.
- On bands with steady atmospheric noise (e.g. 160 m / 80 m), a threshold of
  **5 – 8** usually works well.
- On quieter VHF/UHF bands where the noise floor is low, values of **15 – 25**
  avoid false triggers from strong signals.

### Web UI

When the server reports noise-blanker support, two controls appear in the
**SDR Settings** row of the web interface:

- **Noise Blanker** checkbox — enables or disables the blanker in real time.
- **NB Threshold** number input (1–100) with a **Set** button — adjusts the
  detection threshold.  Press Enter or click Set to apply.

Both controls stay hidden until the server sends filter state containing NB
fields, so they only appear when connected to an SDR backend.

### HTTP API

```
POST /set_sdr_noise_blanker?enabled=true&threshold=10
```

| Parameter   | Type   | Required | Description |
|-------------|--------|----------|-------------|
| `enabled`   | bool   | yes      | `true` or `false` |
| `threshold` | float  | yes      | Value between 1 and 100 |

### How it works

The blanker runs on every IQ block (4096 samples) *before* the mixer stage in
the DSP pipeline:

1. For each sample, compute magnitude² (`re² + im²`).
2. Compare against `threshold² × mean_sq` (the exponentially-smoothed running
   mean of magnitude²).
3. If the sample exceeds the threshold, replace it with the previous clean
   sample.
4. Otherwise, update the running mean with smoothing factor α = 1/128 and store
   the sample as the last clean value.

Because the blanker operates on raw IQ before frequency translation, it removes
impulse noise across the entire captured bandwidth regardless of the tuned
channel offset.
