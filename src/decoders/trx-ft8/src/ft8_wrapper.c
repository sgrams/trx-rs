// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#include <ft8/decode.h>
#include <ft8/ldpc.h>
#include <ft8/crc.h>
#include <ft8/message.h>
#include <ft8/text.h>
#define LOG_LEVEL LOG_INFO
#include <ft8/debug.h>
#include <common/monitor.h>
#include <fft/kiss_fftr.h>
#include <fft/kiss_fft.h>

#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <math.h>
#include <complex.h>

enum
{
    TRX_FTX_PROTOCOL_FT4 = 0,
    TRX_FTX_PROTOCOL_FT8 = 1,
    TRX_FTX_PROTOCOL_FT2 = 2,
};

// Callsign hash table (from demo/decode_ft8.c)
#define CALLSIGN_HASHTABLE_SIZE 256

typedef struct
{
    uint32_t hash;
    char callsign[12];
} callsign_hashtable_entry_t;

static callsign_hashtable_entry_t callsign_hashtable[CALLSIGN_HASHTABLE_SIZE];
static int callsign_hashtable_size = 0;

static void hashtable_init(void)
{
    callsign_hashtable_size = 0;
    memset(callsign_hashtable, 0, sizeof(callsign_hashtable));
}

static void hashtable_cleanup(uint8_t max_age)
{
    for (int idx_hash = 0; idx_hash < CALLSIGN_HASHTABLE_SIZE; ++idx_hash)
    {
        if (callsign_hashtable[idx_hash].callsign[0] != '\0')
        {
            uint8_t age = (uint8_t)(callsign_hashtable[idx_hash].hash >> 24);
            if (age >= max_age)
            {
                callsign_hashtable[idx_hash].callsign[0] = '\0';
                callsign_hashtable[idx_hash].hash = 0;
                callsign_hashtable_size--;
            }
            else
            {
                callsign_hashtable[idx_hash].hash = (((uint32_t)age + 1u) << 24) | (callsign_hashtable[idx_hash].hash & 0x3FFFFFu);
            }
        }
    }
}

static void hashtable_add(const char* callsign, uint32_t hash)
{
    int idx_hash = hash % CALLSIGN_HASHTABLE_SIZE;
    while (callsign_hashtable[idx_hash].callsign[0] != '\0')
    {
        if (((callsign_hashtable[idx_hash].hash & 0x3FFFFFu) == hash) && (0 == strcmp(callsign_hashtable[idx_hash].callsign, callsign)))
        {
            callsign_hashtable[idx_hash].hash &= 0x3FFFFFu;
            return;
        }
        idx_hash = (idx_hash + 1) % CALLSIGN_HASHTABLE_SIZE;
    }
    callsign_hashtable_size++;
    strncpy(callsign_hashtable[idx_hash].callsign, callsign, 11);
    callsign_hashtable[idx_hash].callsign[11] = '\0';
    callsign_hashtable[idx_hash].hash = hash;
}

static bool hashtable_lookup(ftx_callsign_hash_type_t hash_type, uint32_t hash, char* callsign)
{
    int hash_shift = (hash_type == FTX_CALLSIGN_HASH_22_BITS) ? 0 : (hash_type == FTX_CALLSIGN_HASH_12_BITS) ? 10 : 12;
    uint32_t mask = (hash_type == FTX_CALLSIGN_HASH_22_BITS) ? 0x3FFFFFu : (hash_type == FTX_CALLSIGN_HASH_12_BITS) ? 0xFFFu : 0x3FFu;

    int idx_hash = hash % CALLSIGN_HASHTABLE_SIZE;
    while (callsign_hashtable[idx_hash].callsign[0] != '\0')
    {
        if (((callsign_hashtable[idx_hash].hash & 0x3FFFFFu) >> hash_shift) == (hash & mask))
        {
            strcpy(callsign, callsign_hashtable[idx_hash].callsign);
            return true;
        }
        idx_hash = (idx_hash + 1) % CALLSIGN_HASHTABLE_SIZE;
    }
    callsign[0] = '\0';
    return false;
}

static ftx_callsign_hash_interface_t hash_if = {
    .lookup_hash = hashtable_lookup,
    .save_hash = hashtable_add,
};

static bool ft2_unpack_message(const uint8_t plain174[], ftx_message_t* message);

// Decoder wrapper

typedef struct
{
    monitor_t mon;
    monitor_config_t cfg;
    float* ft2_raw;
    int ft2_raw_capacity;
    int ft2_raw_len;
} ft8_decoder_t;

typedef struct
{
    char text[FTX_MAX_MESSAGE_LENGTH];
    float snr_db;
    float dt_s;
    float freq_hz;
} ft8_decode_result_t;

static float ft2_frequency_offset_hz(void)
{
    return -1.5f / FT2_SYMBOL_PERIOD;
}

static float decoder_candidate_freq_hz(const ft8_decoder_t* dec, const ftx_candidate_t* cand)
{
    const ftx_waterfall_t* wf = &dec->mon.wf;
    float freq_hz = (dec->mon.min_bin + cand->freq_offset + (float)cand->freq_sub / wf->freq_osr) / dec->mon.symbol_period;
    if (dec->cfg.protocol == FTX_PROTOCOL_FT2)
    {
        freq_hz += ft2_frequency_offset_hz();
    }
    return freq_hz;
}

static float decoder_candidate_dt_s(const ft8_decoder_t* dec, const ftx_candidate_t* cand)
{
    const ftx_waterfall_t* wf = &dec->mon.wf;
    float time_sec = (cand->time_offset + (float)cand->time_sub / wf->time_osr) * dec->mon.symbol_period;
    if (dec->cfg.protocol == FTX_PROTOCOL_FT2)
    {
        time_sec -= 0.5f;
    }
    return time_sec;
}

#define FT2_NDOWN 9
#define FT2_NFFT1 1152
#define FT2_NH1   (FT2_NFFT1 / 2)
#define FT2_NSTEP 288
#define FT2_NMAX 45000
#define FT2_MAX_RAW_CANDIDATES 96
#define FT2_MAX_SCAN_HITS 128
#define FT2_SYNC_TWEAK_MIN (-16)
#define FT2_SYNC_TWEAK_MAX (16)
#define FT2_NSS (FT2_NSTEP / FT2_NDOWN)
#define FT2_FRAME_SYMBOLS (FT2_NN - FT2_NR)
#define FT2_FRAME_SAMPLES (FT2_FRAME_SYMBOLS * FT2_NSS)

typedef struct
{
    float freq_hz;
    float score;
} ft2_raw_candidate_t;

typedef struct
{
    int index;
    float reliability;
} ft2_reliability_t;

typedef struct
{
    int peaks_found;
    int hits_found;
    float best_peak_score;
    float best_sync_score;
} ft2_scan_stats_t;

typedef struct
{
    int ntype[5];
    int nharderror[5];
    float dmin[5];
} ft2_pass_diag_t;

typedef struct
{
    int nraw;
    int nfft2;
    float df;
    float* window;
    kiss_fft_cpx* spectrum;
    kiss_fft_cpx* band;
    kiss_fft_cfg ifft_cfg;
    void* ifft_mem;
} ft2_downsample_ctx_t;

typedef enum
{
    FT2_FAIL_NONE = 0,
    FT2_FAIL_REFINED_SYNC,
    FT2_FAIL_FREQ_RANGE,
    FT2_FAIL_FINAL_DOWNSAMPLE,
    FT2_FAIL_BITMETRICS,
    FT2_FAIL_SYNC_QUAL,
    FT2_FAIL_LDPC,
    FT2_FAIL_CRC,
    FT2_FAIL_UNPACK
} ft2_fail_stage_t;

