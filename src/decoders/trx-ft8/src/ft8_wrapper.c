// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#include <ft8/decode.h>
#include <ft8/message.h>
#include <ft8/text.h>
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
#define FT2_MAX_RAW_CANDIDATES 96
#define FT2_SYNC_TWEAK_MIN (-16)
#define FT2_SYNC_TWEAK_MAX (16)

typedef struct
{
    float freq_hz;
    float score;
} ft2_raw_candidate_t;

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
            float dphase = 2.0f * (float)M_PI * tone / nss;
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
            float phase = 2.0f * (float)M_PI * idf * n / fs_down;
            tweak_wave[idf - FT2_SYNC_TWEAK_MIN][n] = cexpf(I * phase);
        }
    }
}

static int ft2_downsample_candidate(
    const ft8_decoder_t* dec,
    float freq_hz,
    float complex* out_samples,
    int out_len)
{
    const int nraw = dec->ft2_raw_len;
    const int nfft2 = nraw / FT2_NDOWN;
    if (!dec->ft2_raw || nraw <= 0 || out_len < nfft2)
        return 0;

    kiss_fft_scalar* timedata = (kiss_fft_scalar*)malloc(sizeof(kiss_fft_scalar) * nraw);
    kiss_fft_cpx* spectrum = (kiss_fft_cpx*)malloc(sizeof(kiss_fft_cpx) * (nraw / 2 + 1));
    kiss_fft_cpx* band = (kiss_fft_cpx*)calloc(nfft2, sizeof(kiss_fft_cpx));
    float* filt = (float*)calloc(nfft2, sizeof(float));
    if (!timedata || !spectrum || !band || !filt)
    {
        free(timedata);
        free(spectrum);
        free(band);
        free(filt);
        return 0;
    }

    size_t rfft_mem_len = 0;
    kiss_fftr_alloc(nraw, 0, NULL, &rfft_mem_len);
    void* rfft_mem = malloc(rfft_mem_len);
    kiss_fftr_cfg rfft_cfg = kiss_fftr_alloc(nraw, 0, rfft_mem, &rfft_mem_len);

    size_t ifft_mem_len = 0;
    kiss_fft_alloc(nfft2, 1, NULL, &ifft_mem_len);
    void* ifft_mem = malloc(ifft_mem_len);
    kiss_fft_cfg ifft_cfg = kiss_fft_alloc(nfft2, 1, ifft_mem, &ifft_mem_len);

    if (!rfft_cfg || !ifft_cfg)
    {
        free(timedata);
        free(spectrum);
        free(band);
        free(filt);
        free(rfft_mem);
        free(ifft_mem);
        return 0;
    }

    for (int i = 0; i < nraw; ++i)
        timedata[i] = dec->ft2_raw[i];
    kiss_fftr(rfft_cfg, timedata, spectrum);

    const float df = (float)dec->cfg.sample_rate / nraw;
    const float baud = 1.0f / FT2_SYMBOL_PERIOD;
    const int i0 = (int)lroundf(freq_hz / df);
    const int iwt = (int)lroundf((0.5f * baud) / df);
    const int iwf = (int)lroundf((4.0f * baud) / df);
    const int iws = (int)lroundf(baud / df);
    for (int i = 0; i < nfft2; ++i)
        filt[i] = 0.0f;
    for (int i = 0; i < iwt && i < nfft2; ++i)
        filt[i] = 0.5f * (1.0f + cosf((float)M_PI * (float)(iwt - 1 - i) / fmaxf((float)iwt, 1.0f)));
    for (int i = iwt; i < iwt + iwf && i < nfft2; ++i)
        filt[i] = 1.0f;
    for (int i = iwt + iwf; i < 2 * iwt + iwf && i < nfft2; ++i)
        filt[i] = 0.5f * (1.0f + cosf((float)M_PI * (float)(i - (iwt + iwf)) / fmaxf((float)iwt, 1.0f)));

    if (iws > 0)
    {
        float* shifted = (float*)calloc(nfft2, sizeof(float));
        for (int i = 0; i < nfft2; ++i)
        {
            shifted[(i + iws) % nfft2] = filt[i];
        }
        memcpy(filt, shifted, sizeof(float) * nfft2);
        free(shifted);
    }

    if (i0 >= 0 && i0 <= nraw / 2)
        band[0] = spectrum[i0];
    for (int i = 1; i < nfft2 / 2; ++i)
    {
        if ((i0 + i) >= 0 && (i0 + i) <= nraw / 2)
            band[i] = spectrum[i0 + i];
        if ((i0 - i) >= 0 && (i0 - i) <= nraw / 2)
            band[nfft2 - i] = spectrum[i0 - i];
    }

    for (int i = 0; i < nfft2; ++i)
    {
        band[i].r = band[i].r * filt[i] / nfft2;
        band[i].i = band[i].i * filt[i] / nfft2;
    }
    kiss_fft(ifft_cfg, band, band);
    for (int i = 0; i < nfft2; ++i)
        out_samples[i] = band[i].r + I * band[i].i;

    free(timedata);
    free(spectrum);
    free(band);
    free(filt);
    free(rfft_mem);
    free(ifft_mem);
    return nfft2;
}

