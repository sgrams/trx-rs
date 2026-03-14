// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#ifndef TRX_FT2_LDPC_H
#define TRX_FT2_LDPC_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

void ft2_decode174_91_osd(float llr[], int keff, int maxosd, int norder, uint8_t apmask[], uint8_t message91[], uint8_t cw[], int* ntype, int* nharderror, float* dmin);

#ifdef __cplusplus
}
#endif

#endif
