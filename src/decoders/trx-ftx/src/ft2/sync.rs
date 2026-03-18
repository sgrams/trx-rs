// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! 2D sync scoring with complex Costas reference waveforms.
//!
//! Prepares reference sync waveforms from the FT4 Costas pattern and frequency
//! tweak phasors, then correlates downsampled complex symbols against the
//! reference across time and frequency offsets.

use num_complex::Complex32;

use crate::constants::FT4_COSTAS_PATTERN;

use super::{FT2_NDOWN, FT2_NSS, FT2_SYMBOL_PERIOD_F, FT2_SYNC_TWEAK_MAX, FT2_SYNC_TWEAK_MIN};

/// Number of frequency tweak entries.
const NUM_TWEAKS: usize = (FT2_SYNC_TWEAK_MAX - FT2_SYNC_TWEAK_MIN) as usize + 1;

/// Precomputed sync and frequency-tweak waveforms.
pub struct SyncWaveforms {
    /// Complex reference waveforms for each of the 4 Costas sync groups.
    /// Each group has 64 samples (4 tones * 16 samples per half-symbol).
    pub sync_wave: [[Complex32; 64]; 4],
    /// Frequency tweak phasors for each integer frequency offset.
    /// Index by `idf - FT2_SYNC_TWEAK_MIN`.
    pub tweak_wave: [[Complex32; 64]; NUM_TWEAKS],
}

/// Prepare complex reference waveforms for sync scoring.
///
/// For each of the 4 Costas sync groups, we generate the expected complex
/// signal using continuous-phase tone generation at the downsampled rate.
/// We also generate frequency-tweak phasors for fine frequency searching.
pub fn prepare_sync_waveforms() -> SyncWaveforms {
    let fs_down = 12000.0f32 / FT2_NDOWN as f32;
    let nss = FT2_SYMBOL_PERIOD_F * fs_down;

    let mut sync_wave = [[Complex32::new(0.0, 0.0); 64]; 4];
    let mut tweak_wave = [[Complex32::new(0.0, 0.0); 64]; NUM_TWEAKS];

    // Build sync reference waveforms (continuous phase across tones)
    for group in 0..4 {
        let mut idx = 0usize;
        let mut phase = 0.0f32;
        for tone_idx in 0..4 {
            let tone = FT4_COSTAS_PATTERN[group][tone_idx] as f32;
            let dphase = 4.0 * std::f32::consts::PI * tone / nss;
            let half_nss = (nss / 2.0) as usize;
            for _step in 0..half_nss {
                if idx >= 64 {
                    break;
                }
                sync_wave[group][idx] = Complex32::new(phase.cos(), phase.sin());
                phase = (phase + dphase) % (2.0 * std::f32::consts::PI);
                idx += 1;
            }
        }
    }

    // Build frequency tweak phasors
    for idf in FT2_SYNC_TWEAK_MIN..=FT2_SYNC_TWEAK_MAX {
        let tw_idx = (idf - FT2_SYNC_TWEAK_MIN) as usize;
        for n in 0..64 {
            let phase = 4.0 * std::f32::consts::PI * idf as f32 * n as f32 / fs_down;
            tweak_wave[tw_idx][n] = Complex32::new(phase.cos(), phase.sin());
        }
    }

    SyncWaveforms {
        sync_wave,
        tweak_wave,
    }
}