typedef struct
{
    float freq_hz;
    float snr0;
    float sync_score;
    int start;
    int idf;
} ft2_scan_hit_t;

static void ft2_nuttall_window(float* window, int n)
{
    const float a0 = 0.355768f;
    const float a1 = 0.487396f;
    const float a2 = 0.144232f;
    const float a3 = 0.012604f;
    for (int i = 0; i < n; ++i)
    {
        float phase = (2.0f * (float)M_PI * i) / (float)(n - 1);
        window[i] = a0 - a1 * cosf(phase) + a2 * cosf(2.0f * phase) - a3 * cosf(3.0f * phase);
    }
}

static int ft2_cmp_candidates_desc(const void* lhs, const void* rhs)
{
    const ft2_raw_candidate_t* a = (const ft2_raw_candidate_t*)lhs;
    const ft2_raw_candidate_t* b = (const ft2_raw_candidate_t*)rhs;
    if (a->score < b->score)
        return 1;
    if (a->score > b->score)
        return -1;
    return 0;
}

static int ft2_cmp_reliability_asc(const void* lhs, const void* rhs)
{
    const ft2_reliability_t* a = (const ft2_reliability_t*)lhs;
    const ft2_reliability_t* b = (const ft2_reliability_t*)rhs;
    if (a->reliability < b->reliability)
        return -1;
    if (a->reliability > b->reliability)
        return 1;
    return 0;
}

static int ft2_cmp_scan_hits_desc(const void* lhs, const void* rhs)
{
    const ft2_scan_hit_t* a = (const ft2_scan_hit_t*)lhs;
    const ft2_scan_hit_t* b = (const ft2_scan_hit_t*)rhs;
    if (a->sync_score < b->sync_score)
        return 1;
    if (a->sync_score > b->sync_score)
        return -1;
    return 0;
}

static int ft2_find_frequency_peaks(
    const ft8_decoder_t* dec,
    ft2_raw_candidate_t* candidates,
    int max_candidates)
{
    if (!dec->ft2_raw || dec->ft2_raw_len < FT2_NFFT1)
        return 0;

    const float fs = (float)dec->cfg.sample_rate;
    const float df = fs / FT2_NFFT1;
    const int n_frames = 1 + (dec->ft2_raw_len - FT2_NFFT1) / FT2_NSTEP;
    float* avg = (float*)calloc(FT2_NH1, sizeof(float));
    float* smooth = (float*)calloc(FT2_NH1, sizeof(float));
    float* baseline = (float*)calloc(FT2_NH1, sizeof(float));
    float* window = (float*)malloc(sizeof(float) * FT2_NFFT1);
    kiss_fft_scalar* timebuf = (kiss_fft_scalar*)malloc(sizeof(kiss_fft_scalar) * FT2_NFFT1);
    kiss_fft_cpx* freqbuf = (kiss_fft_cpx*)malloc(sizeof(kiss_fft_cpx) * (FT2_NH1 + 1));
    if (!avg || !smooth || !baseline || !window || !timebuf || !freqbuf)
    {
        free(avg);
        free(smooth);
        free(baseline);
        free(window);
        free(timebuf);
        free(freqbuf);
        return 0;
    }

    ft2_nuttall_window(window, FT2_NFFT1);
    size_t fft_mem_len = 0;
    kiss_fftr_alloc(FT2_NFFT1, 0, NULL, &fft_mem_len);
    void* fft_mem = malloc(fft_mem_len);
    kiss_fftr_cfg fft_cfg = kiss_fftr_alloc(FT2_NFFT1, 0, fft_mem, &fft_mem_len);
    if (!fft_cfg)
    {
        free(avg);
        free(smooth);
        free(baseline);
        free(window);
        free(timebuf);
        free(freqbuf);
        free(fft_mem);
        return 0;
    }

    for (int frame = 0; frame < n_frames; ++frame)
    {
        int start = frame * FT2_NSTEP;
        for (int i = 0; i < FT2_NFFT1; ++i)
        {
            timebuf[i] = dec->ft2_raw[start + i] * window[i];
        }
        kiss_fftr(fft_cfg, timebuf, freqbuf);
        for (int bin = 1; bin < FT2_NH1; ++bin)
        {
            float power = freqbuf[bin].r * freqbuf[bin].r + freqbuf[bin].i * freqbuf[bin].i;
            avg[bin] += power;
        }
    }

    for (int bin = 1; bin < FT2_NH1; ++bin)
    {
        avg[bin] /= (float)n_frames;
    }
    for (int bin = 8; bin < FT2_NH1 - 8; ++bin)
    {
        float sum = 0.0f;
        for (int i = bin - 7; i <= bin + 7; ++i)
            sum += avg[i];
        smooth[bin] = sum / 15.0f;
    }
    for (int bin = 32; bin < FT2_NH1 - 32; ++bin)
    {
        float sum = 0.0f;
        for (int i = bin - 31; i <= bin + 31; ++i)
            sum += smooth[i];
        baseline[bin] = (sum / 63.0f) + 1e-9f;
    }

    const int min_bin = (int)lroundf(200.0f / df);
    const int max_bin = (int)lroundf(4910.0f / df);
    int count = 0;
    for (int bin = min_bin + 1; bin < max_bin - 1 && count < max_candidates; ++bin)
    {
        if (baseline[bin] <= 0.0f)
            continue;
        float value = smooth[bin] / baseline[bin];
        if (value < 1.08f)
            continue;
        if (!(value >= (smooth[bin - 1] / fmaxf(baseline[bin - 1], 1e-9f)) &&
              value >= (smooth[bin + 1] / fmaxf(baseline[bin + 1], 1e-9f))))
            continue;

        float left = smooth[bin - 1] / fmaxf(baseline[bin - 1], 1e-9f);
        float right = smooth[bin + 1] / fmaxf(baseline[bin + 1], 1e-9f);
        float den = left - 2.0f * value + right;
        float delta = (fabsf(den) > 1e-6f) ? (0.5f * (left - right) / den) : 0.0f;
        float freq_hz = (bin + delta) * df + ft2_frequency_offset_hz();
        if (freq_hz < 200.0f || freq_hz > 4910.0f)
            continue;
        candidates[count].freq_hz = freq_hz;
        candidates[count].score = value;
        ++count;
    }

    qsort(candidates, count, sizeof(candidates[0]), ft2_cmp_candidates_desc);

    free(avg);
    free(smooth);
    free(baseline);
    free(window);
    free(timebuf);
    free(freqbuf);
    free(fft_mem);
    return count;
}

static void ft2_prepare_sync_waveforms(float complex sync_wave[4][64], float complex tweak_wave[33][64])
{
    const float fs_down = 12000.0f / FT2_NDOWN;
    const float nss = FT2_SYMBOL_PERIOD * fs_down;
    for (int group = 0; group < 4; ++group)
    {
        int idx = 0;
        float phase = 0.0f;
        for (int tone_idx = 0; tone_idx < 4; ++tone_idx)
        {
            int tone = kFT4_Costas_pattern[group][tone_idx];
            float dphase = 4.0f * (float)M_PI * tone / nss;
            for (int step = 0; step < (int)(nss / 2.0f); ++step)
            {
                sync_wave[group][idx++] = cexpf(I * phase);
                phase = fmodf(phase + dphase, 2.0f * (float)M_PI);
            }
        }
    }

    for (int idf = FT2_SYNC_TWEAK_MIN; idf <= FT2_SYNC_TWEAK_MAX; ++idf)
    {
        for (int n = 0; n < 64; ++n)
        {
            float phase = 4.0f * (float)M_PI * idf * n / fs_down;
            tweak_wave[idf - FT2_SYNC_TWEAK_MIN][n] = cexpf(I * phase);
        }
    }
}

