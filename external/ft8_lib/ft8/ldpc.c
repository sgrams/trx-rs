//
// LDPC decoder for FT8.
//
// given a 174-bit codeword as an array of log-likelihood of zero,
// return a 174-bit corrected codeword, or zero-length array.
// last 87 bits are the (systematic) plain-text.
// this is an implementation of the sum-product algorithm
// from Sarah Johnson's Iterative Error Correction book.
// codeword[i] = log ( P(x=0) / P(x=1) )
//

#include "ldpc.h"
#include "constants.h"
#include "crc.h"

#include <stdio.h>
#include <math.h>
#include <stdlib.h>
#include <stdbool.h>
#include <string.h>

static int ldpc_check(uint8_t codeword[]);
static float fast_tanh(float x);
static float fast_atanh(float x);
static float platanh(float x);
static void pack_bits91(const uint8_t bit_array[], int num_bits, uint8_t packed[]);
static void unpack_bits91(const uint8_t packed[], int num_bits, uint8_t bit_array[]);
static bool check_crc91(const uint8_t plain91[]);
static void encode174_91_nocrc_bits(const uint8_t message91[], uint8_t codeword[]);
static int cmp_reliability_desc(const void* lhs, const void* rhs);

// codeword is 174 log-likelihoods.
// plain is a return value, 174 ints, to be 0 or 1.
// max_iters is how hard to try.
// ok == 87 means success.
void ldpc_decode(float codeword[], int max_iters, uint8_t plain[], int* ok)
{
    float m[FTX_LDPC_M][FTX_LDPC_N]; // ~60 kB
    float e[FTX_LDPC_M][FTX_LDPC_N]; // ~60 kB
    int min_errors = FTX_LDPC_M;

    for (int j = 0; j < FTX_LDPC_M; j++)
    {
        for (int i = 0; i < FTX_LDPC_N; i++)
        {
            m[j][i] = codeword[i];
            e[j][i] = 0.0f;
        }
    }

    for (int iter = 0; iter < max_iters; iter++)
    {
        for (int j = 0; j < FTX_LDPC_M; j++)
        {
            for (int ii1 = 0; ii1 < kFTX_LDPC_Num_rows[j]; ii1++)
            {
                int i1 = kFTX_LDPC_Nm[j][ii1] - 1;
                float a = 1.0f;
                for (int ii2 = 0; ii2 < kFTX_LDPC_Num_rows[j]; ii2++)
                {
                    int i2 = kFTX_LDPC_Nm[j][ii2] - 1;
                    if (i2 != i1)
                    {
                        a *= fast_tanh(-m[j][i2] / 2.0f);
                    }
                }
                e[j][i1] = -2.0f * fast_atanh(a);
            }
        }

        for (int i = 0; i < FTX_LDPC_N; i++)
        {
            float l = codeword[i];
            for (int j = 0; j < 3; j++)
                l += e[kFTX_LDPC_Mn[i][j] - 1][i];
            plain[i] = (l > 0) ? 1 : 0;
        }

        int errors = ldpc_check(plain);

        if (errors < min_errors)
        {
            // Update the current best result
            min_errors = errors;

            if (errors == 0)
            {
                break; // Found a perfect answer
            }
        }

        for (int i = 0; i < FTX_LDPC_N; i++)
        {
            for (int ji1 = 0; ji1 < 3; ji1++)
            {
                int j1 = kFTX_LDPC_Mn[i][ji1] - 1;
                float l = codeword[i];
                for (int ji2 = 0; ji2 < 3; ji2++)
                {
                    if (ji1 != ji2)
                    {
                        int j2 = kFTX_LDPC_Mn[i][ji2] - 1;
                        l += e[j2][i];
                    }
                }
                m[j1][i] = l;
            }
        }
    }

    *ok = min_errors;
}

//
// does a 174-bit codeword pass the FT8's LDPC parity checks?
// returns the number of parity errors.
// 0 means total success.
//
static int ldpc_check(uint8_t codeword[])
{
    int errors = 0;

    for (int m = 0; m < FTX_LDPC_M; ++m)
    {
        uint8_t x = 0;
        for (int i = 0; i < kFTX_LDPC_Num_rows[m]; ++i)
        {
            x ^= codeword[kFTX_LDPC_Nm[m][i] - 1];
        }
        if (x != 0)
        {
            ++errors;
        }
    }
    return errors;
}