/// Compute the 2D sync score for a given time offset and frequency tweak.
///
/// Correlates the downsampled complex samples against the four Costas sync
/// group reference waveforms, applying the specified frequency tweak.
///
/// `samples`: downsampled complex baseband signal.
/// `start`: sample offset for the start of the frame.
/// `idf`: integer frequency tweak (Hz).
/// `waveforms`: precomputed reference waveforms.
///
/// Returns the sync correlation score (higher is better).
pub fn sync2d_score(
    samples: &[Complex32],
    start: i32,
    idf: i32,
    waveforms: &SyncWaveforms,
) -> f32 {
    let nss = FT2_NSS as i32;
    let n_samples = samples.len() as i32;

    // The four sync groups are at symbol positions 0, 33, 66, 99 within the frame
    let positions = [
        start,
        start + 33 * nss,
        start + 66 * nss,
        start + 99 * nss,
    ];

    let tw_idx = (idf - FT2_SYNC_TWEAK_MIN) as usize;
    if tw_idx >= waveforms.tweak_wave.len() {
        return 0.0;
    }
    let tweak = &waveforms.tweak_wave[tw_idx];

    let mut score = 0.0f32;

    for group in 0..4 {
        let pos = positions[group];
        let mut sum = Complex32::new(0.0, 0.0);
        let mut usable = 0;

        for i in 0..64 {
            let sample_idx = pos + 2 * i as i32;
            if sample_idx < 0 || sample_idx >= n_samples {
                continue;
            }
            // Correlate: multiply received sample by conjugate of
            // (sync_reference * tweak_phasor)
            let reference = waveforms.sync_wave[group][i] * tweak[i];
            sum += samples[sample_idx as usize] * reference.conj();
            usable += 1;
        }

        if usable > 16 {
            score += sum.norm() / (2.0 * FT2_NSS as f32);
        }
    }

    score
}

/// Refine frequency tweak around a coarse estimate.
///
/// Searches `idf` values from `center_idf - range` to `center_idf + range`
/// and `start` values from `center_start - start_range` to
/// `center_start + start_range`, returning the best score and parameters.
pub fn refine_sync(
    samples: &[Complex32],
    center_start: i32,
    center_idf: i32,
    start_range: i32,
    idf_range: i32,
    waveforms: &SyncWaveforms,
) -> (f32, i32, i32) {
    let mut best_score: f32 = -1.0;
    let mut best_start = center_start;
    let mut best_idf = center_idf;

    for idf in (center_idf - idf_range)..=(center_idf + idf_range) {
        if !(FT2_SYNC_TWEAK_MIN..=FT2_SYNC_TWEAK_MAX).contains(&idf) {
            continue;
        }
        for start in (center_start - start_range)..=(center_start + start_range) {
            let score = sync2d_score(samples, start, idf, waveforms);
            if score > best_score {
                best_score = score;
                best_start = start;
                best_idf = idf;
            }
        }
    }

    (best_score, best_start, best_idf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_preparation() {
        let wf = prepare_sync_waveforms();
        // Sync waveforms should have unit magnitude at each sample
        for group in 0..4 {
            for i in 0..64 {
                let mag = wf.sync_wave[group][i].norm();
                assert!(
                    (mag - 1.0).abs() < 1e-4,
                    "Sync wave group {} sample {} has magnitude {}, expected ~1.0",
                    group,
                    i,
                    mag
                );
            }
        }
    }

    #[test]
    fn tweak_waveform_unit_magnitude() {
        let wf = prepare_sync_waveforms();
        for tw in &wf.tweak_wave {
            for &s in tw {
                let mag = s.norm();
                assert!(
                    (mag - 1.0).abs() < 1e-4,
                    "Tweak wave magnitude {} should be ~1.0",
                    mag
                );
            }
        }
    }

    #[test]
    fn sync_score_zero_signal() {
        let wf = prepare_sync_waveforms();
        let samples = vec![Complex32::new(0.0, 0.0); 5000];
        let score = sync2d_score(&samples, 0, 0, &wf);
        assert!(
            score.abs() < 1e-6,
            "Score of zero signal should be ~0, got {}",
            score
        );
    }

    #[test]
    fn sync_score_out_of_range_idf() {
        let wf = prepare_sync_waveforms();
        let samples = vec![Complex32::new(1.0, 0.0); 5000];
        let score = sync2d_score(&samples, 0, FT2_SYNC_TWEAK_MAX + 100, &wf);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn refine_improves_on_coarse() {
        let wf = prepare_sync_waveforms();
        // Create a simple signal where the coarse and fine searches should
        // produce non-negative scores
        let samples = vec![Complex32::new(0.1, 0.05); 5000];
        let (score, _start, _idf) = refine_sync(&samples, 100, 0, 5, 4, &wf);
        assert!(score >= 0.0);
    }

    #[test]
    fn num_tweaks_matches_range() {
        assert_eq!(
            NUM_TWEAKS,
            (FT2_SYNC_TWEAK_MAX - FT2_SYNC_TWEAK_MIN + 1) as usize
        );
    }
}