static int ft2_downsample_candidate(
    const ft2_downsample_ctx_t* ctx,
    float freq_hz,
    float complex* out_samples,
    int out_len)
{
    if (!ctx || !ctx->spectrum || !ctx->window || !ctx->band || !ctx->ifft_cfg)
        return 0;

    const int nraw = ctx->nraw;
    const int nfft2 = ctx->nfft2;
    if (nraw <= 0 || out_len < nfft2)
        return 0;

    memset(ctx->band, 0, sizeof(kiss_fft_cpx) * nfft2);
    const int i0 = (int)lroundf(freq_hz / ctx->df);
    if (i0 >= 0 && i0 <= nraw / 2)
        ctx->band[0] = ctx->spectrum[i0];
    for (int i = 1; i <= nfft2 / 2; ++i)
    {
        if ((i0 + i) >= 0 && (i0 + i) <= nraw / 2)
            ctx->band[i] = ctx->spectrum[i0 + i];
        if ((i0 - i) >= 0 && (i0 - i) <= nraw / 2)
            ctx->band[nfft2 - i] = ctx->spectrum[i0 - i];
    }

    for (int i = 0; i < nfft2; ++i)
    {
        ctx->band[i].r = ctx->band[i].r * ctx->window[i] / nfft2;
        ctx->band[i].i = ctx->band[i].i * ctx->window[i] / nfft2;
    }
    kiss_fft(ctx->ifft_cfg, ctx->band, ctx->band);
    for (int i = 0; i < nfft2; ++i)
        out_samples[i] = ctx->band[i].r + I * ctx->band[i].i;

    return nfft2;
}

static void ft2_downsample_ctx_free(ft2_downsample_ctx_t* ctx)
{
    if (!ctx)
        return;
    free(ctx->window);
    free(ctx->spectrum);
    free(ctx->band);
    free(ctx->ifft_mem);
    memset(ctx, 0, sizeof(*ctx));
}

static bool ft2_downsample_ctx_init(const ft8_decoder_t* dec, ft2_downsample_ctx_t* ctx)
{
    if (!dec || !ctx || !dec->ft2_raw)
        return false;

    memset(ctx, 0, sizeof(*ctx));
    ctx->nraw = dec->ft2_raw_len;
    ctx->nfft2 = ctx->nraw / FT2_NDOWN;
    if (ctx->nraw <= 0 || ctx->nfft2 <= 0)
        return false;

    ctx->df = (float)dec->cfg.sample_rate / ctx->nraw;
    ctx->window = (float*)calloc(ctx->nfft2, sizeof(float));
    ctx->spectrum = (kiss_fft_cpx*)malloc(sizeof(kiss_fft_cpx) * (ctx->nraw / 2 + 1));
    ctx->band = (kiss_fft_cpx*)malloc(sizeof(kiss_fft_cpx) * ctx->nfft2);
    if (!ctx->window || !ctx->spectrum || !ctx->band)
    {
        ft2_downsample_ctx_free(ctx);
        return false;
    }

    const float baud = 1.0f / FT2_SYMBOL_PERIOD;
    const int iwt = (int)((0.5f * baud) / ctx->df);
    const int iwf = (int)((4.0f * baud) / ctx->df);
    const int iws = (int)(baud / ctx->df);
    if (iwt <= 0)
    {
        ft2_downsample_ctx_free(ctx);
        return false;
    }
    for (int i = 0; i < iwt && i < ctx->nfft2; ++i)
        ctx->window[i] = 0.5f * (1.0f + cosf((float)M_PI * (float)(iwt - 1 - i) / (float)iwt));
    for (int i = iwt; i < iwt + iwf && i < ctx->nfft2; ++i)
        ctx->window[i] = 1.0f;
    for (int i = iwt + iwf; i < 2 * iwt + iwf && i < ctx->nfft2; ++i)
        ctx->window[i] = 0.5f * (1.0f + cosf((float)M_PI * (float)(i - (iwt + iwf)) / (float)iwt));
    if (iws > 0)
    {
        float* shifted = (float*)calloc(ctx->nfft2, sizeof(float));
        if (!shifted)
        {
            ft2_downsample_ctx_free(ctx);
            return false;
        }
        for (int i = 0; i < ctx->nfft2; ++i)
            shifted[i] = ctx->window[(i + iws) % ctx->nfft2];
        memcpy(ctx->window, shifted, sizeof(float) * ctx->nfft2);
        free(shifted);
    }

    kiss_fft_scalar* timedata = (kiss_fft_scalar*)malloc(sizeof(kiss_fft_scalar) * ctx->nraw);
    if (!timedata)
    {
        ft2_downsample_ctx_free(ctx);
        return false;
    }

    size_t rfft_mem_len = 0;
    kiss_fftr_alloc(ctx->nraw, 0, NULL, &rfft_mem_len);
    void* rfft_mem = malloc(rfft_mem_len);
    kiss_fftr_cfg rfft_cfg = kiss_fftr_alloc(ctx->nraw, 0, rfft_mem, &rfft_mem_len);
    if (!rfft_cfg)
    {
        free(timedata);
        free(rfft_mem);
        ft2_downsample_ctx_free(ctx);
        return false;
    }
    for (int i = 0; i < ctx->nraw; ++i)
        timedata[i] = dec->ft2_raw[i];
    kiss_fftr(rfft_cfg, timedata, ctx->spectrum);
    free(timedata);
    free(rfft_mem);

    size_t ifft_mem_len = 0;
    kiss_fft_alloc(ctx->nfft2, 1, NULL, &ifft_mem_len);
    ctx->ifft_mem = malloc(ifft_mem_len);
    ctx->ifft_cfg = kiss_fft_alloc(ctx->nfft2, 1, ctx->ifft_mem, &ifft_mem_len);
    if (!ctx->ifft_cfg)
    {
        ft2_downsample_ctx_free(ctx);
        return false;
    }

    return true;
}

static float ft2_sync2d_score(
    const float complex* samples,
    int n_samples,
    int start,
    int idf,
    const float complex sync_wave[4][64],
    const float complex tweak_wave[33][64])
{
    const int nss = FT2_NSS;
    const int positions[4] = {
        start,
        start + 33 * nss,
        start + 66 * nss,
        start + 99 * nss,
    };
    float score = 0.0f;
    const float complex* tweak = tweak_wave[idf - FT2_SYNC_TWEAK_MIN];

    for (int group = 0; group < 4; ++group)
    {
        int pos = positions[group];
        float complex sum = 0.0f;
        int usable = 0;
        for (int i = 0; i < 64; ++i)
        {
            int sample_idx = pos + 2 * i;
            if (sample_idx < 0 || sample_idx >= n_samples)
                continue;
            sum += samples[sample_idx] * conjf(sync_wave[group][i] * tweak[i]);
            ++usable;
        }
        if (usable > 16)
            score += cabsf(sum) / (2.0f * nss);
    }
    return score;
}