void bp_decode(float codeword[], int max_iters, uint8_t plain[], int* ok)
{
    float tov[FTX_LDPC_N][3];
    float toc[FTX_LDPC_M][7];

    int min_errors = FTX_LDPC_M;

    // initialize message data
    for (int n = 0; n < FTX_LDPC_N; ++n)
    {
        tov[n][0] = tov[n][1] = tov[n][2] = 0;
    }

    for (int iter = 0; iter < max_iters; ++iter)
    {
        // Do a hard decision guess (tov=0 in iter 0)
        int plain_sum = 0;
        for (int n = 0; n < FTX_LDPC_N; ++n)
        {
            plain[n] = ((codeword[n] + tov[n][0] + tov[n][1] + tov[n][2]) > 0) ? 1 : 0;
            plain_sum += plain[n];
        }

        if (plain_sum == 0)
        {
            // message converged to all-zeros, which is prohibited
            break;
        }

        // Check to see if we have a codeword (check before we do any iter)
        int errors = ldpc_check(plain);

        if (errors < min_errors)
        {
            // we have a better guess - update the result
            min_errors = errors;

            if (errors == 0)
            {
                break; // Found a perfect answer
            }
        }

        // Send messages from bits to check nodes
        for (int m = 0; m < FTX_LDPC_M; ++m)
        {
            for (int n_idx = 0; n_idx < kFTX_LDPC_Num_rows[m]; ++n_idx)
            {
                int n = kFTX_LDPC_Nm[m][n_idx] - 1;
                // for each (n, m)
                float Tnm = codeword[n];
                for (int m_idx = 0; m_idx < 3; ++m_idx)
                {
                    if ((kFTX_LDPC_Mn[n][m_idx] - 1) != m)
                    {
                        Tnm += tov[n][m_idx];
                    }
                }
                toc[m][n_idx] = fast_tanh(-Tnm / 2);
            }
        }

        // send messages from check nodes to variable nodes
        for (int n = 0; n < FTX_LDPC_N; ++n)
        {
            for (int m_idx = 0; m_idx < 3; ++m_idx)
            {
                int m = kFTX_LDPC_Mn[n][m_idx] - 1;
                // for each (n, m)
                float Tmn = 1.0f;
                for (int n_idx = 0; n_idx < kFTX_LDPC_Num_rows[m]; ++n_idx)
                {
                    if ((kFTX_LDPC_Nm[m][n_idx] - 1) != n)
                    {
                        Tmn *= toc[m][n_idx];
                    }
                }
                tov[n][m_idx] = -2 * fast_atanh(Tmn);
            }
        }
    }

    *ok = min_errors;
}

// Ideas for approximating tanh/atanh:
// * https://varietyofsound.wordpress.com/2011/02/14/efficient-tanh-computation-using-lamberts-continued-fraction/
// * http://functions.wolfram.com/ElementaryFunctions/ArcTanh/10/0001/
// * https://mathr.co.uk/blog/2017-09-06_approximating_hyperbolic_tangent.html
// * https://math.stackexchange.com/a/446411

static float fast_tanh(float x)
{
    if (x < -4.97f)
    {
        return -1.0f;
    }
    if (x > 4.97f)
    {
        return 1.0f;
    }
    float x2 = x * x;
    // float a = x * (135135.0f + x2 * (17325.0f + x2 * (378.0f + x2)));
    // float b = 135135.0f + x2 * (62370.0f + x2 * (3150.0f + x2 * 28.0f));
    // float a = x * (10395.0f + x2 * (1260.0f + x2 * 21.0f));
    // float b = 10395.0f + x2 * (4725.0f + x2 * (210.0f + x2));
    float a = x * (945.0f + x2 * (105.0f + x2));
    float b = 945.0f + x2 * (420.0f + x2 * 15.0f);
    return a / b;
}

