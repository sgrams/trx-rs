// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#include <ft8/decode.h>
#include <ft8/message.h>
#include <ft8/text.h>
#include <common/monitor.h>

#include <stdlib.h>
#include <string.h>
#include <stdio.h>

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
    int hash_shift = (hash_type == FTX_CALLSIGN_HASH_22) ? 0 : (hash_type == FTX_CALLSIGN_HASH_12) ? 10 : 12;
    uint32_t mask = (hash_type == FTX_CALLSIGN_HASH_22) ? 0x3FFFFFu : (hash_type == FTX_CALLSIGN_HASH_12) ? 0xFFFu : 0x3FFu;

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
} ft8_decoder_t;

typedef struct
{
    char text[FTX_MAX_MESSAGE_LENGTH];
    float snr_db;
    float dt_s;
    float freq_hz;
} ft8_decode_result_t;

ft8_decoder_t* ft8_decoder_create(int sample_rate, float f_min, float f_max, int time_osr, int freq_osr)
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
    dec->cfg.protocol = FTX_PROTOCOL_FT8;

    hashtable_init();
    monitor_init(&dec->mon, &dec->cfg);
    return dec;
}

void ft8_decoder_free(ft8_decoder_t* dec)
{
    if (!dec)
        return;
    monitor_free(&dec->mon);
    free(dec);
}

int ft8_decoder_block_size(const ft8_decoder_t* dec)
{
    return dec ? dec->mon.block_size : 0;
}

void ft8_decoder_reset(ft8_decoder_t* dec)
{
    if (!dec)
        return;
    monitor_reset(&dec->mon);
}

void ft8_decoder_process(ft8_decoder_t* dec, const float* frame)
{
    if (!dec || !frame)
        return;
    monitor_process(&dec->mon, frame);
}

int ft8_decoder_is_ready(const ft8_decoder_t* dec)
{
    if (!dec)
        return 0;
    return (dec->mon.wf.num_blocks >= dec->mon.wf.max_blocks) ? 1 : 0;
}

int ft8_decoder_decode(ft8_decoder_t* dec, ft8_decode_result_t* out, int max_results)
{
    if (!dec || !out || max_results <= 0)
        return 0;

    const ftx_waterfall_t* wf = &dec->mon.wf;
    const int kMaxCandidates = 200;
    const int kMinScore = 10;
    const int kLdpcIters = 30;

    ftx_candidate_t candidate_list[kMaxCandidates];
    int num_candidates = ftx_find_candidates(wf, kMaxCandidates, candidate_list, kMinScore);

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

        float freq_hz = (dec->mon.min_bin + cand->freq_offset + (float)cand->freq_sub / wf->freq_osr) / dec->mon.symbol_period;
        float time_sec = (cand->time_offset + (float)cand->time_sub / wf->time_osr) * dec->mon.symbol_period;

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
        {
            snprintf(text, sizeof(text), "Error [%d] while unpacking!", (int)unpack_status);
        }

        ft8_decode_result_t* dst = &out[num_decoded];
        strncpy(dst->text, text, sizeof(dst->text) - 1);
        dst->text[sizeof(dst->text) - 1] = '\0';
        dst->dt_s = time_sec;
        dst->freq_hz = freq_hz;
        dst->snr_db = cand->score * 0.5f;

        num_decoded++;
    }

    hashtable_cleanup(10);
    return num_decoded;
}