static void ft2_normalize_downsampled(float complex* samples, int n_samples, int ref_count)
{
    float power = 0.0f;
    for (int i = 0; i < n_samples; ++i)
    {
        power += crealf(samples[i] * conjf(samples[i]));
    }
    if (power <= 0.0f)
        return;
    if (ref_count <= 0)
        ref_count = n_samples;
    float scale = sqrtf((float)ref_count / power);
    for (int i = 0; i < n_samples; ++i)
    {
        samples[i] *= scale;
    }
}

static int ft2_find_scan_hits(
    const ft8_decoder_t* dec,
    const ft2_downsample_ctx_t* downsample_ctx,
    ft2_scan_hit_t* out,
    int max_hits,
    ft2_scan_stats_t* stats)
{
    if (!downsample_ctx)
        return 0;

    ft2_raw_candidate_t peaks[FT2_MAX_RAW_CANDIDATES];
    int n_peaks = ft2_find_frequency_peaks(dec, peaks, FT2_MAX_RAW_CANDIDATES);
    if (stats)
    {
        stats->peaks_found = n_peaks;
        stats->hits_found = 0;
        stats->best_peak_score = 0.0f;
        stats->best_sync_score = 0.0f;
        for (int i = 0; i < n_peaks; ++i)
        {
            if (peaks[i].score > stats->best_peak_score)
                stats->best_peak_score = peaks[i].score;
        }
    }
    if (n_peaks <= 0)
        return 0;

    const int nfft2 = downsample_ctx ? downsample_ctx->nfft2 : 0;
    float complex* down = (float complex*)malloc(sizeof(float complex) * nfft2);
    float complex sync_wave[4][64];
    float complex tweak_wave[33][64];
    ft2_prepare_sync_waveforms(sync_wave, tweak_wave);

    int count = 0;
    for (int peak = 0; peak < n_peaks && count < max_hits; ++peak)
    {
        int produced = ft2_downsample_candidate(downsample_ctx, peaks[peak].freq_hz, down, nfft2);
        if (produced <= 0)
            continue;
        ft2_normalize_downsampled(down, produced, produced);

        float best_score = -1.0f;
        int best_start = 0;
        int best_idf = 0;
        for (int idf = -12; idf <= 12; idf += 3)
        {
            for (int start = -688; start <= 2024; start += 4)
            {
                float score = ft2_sync2d_score(down, produced, start, idf, sync_wave, tweak_wave);
                if (score > best_score)
                {
                    best_score = score;
                    best_start = start;
                    best_idf = idf;
                }
            }
        }
        if (best_score < 0.60f)
            continue;

        for (int idf = best_idf - 4; idf <= best_idf + 4; ++idf)
        {
            if (idf < FT2_SYNC_TWEAK_MIN || idf > FT2_SYNC_TWEAK_MAX)
                continue;
            for (int start = best_start - 5; start <= best_start + 5; ++start)
            {
                float score = ft2_sync2d_score(down, produced, start, idf, sync_wave, tweak_wave);
                if (score > best_score)
                {
                    best_score = score;
                    best_start = start;
                    best_idf = idf;
                }
            }
        }
        if (best_score < 0.60f)
            continue;

        out[count].freq_hz = peaks[peak].freq_hz;
        out[count].snr0 = peaks[peak].score - 1.0f;
        out[count].sync_score = best_score;
        out[count].start = best_start;
        out[count].idf = best_idf;
        if (stats && best_score > stats->best_sync_score)
            stats->best_sync_score = best_score;
        ++count;
    }

    qsort(out, count, sizeof(out[0]), ft2_cmp_scan_hits_desc);
    if (stats)
        stats->hits_found = count;
    free(down);
    return count;
}

static void ft2_extract_signal_region(const float complex* input, int input_len, int start, float complex* out_signal)
{
    for (int i = 0; i < FT2_FRAME_SAMPLES; ++i)
    {
        int src = start + i;
        out_signal[i] = (src >= 0 && src < input_len) ? input[src] : 0.0f;
    }
}

static void ft2_normalize_metric(float* metric, int count)
{
    float sum = 0.0f;
    float sum2 = 0.0f;
    for (int i = 0; i < count; ++i)
    {
        sum += metric[i];
        sum2 += metric[i] * metric[i];
    }
    float mean = sum / count;
    float variance = (sum2 / count) - (mean * mean);
    float sigma = (variance > 0.0f) ? sqrtf(variance) : sqrtf(fmaxf(sum2 / count, 0.0f));
    if (sigma <= 1.0e-6f)
        return;
    for (int i = 0; i < count; ++i)
        metric[i] /= sigma;
}

static bool ft2_extract_bitmetrics_raw(const float complex* signal, float bitmetrics[2 * FT2_FRAME_SYMBOLS][3])
{
    float complex symbols[4][FT2_FRAME_SYMBOLS];
    float s4[4][FT2_FRAME_SYMBOLS];
    memset(bitmetrics, 0, sizeof(float) * 2 * FT2_FRAME_SYMBOLS * 3);

    size_t fft_mem_len = 0;
    kiss_fft_alloc(FT2_NSS, 0, NULL, &fft_mem_len);
    void* fft_mem = malloc(fft_mem_len);
    kiss_fft_cfg fft_cfg = kiss_fft_alloc(FT2_NSS, 0, fft_mem, &fft_mem_len);
    if (!fft_cfg)
    {
        free(fft_mem);
        return false;
    }

    for (int sym = 0; sym < FT2_FRAME_SYMBOLS; ++sym)
    {
        kiss_fft_cpx csymb[FT2_NSS];
        for (int i = 0; i < FT2_NSS; ++i)
        {
            float complex sample = signal[sym * FT2_NSS + i];
            csymb[i].r = crealf(sample);
            csymb[i].i = cimagf(sample);
        }
        kiss_fft(fft_cfg, csymb, csymb);
        for (int tone = 0; tone < 4; ++tone)
        {
            float complex bin = csymb[tone].r + I * csymb[tone].i;
            symbols[tone][sym] = bin;
            s4[tone][sym] = cabsf(bin);
        }
    }

    free(fft_mem);

    static bool one_ready = false;
    static uint8_t one_mask[256][8];
    if (!one_ready)
    {
        for (int i = 0; i < 256; ++i)
        {
            for (int j = 0; j < 8; ++j)
                one_mask[i][j] = ((i & (1 << j)) != 0) ? 1u : 0u;
        }
        one_ready = true;
    }

    int sync_ok = 0;
    for (int group = 0; group < 4; ++group)
    {
        int base = group * 33;
        for (int i = 0; i < 4; ++i)
        {
            int best = 0;
            for (int tone = 1; tone < 4; ++tone)
            {
                if (s4[tone][base + i] > s4[best][base + i])
                    best = tone;
            }
            if (best == kFT4_Costas_pattern[group][i])
                ++sync_ok;
        }
    }
    if (sync_ok < 4)
        return false;

    float metric1[2 * FT2_FRAME_SYMBOLS] = { 0 };
    float metric2[2 * FT2_FRAME_SYMBOLS] = { 0 };
    float metric4[2 * FT2_FRAME_SYMBOLS] = { 0 };
    float s2[256];

    for (int nseq = 0; nseq < 3; ++nseq)
    {
        int nsym = (nseq == 0) ? 1 : (nseq == 1 ? 2 : 4);
        int nt = 1 << (2 * nsym);
        for (int ks = 0; ks <= FT2_FRAME_SYMBOLS - nsym; ks += nsym)
        {
            for (int i = 0; i < nt; ++i)
            {
                int i1 = i / 64;
                int i2 = (i & 63) / 16;
                int i3 = (i & 15) / 4;
                int i4 = i & 3;
                if (nsym == 1)
                {
                    s2[i] = cabsf(symbols[kFT4_Gray_map[i4]][ks]);
                }
                else if (nsym == 2)
                {
                    s2[i] = cabsf(symbols[kFT4_Gray_map[i3]][ks] + symbols[kFT4_Gray_map[i4]][ks + 1]);
                }
                else
                {
                    s2[i] = cabsf(
                        symbols[kFT4_Gray_map[i1]][ks] +
                        symbols[kFT4_Gray_map[i2]][ks + 1] +
                        symbols[kFT4_Gray_map[i3]][ks + 2] +
                        symbols[kFT4_Gray_map[i4]][ks + 3]);
                }
            }

            int ipt = 2 * ks;
            int ibmax = (nsym == 1) ? 1 : (nsym == 2 ? 3 : 7);
            for (int ib = 0; ib <= ibmax; ++ib)
            {
                float max_one = -INFINITY;
                float max_zero = -INFINITY;
                for (int i = 0; i < nt; ++i)
                {
                    if (one_mask[i][ibmax - ib])
                    {
                        if (s2[i] > max_one)
                            max_one = s2[i];
                    }
                    else if (s2[i] > max_zero)
                    {
                        max_zero = s2[i];
                    }
                }
                if ((ipt + ib) >= 2 * FT2_FRAME_SYMBOLS)
                    continue;
                if (nseq == 0)
                    metric1[ipt + ib] = max_one - max_zero;
                else if (nseq == 1)
                    metric2[ipt + ib] = max_one - max_zero;
                else
                    metric4[ipt + ib] = max_one - max_zero;
            }
        }
    }

    metric2[204] = metric1[204];
    metric2[205] = metric1[205];
    metric4[200] = metric2[200];
    metric4[201] = metric2[201];
    metric4[202] = metric2[202];
    metric4[203] = metric2[203];
    metric4[204] = metric1[204];
    metric4[205] = metric1[205];

    ft2_normalize_metric(metric1, 2 * FT2_FRAME_SYMBOLS);
    ft2_normalize_metric(metric2, 2 * FT2_FRAME_SYMBOLS);
    ft2_normalize_metric(metric4, 2 * FT2_FRAME_SYMBOLS);

    for (int i = 0; i < 2 * FT2_FRAME_SYMBOLS; ++i)
    {
        bitmetrics[i][0] = metric1[i];
        bitmetrics[i][1] = metric2[i];
        bitmetrics[i][2] = metric4[i];
    }
    return true;
}