static float fast_atanh(float x)
{
    float x2 = x * x;
    // float a = x * (-15015.0f + x2 * (19250.0f + x2 * (-5943.0f + x2 * 256.0f)));
    // float b = (-15015.0f + x2 * (24255.0f + x2 * (-11025.0f + x2 * 1225.0f)));
    // float a = x * (-1155.0f + x2 * (1190.0f + x2 * -231.0f));
    // float b = (-1155.0f + x2 * (1575.0f + x2 * (-525.0f + x2 * 25.0f)));
    float a = x * (945.0f + x2 * (-735.0f + x2 * 64.0f));
    float b = (945.0f + x2 * (-1050.0f + x2 * 225.0f));
    return a / b;
}

static float platanh(float x)
{
    int isign = 1;
    float z = x;
    if (x < 0.0f)
    {
        isign = -1;
        z = -x;
    }
    if (z <= 0.664f)
        return x / 0.83f;
    if (z <= 0.9217f)
        return isign * ((z - 0.4064f) / 0.322f);
    if (z <= 0.9951f)
        return isign * ((z - 0.8378f) / 0.0524f);
    if (z <= 0.9998f)
        return isign * ((z - 0.9914f) / 0.0012f);
    return isign * 7.0f;
}

typedef struct
{
    int index;
    float abs_llr;
} reliability_entry_t;

typedef struct
{
    int* head;
    int* next;
    int (*pairs)[2];
    int capacity;
    int count;
    int size;
    int last_pattern;
    int next_index;
} osd_box_t;

static void pack_bits91(const uint8_t bit_array[], int num_bits, uint8_t packed[])
{
    int num_bytes = (num_bits + 7) / 8;
    memset(packed, 0, (size_t)num_bytes);
    uint8_t mask = 0x80u;
    int byte_idx = 0;
    for (int i = 0; i < num_bits; ++i)
    {
        if (bit_array[i])
            packed[byte_idx] |= mask;
        mask >>= 1;
        if (mask == 0)
        {
            mask = 0x80u;
            ++byte_idx;
        }
    }
}

static void unpack_bits91(const uint8_t packed[], int num_bits, uint8_t bit_array[])
{
    for (int i = 0; i < num_bits; ++i)
        bit_array[i] = (packed[i / 8] >> (7 - (i % 8))) & 0x1u;
}

static bool check_crc91(const uint8_t plain91[])
{
    uint8_t a91[FTX_LDPC_K_BYTES];
    pack_bits91(plain91, FTX_LDPC_K, a91);
    uint16_t crc_extracted = ftx_extract_crc(a91);
    a91[9] &= 0xF8;
    a91[10] &= 0x00;
    uint16_t crc_calculated = ftx_compute_crc(a91, 96 - 14);
    return crc_extracted == crc_calculated;
}

static uint8_t parity8(uint8_t x)
{
    x ^= x >> 4;
    x ^= x >> 2;
    x ^= x >> 1;
    return x & 1u;
}

static void encode174_91_nocrc_bits(const uint8_t message91[], uint8_t codeword[])
{
    uint8_t packed[FTX_LDPC_K_BYTES];
    pack_bits91(message91, FTX_LDPC_K, packed);
    for (int i = 0; i < FTX_LDPC_K; ++i)
    {
        codeword[i] = message91[i] & 0x1u;
    }
    for (int i = 0; i < FTX_LDPC_M; ++i)
    {
        uint8_t nsum = 0;
        for (int j = 0; j < FTX_LDPC_K_BYTES; ++j)
        {
            nsum ^= parity8(packed[j] & kFTX_LDPC_generator[i][j]);
        }
        codeword[FTX_LDPC_K + i] = nsum & 0x1u;
    }
}

static int cmp_reliability_desc(const void* lhs, const void* rhs)
{
    const reliability_entry_t* a = (const reliability_entry_t*)lhs;
    const reliability_entry_t* b = (const reliability_entry_t*)rhs;
    if (a->abs_llr < b->abs_llr)
        return 1;
    if (a->abs_llr > b->abs_llr)
        return -1;
    return 0;
}

static void xor_rows(uint8_t* dst, const uint8_t* src, int len)
{
    for (int i = 0; i < len; ++i)
        dst[i] ^= src[i];
}

static void mrbencode91(const uint8_t* me, uint8_t* codeword, uint8_t* g2, int n, int k)
{
    memset(codeword, 0, (size_t)n);
    for (int i = 0; i < k; ++i)
    {
        if (me[i] == 0)
            continue;
        for (int j = 0; j < n; ++j)
            codeword[j] ^= g2[j * k + i];
    }
}