static float ft2_sync2d_score(
    const float complex* samples,
    int n_samples,
    int start,
    int idf,
    const float complex sync_wave[4][64],
    const float complex tweak_wave[33][64])
{
    const int nss = FT2_NSTEP / FT2_NDOWN;
    const int positions[4] = {
        start,
        start + 33 * nss,
        start + 66 * nss,
        start + 99 * nss,
    };
    float score = 0.0f;
    int groups = 0;
    const float complex* tweak = tweak_wave[idf - FT2_SYNC_TWEAK_MIN];

    for (int group = 0; group < 4; ++group)
    {
        int pos = positions[group];
        if (pos < 0 || (pos + 4 * nss) > n_samples)
            continue;
        float complex sum = 0.0f;
        for (int i = 0; i < 64; ++i)
        {
            int sample_idx = pos + 2 * i;
            sum += samples[sample_idx] * conjf(sync_wave[group][i] * tweak[i]);
        }
        score += cabsf(sum) / 64.0f;
        ++groups;
    }
    return (groups > 0) ? (score / groups) : 0.0f;
}

static int ft2_find_candidates_raw(
    const ft8_decoder_t* dec,
    ftx_candidate_t* out,
    int max_candidates)
{
    ft2_raw_candidate_t peaks[FT2_MAX_RAW_CANDIDATES];
    int n_peaks = ft2_find_frequency_peaks(dec, peaks, FT2_MAX_RAW_CANDIDATES);
    if (n_peaks <= 0)
        return 0;

    const int nraw = dec->ft2_raw_len;
    const int nfft2 = nraw / FT2_NDOWN;
    float complex* down = (float complex*)malloc(sizeof(float complex) * nfft2);
    float complex sync_wave[4][64];
    float complex tweak_wave[33][64];
    ft2_prepare_sync_waveforms(sync_wave, tweak_wave);

    int count = 0;
    for (int peak = 0; peak < n_peaks && count < max_candidates; ++peak)
    {
        int produced = ft2_downsample_candidate(dec, peaks[peak].freq_hz, down, nfft2);
        if (produced <= 0)
            continue;

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
        if (best_score < 0.18f)
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
        if (best_score < 0.24f)
            continue;

        float corrected_freq_hz = peaks[peak].freq_hz + best_idf;
        float base_freq_hz = corrected_freq_hz - ft2_frequency_offset_hz();
        float bin_pos = base_freq_hz * FT2_SYMBOL_PERIOD;
        int absolute_bin = (int)floorf(bin_pos);
        int freq_sub = (int)lroundf((bin_pos - absolute_bin) * dec->cfg.freq_osr);
        if (freq_sub >= dec->cfg.freq_osr)
        {
            absolute_bin += 1;
            freq_sub = 0;
        }
        int freq_offset = absolute_bin - dec->mon.min_bin;
        if (freq_offset < 0 || (freq_offset + 3) >= dec->mon.wf.num_bins)
            continue;

        int start_with_ramp = best_start - 32;
        int time_offset = start_with_ramp / 32;
        int rem = start_with_ramp - time_offset * 32;
        if (rem < 0)
        {
            rem += 32;
            time_offset -= 1;
        }
        int time_sub = (int)lroundf((float)rem / 4.0f);
        if (time_sub >= dec->cfg.time_osr)
        {
            time_offset += 1;
            time_sub = 0;
        }

        out[count].score = (int16_t)lroundf(best_score * 100.0f);
        out[count].time_offset = (int16_t)time_offset;
        out[count].time_sub = (uint8_t)time_sub;
        out[count].freq_offset = (int16_t)freq_offset;
        out[count].freq_sub = (uint8_t)freq_sub;
        ++count;
    }

    free(down);
    return count;
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
        dec->ft2_raw_capacity = dec->mon.block_size * dec->mon.wf.max_blocks;
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
        if (dec->ft2_raw_len + dec->mon.block_size <= dec->ft2_raw_capacity)
        {
            memcpy(dec->ft2_raw + dec->ft2_raw_len, frame, sizeof(float) * dec->mon.block_size);
            dec->ft2_raw_len += dec->mon.block_size;
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
    const int kMaxCandidates = is_ft2 ? 400 : 200;
    const int kMinScore = is_ft2 ? 0 : 10;
    const int kLdpcIters = is_ft2 ? 50 : 30;

    ftx_candidate_t candidate_list[kMaxCandidates];
    int num_candidates = is_ft2
        ? ft2_find_candidates_raw(dec, candidate_list, kMaxCandidates)
        : ftx_find_candidates(wf, kMaxCandidates, candidate_list, kMinScore);

    int num_decoded = 0;
    ftx_message_t decoded[200];
    ftx_message_t* decoded_hashtable[200];
    for (int i = 0; i < 200; ++i)
    {
        decoded_hashtable[i] = NULL;
    }

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
        /* Convert sync score to SNR in dB (WSJT-X 2500 Hz reference bandwidth).
         * score is an average uint8 difference between Costas tones and their
         * neighbours; each unit = 0.5 dB.  Subtract 10*log10(2500/3.125) ≈ 29 dB
         * to normalise from a 3.125 Hz bin (6.25 Hz / freq_osr=2) to 2500 Hz. */
        dst->snr_db = cand->score * 0.5f - 29.0f;

        num_decoded++;
    }

    hashtable_cleanup(10);
    return num_decoded;
}