static void ft2_pack_bits(const uint8_t bit_array[], int num_bits, uint8_t packed[])
{
    int num_bytes = (num_bits + 7) / 8;
    for (int i = 0; i < num_bytes; ++i)
        packed[i] = 0;

    uint8_t mask = 0x80;
    int byte_idx = 0;
    for (int i = 0; i < num_bits; ++i)
    {
        if (bit_array[i])
            packed[byte_idx] |= mask;
        mask >>= 1;
        if (!mask)
        {
            mask = 0x80;
            ++byte_idx;
        }
    }
}

static uint8_t ft2_parity8(uint8_t x)
{
    x ^= x >> 4;
    x ^= x >> 2;
    x ^= x >> 1;
    return x & 1u;
}

static void ft2_encode_codeword_from_a91(const uint8_t a91[FTX_LDPC_K_BYTES], uint8_t codeword[FTX_LDPC_N])
{
    for (int i = 0; i < FTX_LDPC_K; ++i)
    {
        codeword[i] = (a91[i / 8] >> (7 - (i % 8))) & 0x1u;
    }
    for (int i = 0; i < FTX_LDPC_M; ++i)
    {
        uint8_t nsum = 0;
        for (int j = 0; j < FTX_LDPC_K_BYTES; ++j)
        {
            nsum ^= ft2_parity8(a91[j] & kFTX_LDPC_generator[i][j]);
        }
        codeword[FTX_LDPC_K + i] = nsum & 0x1u;
    }
}

static float ft2_codeword_distance(const uint8_t codeword[FTX_LDPC_N], const float log174[FTX_LDPC_N])
{
    float distance = 0.0f;
    for (int i = 0; i < FTX_LDPC_N; ++i)
    {
        uint8_t hard = (log174[i] >= 0.0f) ? 1u : 0u;
        if (codeword[i] != hard)
            distance += fabsf(log174[i]);
    }
    return distance;
}

static bool ft2_try_crc_candidate(const uint8_t a91[FTX_LDPC_K_BYTES], ftx_message_t* message)
{
    uint8_t codeword[FTX_LDPC_N];
    ft2_encode_codeword_from_a91(a91, codeword);
    return ft2_unpack_message(codeword, message);
}

static bool ft2_osd_lite_decode(const float log174[FTX_LDPC_N], ftx_message_t* message)
{
    uint8_t base_a91[FTX_LDPC_K_BYTES];
    memset(base_a91, 0, sizeof(base_a91));
    for (int i = 0; i < FTX_LDPC_K; ++i)
    {
        if (log174[i] >= 0.0f)
            base_a91[i / 8] |= (uint8_t)(0x80u >> (i % 8));
    }

    if (ft2_try_crc_candidate(base_a91, message))
        return true;

    ft2_reliability_t reliabilities[FTX_LDPC_K];
    for (int i = 0; i < FTX_LDPC_K; ++i)
    {
        reliabilities[i].index = i;
        reliabilities[i].reliability = fabsf(log174[i]);
    }
    qsort(reliabilities, FTX_LDPC_K, sizeof(reliabilities[0]), ft2_cmp_reliability_asc);

    const int max_candidates = 12;
    const int n = (FTX_LDPC_K < max_candidates) ? FTX_LDPC_K : max_candidates;
    uint8_t trial_a91[FTX_LDPC_K_BYTES];
    uint8_t best_codeword[FTX_LDPC_N];
    float best_distance = INFINITY;
    bool have_best = false;

    for (int i = 0; i < n; ++i)
    {
        memcpy(trial_a91, base_a91, sizeof(trial_a91));
        int b0 = reliabilities[i].index;
        trial_a91[b0 / 8] ^= (uint8_t)(0x80u >> (b0 % 8));
        if (ft2_try_crc_candidate(trial_a91, message))
            return true;
    }

    for (int i = 0; i < n; ++i)
    {
        for (int j = i + 1; j < n; ++j)
        {
            memcpy(trial_a91, base_a91, sizeof(trial_a91));
            int b0 = reliabilities[i].index;
            int b1 = reliabilities[j].index;
            trial_a91[b0 / 8] ^= (uint8_t)(0x80u >> (b0 % 8));
            trial_a91[b1 / 8] ^= (uint8_t)(0x80u >> (b1 % 8));
            if (ft2_try_crc_candidate(trial_a91, message))
                return true;
        }
    }

    for (int i = 0; i < n; ++i)
    {
        for (int j = i + 1; j < n; ++j)
        {
            for (int k = j + 1; k < n; ++k)
            {
                memcpy(trial_a91, base_a91, sizeof(trial_a91));
                int b0 = reliabilities[i].index;
                int b1 = reliabilities[j].index;
                int b2 = reliabilities[k].index;
                trial_a91[b0 / 8] ^= (uint8_t)(0x80u >> (b0 % 8));
                trial_a91[b1 / 8] ^= (uint8_t)(0x80u >> (b1 % 8));
                trial_a91[b2 / 8] ^= (uint8_t)(0x80u >> (b2 % 8));
                if (ft2_try_crc_candidate(trial_a91, message))
                    return true;

                uint8_t codeword[FTX_LDPC_N];
                ft2_encode_codeword_from_a91(trial_a91, codeword);
                float distance = ft2_codeword_distance(codeword, log174);
                if (distance < best_distance)
                {
                    memcpy(best_codeword, codeword, sizeof(best_codeword));
                    best_distance = distance;
                    have_best = true;
                }
            }
        }
    }

    if (have_best)
        return ft2_unpack_message(best_codeword, message);
    return false;
}