static void nextpat91(uint8_t* mi, int k, int iorder, int* iflag)
{
    int ind = -1;
    for (int i = 0; i < k - 1; ++i)
    {
        if (mi[i] == 0 && mi[i + 1] == 1)
            ind = i;
    }
    if (ind < 0)
    {
        *iflag = -1;
        return;
    }

    uint8_t* ms = (uint8_t*)calloc((size_t)k, sizeof(uint8_t));
    if (!ms)
    {
        *iflag = -1;
        return;
    }
    for (int i = 0; i < ind; ++i)
        ms[i] = mi[i];
    ms[ind] = 1;
    ms[ind + 1] = 0;
    if (ind + 1 < k)
    {
        int nz = iorder;
        for (int i = 0; i < k; ++i)
            nz -= ms[i];
        for (int i = k - nz; i < k; ++i)
            ms[i] = 1;
    }
    memcpy(mi, ms, (size_t)k);
    free(ms);

    *iflag = -1;
    for (int i = 0; i < k; ++i)
    {
        if (mi[i] == 1)
        {
            *iflag = i;
            break;
        }
    }
}

static bool osd_box_init(osd_box_t* box, int ntau)
{
    box->size = 1 << ntau;
    box->capacity = 5000;
    box->count = 0;
    box->last_pattern = -1;
    box->next_index = -1;
    box->head = (int*)malloc(sizeof(int) * box->size);
    box->next = (int*)malloc(sizeof(int) * box->capacity);
    box->pairs = malloc(sizeof(int[2]) * box->capacity);
    if (!box->head || !box->next || !box->pairs)
    {
        free(box->head);
        free(box->next);
        free(box->pairs);
        return false;
    }
    for (int i = 0; i < box->size; ++i)
        box->head[i] = -1;
    for (int i = 0; i < box->capacity; ++i)
    {
        box->next[i] = -1;
        box->pairs[i][0] = -1;
        box->pairs[i][1] = -1;
    }
    return true;
}

static void osd_box_free(osd_box_t* box)
{
    free(box->head);
    free(box->next);
    free(box->pairs);
}

static int pattern_hash(const uint8_t* e2, int ntau)
{
    int ipat = 0;
    for (int i = 0; i < ntau; ++i)
    {
        if (e2[i])
            ipat |= (1 << (ntau - i - 1));
    }
    return ipat;
}

static void boxit91(osd_box_t* box, const uint8_t* e2, int ntau, int i1, int i2)
{
    if (box->count >= box->capacity)
        return;
    int idx = box->count++;
    box->pairs[idx][0] = i1;
    box->pairs[idx][1] = i2;
    int ipat = pattern_hash(e2, ntau);
    int ip = box->head[ipat];
    if (ip == -1)
    {
        box->head[ipat] = idx;
    }
    else
    {
        while (box->next[ip] != -1)
            ip = box->next[ip];
        box->next[ip] = idx;
    }
}

static void fetchit91(osd_box_t* box, const uint8_t* e2, int ntau, int* i1, int* i2)
{
    int ipat = pattern_hash(e2, ntau);
    int index = box->head[ipat];
    if (box->last_pattern != ipat && index >= 0)
    {
        *i1 = box->pairs[index][0];
        *i2 = box->pairs[index][1];
        box->next_index = box->next[index];
    }
    else if (box->last_pattern == ipat && box->next_index >= 0)
    {
        *i1 = box->pairs[box->next_index][0];
        *i2 = box->pairs[box->next_index][1];
        box->next_index = box->next[box->next_index];
    }
    else
    {
        *i1 = -1;
        *i2 = -1;
        box->next_index = -1;
    }
    box->last_pattern = ipat;
}