static bool ft2_unpack_message(const uint8_t plain174[], ftx_message_t* message)
{
    uint8_t a91[FTX_LDPC_K_BYTES];
    ft2_pack_bits(plain174, FTX_LDPC_K, a91);

    uint16_t crc_extracted = ftx_extract_crc(a91);
    a91[9] &= 0xF8;
    a91[10] &= 0x00;
    uint16_t crc_calculated = ftx_compute_crc(a91, 96 - 14);
    if (crc_extracted != crc_calculated)
        return false;

    message->hash = crc_calculated;
    for (int i = 0; i < 10; ++i)
    {
        message->payload[i] = a91[i] ^ kFT4_XOR_sequence[i];
    }
    return true;
}

static bool ft2_decode_hit(
    const ft2_downsample_ctx_t* downsample_ctx,
    const ft2_scan_hit_t* hit,
    ftx_message_t* message,
    float* dt_s,
    float* freq_hz,
    float* snr_db,
    ft2_fail_stage_t* fail_stage,
    ft2_pass_diag_t* pass_diag)
{
    if (!downsample_ctx)
        return false;

    if (fail_stage)
        *fail_stage = FT2_FAIL_NONE;
    if (pass_diag)
    {
        for (int i = 0; i < 5; ++i)
        {
            pass_diag->ntype[i] = 0;
            pass_diag->nharderror[i] = -1;
            pass_diag->dmin[i] = INFINITY;
        }
    }
    const int nfft2 = downsample_ctx ? downsample_ctx->nfft2 : 0;
    float complex* cd2 = (float complex*)malloc(sizeof(float complex) * nfft2);
    float complex* cb = (float complex*)malloc(sizeof(float complex) * nfft2);
    float complex signal[FT2_FRAME_SAMPLES];
    float bitmetrics[2 * FT2_FRAME_SYMBOLS][3];
    float complex sync_wave[4][64];
    float complex tweak_wave[33][64];
    if (!cd2 || !cb)
    {
        free(cd2);
        free(cb);
        return false;
    }

    ft2_prepare_sync_waveforms(sync_wave, tweak_wave);

    int produced = ft2_downsample_candidate(downsample_ctx, hit->freq_hz, cd2, nfft2);
    if (produced <= 0)
    {
        if (fail_stage)
            *fail_stage = FT2_FAIL_FINAL_DOWNSAMPLE;
        free(cd2);
        free(cb);
        return false;
    }
    ft2_normalize_downsampled(cd2, produced, produced);

    float best_score = -1.0f;
    int best_start = hit->start;
    int best_idf = hit->idf;
    for (int idf = hit->idf - 4; idf <= hit->idf + 4; ++idf)
    {
        if (idf < FT2_SYNC_TWEAK_MIN || idf > FT2_SYNC_TWEAK_MAX)
            continue;
        for (int start = hit->start - 5; start <= hit->start + 5; ++start)
        {
            float score = ft2_sync2d_score(cd2, produced, start, idf, sync_wave, tweak_wave);
            if (score > best_score)
            {
                best_score = score;
                best_start = start;
                best_idf = idf;
            }
        }
    }
    if (best_score < 0.80f)
    {
        if (fail_stage)
            *fail_stage = FT2_FAIL_REFINED_SYNC;
        free(cd2);
        free(cb);
        return false;
    }

    float corrected_freq_hz = hit->freq_hz + best_idf;
    if (corrected_freq_hz <= 10.0f || corrected_freq_hz >= 4990.0f)
    {
        if (fail_stage)
            *fail_stage = FT2_FAIL_FREQ_RANGE;
        free(cd2);
        free(cb);
        return false;
    }

    produced = ft2_downsample_candidate(downsample_ctx, corrected_freq_hz, cb, nfft2);
    if (produced <= 0)
    {
        if (fail_stage)
            *fail_stage = FT2_FAIL_FINAL_DOWNSAMPLE;
        free(cd2);
        free(cb);
        return false;
    }
    ft2_normalize_downsampled(cb, produced, FT2_FRAME_SAMPLES);
    ft2_extract_signal_region(cb, produced, best_start, signal);

    if (!ft2_extract_bitmetrics_raw(signal, bitmetrics))
    {
        if (fail_stage)
            *fail_stage = FT2_FAIL_BITMETRICS;
        free(cd2);
        free(cb);
        return false;
    }

    static const uint8_t sync_bits_a[8] = { 0, 0, 0, 1, 1, 0, 1, 1 };
    static const uint8_t sync_bits_b[8] = { 0, 1, 0, 0, 1, 1, 1, 0 };
    static const uint8_t sync_bits_c[8] = { 1, 1, 1, 0, 0, 1, 0, 0 };
    static const uint8_t sync_bits_d[8] = { 1, 0, 1, 1, 0, 0, 0, 1 };
    int sync_qual = 0;
    for (int i = 0; i < 8; ++i)
    {
        sync_qual += ((bitmetrics[i][0] >= 0.0f) ? 1 : 0) == sync_bits_a[i];
        sync_qual += ((bitmetrics[66 + i][0] >= 0.0f) ? 1 : 0) == sync_bits_b[i];
        sync_qual += ((bitmetrics[132 + i][0] >= 0.0f) ? 1 : 0) == sync_bits_c[i];
        sync_qual += ((bitmetrics[198 + i][0] >= 0.0f) ? 1 : 0) == sync_bits_d[i];
    }
    if (sync_qual < 13)
    {
        if (fail_stage)
            *fail_stage = FT2_FAIL_SYNC_QUAL;
        free(cd2);
        free(cb);
        return false;
    }

    float llr_passes[5][FTX_LDPC_N];
    for (int i = 0; i < 58; ++i)
    {
        llr_passes[0][i] = bitmetrics[8 + i][0];
        llr_passes[0][58 + i] = bitmetrics[74 + i][0];
        llr_passes[0][116 + i] = bitmetrics[140 + i][0];
        llr_passes[1][i] = bitmetrics[8 + i][1];
        llr_passes[1][58 + i] = bitmetrics[74 + i][1];
        llr_passes[1][116 + i] = bitmetrics[140 + i][1];
        llr_passes[2][i] = bitmetrics[8 + i][2];
        llr_passes[2][58 + i] = bitmetrics[74 + i][2];
        llr_passes[2][116 + i] = bitmetrics[140 + i][2];
    }
    for (int i = 0; i < FTX_LDPC_N; ++i)
    {
        llr_passes[0][i] *= 2.83f;
        llr_passes[1][i] *= 2.83f;
        llr_passes[2][i] *= 2.83f;
        float a = llr_passes[0][i];
        float b = llr_passes[1][i];
        float c = llr_passes[2][i];
        llr_passes[3][i] = (fabsf(a) >= fabsf(b) && fabsf(a) >= fabsf(c)) ? a : ((fabsf(b) >= fabsf(c)) ? b : c);
        llr_passes[4][i] = (a + b + c) / 3.0f;
    }

    bool ok = false;
    uint8_t apmask[FTX_LDPC_N] = { 0 };
    uint8_t message91[FTX_LDPC_K] = { 0 };
    uint8_t cw[FTX_LDPC_N] = { 0 };
    for (int pass = 0; pass < 5 && !ok; ++pass)
    {
        float log174[FTX_LDPC_N];
        memcpy(log174, llr_passes[pass], sizeof(log174));
        int ntype = 0;
        int nharderror = -1;
        float dmin = 0.0f;
        decode174_91_osd(log174, FTX_LDPC_K, 3, 3, apmask, message91, cw, &ntype, &nharderror, &dmin);
        if (pass_diag)
        {
            pass_diag->ntype[pass] = ntype;
            pass_diag->nharderror[pass] = nharderror;
            pass_diag->dmin[pass] = dmin;
        }
        if (ntype != 0 && nharderror >= 0)
            ok = ft2_unpack_message(cw, message);
    }
    if (!ok && fail_stage)
        *fail_stage = FT2_FAIL_LDPC;

    if (ok)
    {
        float sm1 = ft2_sync2d_score(cd2, produced, best_start - 1, best_idf, sync_wave, tweak_wave);
        float sp1 = ft2_sync2d_score(cd2, produced, best_start + 1, best_idf, sync_wave, tweak_wave);
        float xstart = (float)best_start;
        float den = sm1 - 2.0f * best_score + sp1;
        if (fabsf(den) > 1.0e-6f)
            xstart += 0.5f * (sm1 - sp1) / den;

        *dt_s = xstart / (12000.0f / FT2_NDOWN) - 0.5f;
        *freq_hz = corrected_freq_hz;
        if (hit->snr0 > 0.0f)
            *snr_db = fmaxf(-21.0f, 10.0f * log10f(hit->snr0) - 13.0f);
        else
            *snr_db = -21.0f;
    }
    free(cd2);
    free(cb);
    return ok;
}