static void osd174_91(float llr[], int k, uint8_t apmask[], int ndeep, uint8_t message91[], uint8_t cw[], int* nhardmin, float* dmin)
{
    const int n = FTX_LDPC_N;
    static bool gen_ready = false;
    static uint8_t gen[FTX_LDPC_K][FTX_LDPC_N];
    if (!gen_ready)
    {
        for (int i = 0; i < FTX_LDPC_K; ++i)
        {
            uint8_t msg[FTX_LDPC_K] = { 0 };
            msg[i] = 1;
            if (i < 77)
            {
                for (int j = 77; j < FTX_LDPC_K; ++j)
                    msg[j] = 0;
            }
            encode174_91_nocrc_bits(msg, gen[i]);
        }
        gen_ready = true;
    }

    uint8_t* genmrb = (uint8_t*)malloc((size_t)k * n);
    uint8_t* g2 = (uint8_t*)malloc((size_t)n * k);
    uint8_t* temp = (uint8_t*)malloc((size_t)k);
    uint8_t* m0 = (uint8_t*)malloc((size_t)k);
    uint8_t* me = (uint8_t*)malloc((size_t)k);
    uint8_t* mi = (uint8_t*)malloc((size_t)k);
    uint8_t* misub = (uint8_t*)malloc((size_t)k);
    uint8_t* e2sub = (uint8_t*)malloc((size_t)(n - k));
    uint8_t* e2 = (uint8_t*)malloc((size_t)(n - k));
    uint8_t* ui = (uint8_t*)malloc((size_t)(n - k));
    uint8_t* r2pat = (uint8_t*)malloc((size_t)(n - k));
    uint8_t* hdec = (uint8_t*)malloc((size_t)n);
    uint8_t* c0 = (uint8_t*)malloc((size_t)n);
    uint8_t* ce = (uint8_t*)malloc((size_t)n);
    uint8_t* nxor = (uint8_t*)malloc((size_t)n);
    uint8_t* apmaskr = (uint8_t*)malloc((size_t)n);
    float* rx = (float*)malloc(sizeof(float) * n);
    float* absrx = (float*)malloc(sizeof(float) * n);
    reliability_entry_t* rel = (reliability_entry_t*)malloc(sizeof(reliability_entry_t) * n);
    int* indices = (int*)malloc(sizeof(int) * n);
    if (!genmrb || !g2 || !temp || !m0 || !me || !mi || !misub || !e2sub || !e2 || !ui || !r2pat || !hdec || !c0 || !ce || !nxor || !apmaskr || !rx || !absrx || !rel || !indices)
    {
        goto cleanup;
    }

    for (int i = 0; i < n; ++i)
    {
        rx[i] = llr[i];
        apmaskr[i] = apmask[i];
        hdec[i] = (rx[i] >= 0.0f) ? 1u : 0u;
        absrx[i] = fabsf(rx[i]);
        rel[i].index = i;
        rel[i].abs_llr = absrx[i];
    }
    qsort(rel, n, sizeof(rel[0]), cmp_reliability_desc);
    for (int i = 0; i < n; ++i)
    {
        indices[i] = rel[i].index;
        for (int row = 0; row < k; ++row)
            genmrb[row * n + i] = gen[row][indices[i]];
    }

    for (int id = 0; id < k; ++id)
    {
        int max_col = k + 20;
        if (max_col > n)
            max_col = n;
        for (int col = id; col < max_col; ++col)
        {
            if (genmrb[id * n + col] == 0)
                continue;
            if (col != id)
            {
                for (int row = 0; row < k; ++row)
                {
                    uint8_t swap = genmrb[row * n + id];
                    genmrb[row * n + id] = genmrb[row * n + col];
                    genmrb[row * n + col] = swap;
                }
                int itmp = indices[id];
                indices[id] = indices[col];
                indices[col] = itmp;
            }
            for (int row = 0; row < k; ++row)
            {
                if (row != id && genmrb[row * n + id] == 1)
                    xor_rows(&genmrb[row * n], &genmrb[id * n], n);
            }
            break;
        }
    }

    for (int row = 0; row < k; ++row)
    {
        for (int col = 0; col < n; ++col)
            g2[col * k + row] = genmrb[row * n + col];
    }

    for (int i = 0; i < n; ++i)
    {
        hdec[i] = (rx[indices[i]] >= 0.0f) ? 1u : 0u;
        absrx[i] = fabsf(rx[indices[i]]);
        rx[i] = llr[indices[i]];
        apmaskr[i] = apmask[indices[i]];
    }
    for (int i = 0; i < k; ++i)
        m0[i] = hdec[i];

    mrbencode91(m0, c0, g2, n, k);
    for (int i = 0; i < n; ++i)
        nxor[i] = c0[i] ^ hdec[i];
    *nhardmin = 0;
    *dmin = 0.0f;
    for (int i = 0; i < n; ++i)
    {
        *nhardmin += nxor[i];
        *dmin += nxor[i] ? absrx[i] : 0.0f;
    }
    memcpy(cw, c0, (size_t)n);

    if (ndeep > 6)
        ndeep = 6;
    int nord = 0, npre1 = 0, npre2 = 0, nt = 0, ntheta = 0, ntau = 0;
    if (ndeep == 0)
    {
        goto reorder;
    }
    else if (ndeep == 1)
    {
        nord = 1; nt = 40; ntheta = 12;
    }
    else if (ndeep == 2)
    {
        nord = 1; npre1 = 1; nt = 40; ntheta = 10;
    }
    else if (ndeep == 3)
    {
        nord = 1; npre1 = 1; npre2 = 1; nt = 40; ntheta = 12; ntau = 14;
    }
    else if (ndeep == 4)
    {
        nord = 2; npre1 = 1; npre2 = 1; nt = 40; ntheta = 12; ntau = 17;
    }
    else if (ndeep == 5)
    {
        nord = 3; npre1 = 1; npre2 = 1; nt = 40; ntheta = 12; ntau = 15;
    }
    else
    {
        nord = 4; npre1 = 1; npre2 = 1; nt = 95; ntheta = 12; ntau = 15;
    }

    for (int iorder = 1; iorder <= nord; ++iorder)
    {
        memset(misub, 0, (size_t)k);
        for (int i = k - iorder; i < k; ++i)
            misub[i] = 1;
        int iflag = k - iorder;
        while (iflag >= 0)
        {
            int iend = (iorder == nord && npre1 == 0) ? iflag : 0;
            float d1 = 0.0f;
            for (int n1 = iflag; n1 >= iend; --n1)
            {
                memcpy(mi, misub, (size_t)k);
                mi[n1] = 1;
                bool masked = false;
                for (int i = 0; i < k; ++i)
                {
                    if (apmaskr[i] && mi[i])
                    {
                        masked = true;
                        break;
                    }
                }
                if (masked)
                    continue;
                for (int i = 0; i < k; ++i)
                    me[i] = m0[i] ^ mi[i];
                if (n1 == iflag)
                {
                    mrbencode91(me, ce, g2, n, k);
                    for (int i = 0; i < n - k; ++i)
                    {
                        e2sub[i] = ce[k + i] ^ hdec[k + i];
                        e2[i] = e2sub[i];
                    }
                    int nd1kpt = 1;
                    for (int i = 0; i < nt; ++i)
                        nd1kpt += e2sub[i];
                    d1 = 0.0f;
                    for (int i = 0; i < k; ++i)
                        d1 += ((me[i] ^ hdec[i]) ? absrx[i] : 0.0f);
                    if (nd1kpt <= ntheta)
                    {
                        float dd = d1;
                        for (int i = 0; i < n - k; ++i)
                            dd += e2sub[i] ? absrx[k + i] : 0.0f;
                        if (dd < *dmin)
                        {
                            *dmin = dd;
                            memcpy(cw, ce, (size_t)n);
                            *nhardmin = 0;
                            for (int i = 0; i < n; ++i)
                                *nhardmin += (ce[i] ^ hdec[i]);
                        }
                    }
                }
                else
                {
                    for (int i = 0; i < n - k; ++i)
                        e2[i] = e2sub[i] ^ g2[(k + i) * k + n1];
                    int nd1kpt = 2;
                    for (int i = 0; i < nt; ++i)
                        nd1kpt += e2[i];
                    if (nd1kpt <= ntheta)
                    {
                        mrbencode91(me, ce, g2, n, k);
                        float dd = d1 + ((ce[n1] ^ hdec[n1]) ? absrx[n1] : 0.0f);
                        for (int i = 0; i < n - k; ++i)
                            dd += e2[i] ? absrx[k + i] : 0.0f;
                        if (dd < *dmin)
                        {
                            *dmin = dd;
                            memcpy(cw, ce, (size_t)n);
                            *nhardmin = 0;
                            for (int i = 0; i < n; ++i)
                                *nhardmin += (ce[i] ^ hdec[i]);
                        }
                    }
                }
            }
            nextpat91(misub, k, iorder, &iflag);
        }
    }

    if (npre2 == 1)
    {
        osd_box_t box;
        if (osd_box_init(&box, ntau))
        {
            for (int i1 = k - 1; i1 >= 0; --i1)
            {
                for (int i2 = i1 - 1; i2 >= 0; --i2)
                {
                    for (int i = 0; i < ntau; ++i)
                        mi[i] = g2[(k + i) * k + i1] ^ g2[(k + i) * k + i2];
                    boxit91(&box, mi, ntau, i1, i2);
                }
            }

            memset(misub, 0, (size_t)k);
            for (int i = k - nord; i < k; ++i)
                misub[i] = 1;
            int iflag = k - nord;
            while (iflag >= 0)
            {
                for (int i = 0; i < k; ++i)
                    me[i] = m0[i] ^ misub[i];
                mrbencode91(me, ce, g2, n, k);
                for (int i = 0; i < n - k; ++i)
                    e2sub[i] = ce[k + i] ^ hdec[k + i];
                for (int i2 = 0; i2 <= ntau; ++i2)
                {
                    memset(ui, 0, (size_t)(n - k));
                    if (i2 > 0)
                        ui[i2 - 1] = 1;
                    for (int i = 0; i < ntau; ++i)
                        r2pat[i] = e2sub[i] ^ ui[i];
                    box.last_pattern = -1;
                    box.next_index = -1;
                    while (true)
                    {
                        int in1, in2;
                        fetchit91(&box, r2pat, ntau, &in1, &in2);
                        if (in1 < 0 || in2 < 0)
                            break;
                        memcpy(mi, misub, (size_t)k);
                        mi[in1] = 1;
                        mi[in2] = 1;
                        int w = 0;
                        bool masked = false;
                        for (int i = 0; i < k; ++i)
                        {
                            w += mi[i];
                            if (apmaskr[i] && mi[i])
                                masked = true;
                        }
                        if (w < nord + npre1 + npre2 || masked)
                            continue;
                        for (int i = 0; i < k; ++i)
                            me[i] = m0[i] ^ mi[i];
                        mrbencode91(me, ce, g2, n, k);
                        float dd = 0.0f;
                        int nh = 0;
                        for (int i = 0; i < n; ++i)
                        {
                            uint8_t diff = ce[i] ^ hdec[i];
                            nh += diff;
                            if (diff)
                                dd += absrx[i];
                        }
                        if (dd < *dmin)
                        {
                            *dmin = dd;
                            memcpy(cw, ce, (size_t)n);
                            *nhardmin = nh;
                        }
                    }
                }
                nextpat91(misub, k, nord, &iflag);
            }
            osd_box_free(&box);
        }
    }

reorder:
    {
        uint8_t reordered_cw[FTX_LDPC_N];
        for (int i = 0; i < n; ++i)
            reordered_cw[indices[i]] = cw[i];
        memcpy(cw, reordered_cw, (size_t)n);
        memcpy(message91, cw, FTX_LDPC_K);
        if (!check_crc91(message91))
            *nhardmin = -*nhardmin;
    }

cleanup:
    free(genmrb);
    free(g2);
    free(temp);
    free(m0);
    free(me);
    free(mi);
    free(misub);
    free(e2sub);
    free(e2);
    free(ui);
    free(r2pat);
    free(hdec);
    free(c0);
    free(ce);
    free(nxor);
    free(apmaskr);
    free(rx);
    free(absrx);
    free(rel);
    free(indices);
}