ft8_decoder_t* ft8_decoder_create(int sample_rate, float f_min, float f_max, int time_osr, int freq_osr, int protocol)
{
    ft8_decoder_t* dec = (ft8_decoder_t*)calloc(1, sizeof(ft8_decoder_t));
    if (!dec)
    {
        return NULL;
    }
    dec->cfg.f_min = f_min;
    dec->cfg.f_max = f_max;
    dec->cfg.sample_rate = sample_rate;
    dec->cfg.time_osr = time_osr;
    dec->cfg.freq_osr = freq_osr;
    switch (protocol)
    {
    case TRX_FTX_PROTOCOL_FT4:
        dec->cfg.protocol = FTX_PROTOCOL_FT4;
        break;
    case TRX_FTX_PROTOCOL_FT2:
        dec->cfg.protocol = FTX_PROTOCOL_FT2;
        break;
    case TRX_FTX_PROTOCOL_FT8:
    default:
        dec->cfg.protocol = FTX_PROTOCOL_FT8;
        break;
    }

    hashtable_init();
    monitor_init(&dec->mon, &dec->cfg);
    if (dec->cfg.protocol == FTX_PROTOCOL_FT2)
    {
        dec->ft2_raw_capacity = FT2_NMAX;
        dec->ft2_raw = (float*)calloc(dec->ft2_raw_capacity, sizeof(float));
        dec->ft2_raw_len = 0;
        if (!dec->ft2_raw)
        {
            monitor_free(&dec->mon);
            free(dec);
            return NULL;
        }
    }
    return dec;
}

void ft8_decoder_free(ft8_decoder_t* dec)
{
    if (!dec)
        return;
    free(dec->ft2_raw);
    monitor_free(&dec->mon);
    free(dec);
}

int ft8_decoder_block_size(const ft8_decoder_t* dec)
{
    return dec ? dec->mon.block_size : 0;
}

int ft8_decoder_window_samples(const ft8_decoder_t* dec)
{
    if (!dec)
        return 0;
    if (dec->cfg.protocol == FTX_PROTOCOL_FT2)
        return FT2_NMAX;
    return dec->mon.block_size * dec->mon.wf.max_blocks;
}

void ft8_decoder_reset(ft8_decoder_t* dec)
{
    if (!dec)
        return;
    monitor_reset(&dec->mon);
    dec->ft2_raw_len = 0;
    if (dec->ft2_raw && dec->ft2_raw_capacity > 0)
    {
        memset(dec->ft2_raw, 0, sizeof(float) * dec->ft2_raw_capacity);
    }
}

void ft8_decoder_process(ft8_decoder_t* dec, const float* frame)
{
    if (!dec || !frame)
        return;
    if (dec->cfg.protocol == FTX_PROTOCOL_FT2 && dec->ft2_raw && dec->ft2_raw_capacity > 0)
    {
        int remaining = dec->ft2_raw_capacity - dec->ft2_raw_len;
        if (remaining > 0)
        {
            int copy_len = (remaining < dec->mon.block_size) ? remaining : dec->mon.block_size;
            memcpy(dec->ft2_raw + dec->ft2_raw_len, frame, sizeof(float) * copy_len);
            dec->ft2_raw_len += copy_len;
        }
    }
    monitor_process(&dec->mon, frame);
}

int ft8_decoder_is_ready(const ft8_decoder_t* dec)
{
    if (!dec)
        return 0;
    if (dec->cfg.protocol == FTX_PROTOCOL_FT2)
    {
        return (dec->ft2_raw_len >= dec->ft2_raw_capacity) ? 1 : 0;
    }
    return (dec->mon.wf.num_blocks >= dec->mon.wf.max_blocks) ? 1 : 0;
}