void decode174_91_osd(float llr[], int keff, int maxosd, int norder, uint8_t apmask[], uint8_t message91[], uint8_t cw[], int* ntype, int* nharderror, float* dmin)
{
    if (keff != FTX_LDPC_K)
    {
        *ntype = 0;
        *nharderror = -1;
        *dmin = 0.0f;
        return;
    }

    const int maxiterations = 30;
    int nosd = 0;
    if (maxosd > 3)
        maxosd = 3;
    float zsave[3][FTX_LDPC_N] = { { 0 } };
    if (maxosd == 0)
    {
        nosd = 1;
        memcpy(zsave[0], llr, sizeof(float) * FTX_LDPC_N);
    }
    else if (maxosd > 0)
    {
        nosd = maxosd;
    }

    float tov[FTX_LDPC_N][3] = { { 0 } };
    float toc[FTX_LDPC_M][7] = { { 0 } };
    float zsum[FTX_LDPC_N] = { 0 };
    uint8_t hdec[FTX_LDPC_N];
    uint8_t best_cw[FTX_LDPC_N] = { 0 };
    int ncnt = 0;
    int nclast = 0;

    for (int iter = 0; iter <= maxiterations; ++iter)
    {
        float zn[FTX_LDPC_N];
        for (int i = 0; i < FTX_LDPC_N; ++i)
        {
            zn[i] = llr[i];
            if (apmask[i] != 1)
                zn[i] += tov[i][0] + tov[i][1] + tov[i][2];
            zsum[i] += zn[i];
        }
        if (iter > 0 && iter <= maxosd)
            memcpy(zsave[iter - 1], zsum, sizeof(zsum));

        for (int i = 0; i < FTX_LDPC_N; ++i)
            best_cw[i] = (zn[i] > 0.0f) ? 1u : 0u;
        int ncheck = ldpc_check(best_cw);
        if (ncheck == 0 && check_crc91(best_cw))
        {
            memcpy(message91, best_cw, FTX_LDPC_K);
            memcpy(cw, best_cw, FTX_LDPC_N);
            for (int i = 0; i < FTX_LDPC_N; ++i)
                hdec[i] = (llr[i] >= 0.0f) ? 1u : 0u;
            *nharderror = 0;
            *dmin = 0.0f;
            for (int i = 0; i < FTX_LDPC_N; ++i)
            {
                uint8_t diff = hdec[i] ^ best_cw[i];
                *nharderror += diff;
                if (diff)
                    *dmin += fabsf(llr[i]);
            }
            *ntype = 1;
            return;
        }

        if (iter > 0)
        {
            int nd = ncheck - nclast;
            ncnt = (nd < 0) ? 0 : (ncnt + 1);
            if (ncnt >= 5 && iter >= 10 && ncheck > 15)
            {
                *nharderror = -1;
                break;
            }
        }
        nclast = ncheck;

        for (int m = 0; m < FTX_LDPC_M; ++m)
        {
            for (int n_idx = 0; n_idx < kFTX_LDPC_Num_rows[m]; ++n_idx)
            {
                int n = kFTX_LDPC_Nm[m][n_idx] - 1;
                toc[m][n_idx] = zn[n];
                for (int kk = 0; kk < 3; ++kk)
                {
                    if ((kFTX_LDPC_Mn[n][kk] - 1) == m)
                        toc[m][n_idx] -= tov[n][kk];
                }
            }
        }

        for (int m = 0; m < FTX_LDPC_M; ++m)
        {
            float tanhtoc[7];
            for (int i = 0; i < 7; ++i)
                tanhtoc[i] = tanhf(-toc[m][i] / 2.0f);
            for (int j = 0; j < kFTX_LDPC_Num_rows[m]; ++j)
            {
                int n = kFTX_LDPC_Nm[m][j] - 1;
                float Tmn = 1.0f;
                for (int n_idx = 0; n_idx < kFTX_LDPC_Num_rows[m]; ++n_idx)
                {
                    if ((kFTX_LDPC_Nm[m][n_idx] - 1) != n)
                        Tmn *= tanhtoc[n_idx];
                }
                for (int kk = 0; kk < 3; ++kk)
                {
                    if ((kFTX_LDPC_Mn[n][kk] - 1) == m)
                        tov[n][kk] = 2.0f * platanh(-Tmn);
                }
            }
        }
    }

    for (int i = 0; i < nosd; ++i)
    {
        int osd_harderror = -1;
        float osd_dmin = 0.0f;
        osd174_91(zsave[i], keff, apmask, norder, message91, cw, &osd_harderror, &osd_dmin);
        if (osd_harderror > 0)
        {
            *nharderror = osd_harderror;
            *dmin = 0.0f;
            for (int j = 0; j < FTX_LDPC_N; ++j)
            {
                hdec[j] = (llr[j] >= 0.0f) ? 1u : 0u;
                if ((hdec[j] ^ cw[j]) != 0)
                    *dmin += fabsf(llr[j]);
            }
            *ntype = 2;
            return;
        }
    }

    *ntype = 0;
    *nharderror = -1;
    *dmin = 0.0f;
}