int ft8_decoder_decode(ft8_decoder_t* dec, ft8_decode_result_t* out, int max_results)
{
    if (!dec || !out || max_results <= 0)
        return 0;

    const ftx_waterfall_t* wf = &dec->mon.wf;
    const bool is_ft2 = (dec->cfg.protocol == FTX_PROTOCOL_FT2);

    int num_decoded = 0;
    ftx_message_t decoded[200];
    ftx_message_t* decoded_hashtable[200];
    for (int i = 0; i < 200; ++i)
    {
        decoded_hashtable[i] = NULL;
    }

    if (is_ft2)
    {
        ft2_downsample_ctx_t downsample_ctx;
        if (!ft2_downsample_ctx_init(dec, &downsample_ctx))
        {
            LOG(LOG_WARN, "FT2 decode: downsample context init failed\n");
            return 0;
        }
        ft2_scan_hit_t hit_list[FT2_MAX_SCAN_HITS];
        ft2_scan_stats_t scan_stats;
        int num_hits = ft2_find_scan_hits(dec, &downsample_ctx, hit_list, FT2_MAX_SCAN_HITS, &scan_stats);
        int fail_refined_sync = 0;
        int fail_freq_range = 0;
        int fail_final_downsample = 0;
        int fail_bitmetrics = 0;
        int fail_sync_qual = 0;
        int fail_ldpc = 0;
        int fail_crc = 0;
        int fail_unpack = 0;
        int pass_bp[5] = { 0 };
        int pass_osd[5] = { 0 };
        float pass_best_dmin[5] = { INFINITY, INFINITY, INFINITY, INFINITY, INFINITY };
        for (int idx = 0; idx < num_hits && num_decoded < max_results; ++idx)
        {
            const ft2_scan_hit_t* hit = &hit_list[idx];
            ftx_message_t message;
            float time_sec = 0.0f;
            float freq_hz = 0.0f;
            float snr_db = -21.0f;
            ft2_fail_stage_t fail_stage = FT2_FAIL_NONE;
            ft2_pass_diag_t pass_diag;
            if (!ft2_decode_hit(&downsample_ctx, hit, &message, &time_sec, &freq_hz, &snr_db, &fail_stage, &pass_diag))
            {
                for (int pass = 0; pass < 5; ++pass)
                {
                    if (pass_diag.ntype[pass] == 1)
                        ++pass_bp[pass];
                    else if (pass_diag.ntype[pass] == 2)
                        ++pass_osd[pass];
                    if (pass_diag.dmin[pass] < pass_best_dmin[pass])
                        pass_best_dmin[pass] = pass_diag.dmin[pass];
                }
                switch (fail_stage)
                {
                case FT2_FAIL_REFINED_SYNC:
                    ++fail_refined_sync;
                    break;
                case FT2_FAIL_FREQ_RANGE:
                    ++fail_freq_range;
                    break;
                case FT2_FAIL_FINAL_DOWNSAMPLE:
                    ++fail_final_downsample;
                    break;
                case FT2_FAIL_BITMETRICS:
                    ++fail_bitmetrics;
                    break;
                case FT2_FAIL_SYNC_QUAL:
                    ++fail_sync_qual;
                    break;
                case FT2_FAIL_LDPC:
                    ++fail_ldpc;
                    break;
                case FT2_FAIL_CRC:
                    ++fail_crc;
                    break;
                case FT2_FAIL_UNPACK:
                    ++fail_unpack;
                    break;
                default:
                    break;
                }
                continue;
            }
            for (int pass = 0; pass < 5; ++pass)
            {
                if (pass_diag.ntype[pass] == 1)
                    ++pass_bp[pass];
                else if (pass_diag.ntype[pass] == 2)
                    ++pass_osd[pass];
                if (pass_diag.dmin[pass] < pass_best_dmin[pass])
                    pass_best_dmin[pass] = pass_diag.dmin[pass];
            }

            int idx_hash = message.hash % 200;
            bool found_empty_slot = false;
            bool found_duplicate = false;
            do
            {
                if (decoded_hashtable[idx_hash] == NULL)
                {
                    found_empty_slot = true;
                }
                else if ((decoded_hashtable[idx_hash]->hash == message.hash) && (0 == memcmp(decoded_hashtable[idx_hash]->payload, message.payload, sizeof(message.payload))))
                {
                    found_duplicate = true;
                }
                else
                {
                    idx_hash = (idx_hash + 1) % 200;
                }
            } while (!found_empty_slot && !found_duplicate);

            if (!found_empty_slot)
                continue;

            memcpy(&decoded[idx_hash], &message, sizeof(message));
            decoded_hashtable[idx_hash] = &decoded[idx_hash];

            char text[FTX_MAX_MESSAGE_LENGTH];
            ftx_message_offsets_t offsets;
            ftx_message_rc_t unpack_status = ftx_message_decode(&message, &hash_if, text, &offsets);
            if (unpack_status != FTX_MESSAGE_RC_OK)
            {
                ++fail_unpack;
                continue;
            }

            ft8_decode_result_t* dst = &out[num_decoded];
            strncpy(dst->text, text, sizeof(dst->text) - 1);
            dst->text[sizeof(dst->text) - 1] = '\0';
            dst->dt_s = time_sec;
            dst->freq_hz = freq_hz;
            dst->snr_db = snr_db;

            num_decoded++;
        }
        LOG(LOG_INFO,
            "FT2 window: raw=%d peaks=%d hits=%d best_peak=%.3f best_sync=%.3f decoded=%d fail(sync=%d freq=%d down=%d bits=%d qual=%d ldpc=%d crc=%d unpack=%d) pass(bp=%d/%d/%d/%d/%d osd=%d/%d/%d/%d/%d dmin=%.2f/%.2f/%.2f/%.2f/%.2f)\n",
            dec->ft2_raw_len,
            scan_stats.peaks_found,
            scan_stats.hits_found,
            scan_stats.best_peak_score,
            scan_stats.best_sync_score,
            num_decoded,
            fail_refined_sync,
            fail_freq_range,
            fail_final_downsample,
            fail_bitmetrics,
            fail_sync_qual,
            fail_ldpc,
            fail_crc,
            fail_unpack,
            pass_bp[0], pass_bp[1], pass_bp[2], pass_bp[3], pass_bp[4],
            pass_osd[0], pass_osd[1], pass_osd[2], pass_osd[3], pass_osd[4],
            isfinite(pass_best_dmin[0]) ? pass_best_dmin[0] : -1.0f,
            isfinite(pass_best_dmin[1]) ? pass_best_dmin[1] : -1.0f,
            isfinite(pass_best_dmin[2]) ? pass_best_dmin[2] : -1.0f,
            isfinite(pass_best_dmin[3]) ? pass_best_dmin[3] : -1.0f,
            isfinite(pass_best_dmin[4]) ? pass_best_dmin[4] : -1.0f);
        ft2_downsample_ctx_free(&downsample_ctx);
    }
    else
    {
        const int kMaxCandidates = 200;
        const int kMinScore = 10;
        const int kLdpcIters = 30;
        ftx_candidate_t candidate_list[kMaxCandidates];
        int num_candidates = ftx_find_candidates(wf, kMaxCandidates, candidate_list, kMinScore);

        for (int idx = 0; idx < num_candidates && num_decoded < max_results; ++idx)
        {
            const ftx_candidate_t* cand = &candidate_list[idx];

            float freq_hz = decoder_candidate_freq_hz(dec, cand);
            float time_sec = decoder_candidate_dt_s(dec, cand);

            ftx_message_t message;
            ftx_decode_status_t status;
            if (!ftx_decode_candidate(wf, cand, kLdpcIters, &message, &status))
            {
                continue;
            }

            int idx_hash = message.hash % 200;
            bool found_empty_slot = false;
            bool found_duplicate = false;
            do
            {
                if (decoded_hashtable[idx_hash] == NULL)
                {
                    found_empty_slot = true;
                }
                else if ((decoded_hashtable[idx_hash]->hash == message.hash) && (0 == memcmp(decoded_hashtable[idx_hash]->payload, message.payload, sizeof(message.payload))))
                {
                    found_duplicate = true;
                }
                else
                {
                    idx_hash = (idx_hash + 1) % 200;
                }
            } while (!found_empty_slot && !found_duplicate);

            if (!found_empty_slot)
                continue;

            memcpy(&decoded[idx_hash], &message, sizeof(message));
            decoded_hashtable[idx_hash] = &decoded[idx_hash];

            char text[FTX_MAX_MESSAGE_LENGTH];
            ftx_message_offsets_t offsets;
            ftx_message_rc_t unpack_status = ftx_message_decode(&message, &hash_if, text, &offsets);
            if (unpack_status != FTX_MESSAGE_RC_OK)
                continue;

            ft8_decode_result_t* dst = &out[num_decoded];
            strncpy(dst->text, text, sizeof(dst->text) - 1);
            dst->text[sizeof(dst->text) - 1] = '\0';
            dst->dt_s = time_sec;
            dst->freq_hz = freq_hz;
            dst->snr_db = cand->score * 0.5f - 29.0f;

            num_decoded++;
        }
    }

    hashtable_cleanup(10);
    return num_decoded;
}
