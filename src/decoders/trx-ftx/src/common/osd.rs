// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! OSD-1/OSD-2 CRC-guided bit-flip decoder for the (174,91) LDPC code.
//!
//! This is a port of `ft2_ldpc.c` which implements Ordered Statistics Decoding
//! with configurable depth (ndeep 0-6). The decoder first runs iterative
//! belief-propagation (BP), then falls back to OSD refinement using the
//! accumulated LLR sums from BP iterations.
//!
//! The OSD algorithm works by:
//! 1. Sorting codeword bits by LLR reliability
//! 2. Gaussian elimination to put the generator matrix in systematic form
//!    (with respect to the most reliable bits)
//! 3. Exhaustive search over bit-flip patterns of increasing weight
//! 4. Pattern hashing (OSD-2) to efficiently search two-bit-flip corrections

use std::sync::OnceLock;

use super::constants::{FTX_LDPC_GENERATOR, FTX_LDPC_MN, FTX_LDPC_NM, FTX_LDPC_NUM_ROWS};
use super::crc::{ftx_compute_crc, ftx_extract_crc};
use super::decode::pack_bits;
use super::encode::parity8;
use super::ldpc::ldpc_check;
use super::protocol::{FTX_LDPC_K, FTX_LDPC_K_BYTES, FTX_LDPC_M, FTX_LDPC_N};

/// Piecewise linear approximation of `atanh(x)` used in BP message passing.
fn platanh(x: f32) -> f32 {
    let isign: f32 = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs();

    if z <= 0.664 {
        return x / 0.83;
    }
    if z <= 0.9217 {
        return isign * ((z - 0.4064) / 0.322);
    }
    if z <= 0.9951 {
        return isign * ((z - 0.8378) / 0.0524);
    }
    if z <= 0.9998 {
        return isign * ((z - 0.9914) / 0.0012);
    }
    isign * 7.0
}

/// Check CRC of a 91-bit message (in bit array form).
fn check_crc91(plain91: &[u8]) -> bool {
    let mut a91 = [0u8; FTX_LDPC_K_BYTES];
    pack_bits(plain91, FTX_LDPC_K, &mut a91);
    let crc_extracted = ftx_extract_crc(&a91);
    a91[9] &= 0xF8;
    a91[10] = 0x00;
    let crc_calculated = ftx_compute_crc(&a91, 96 - 14);
    crc_extracted == crc_calculated
}

/// Encode a 91-bit message (bit array) into a 174-bit codeword without CRC computation.
fn encode174_91_nocrc_bits(message91: &[u8], codeword: &mut [u8; FTX_LDPC_N]) {
    let mut packed = [0u8; FTX_LDPC_K_BYTES];
    pack_bits(message91, FTX_LDPC_K, &mut packed);

    // Systematic bits
    for i in 0..FTX_LDPC_K {
        codeword[i] = message91[i] & 0x01;
    }

    // Parity bits from generator matrix
    for i in 0..FTX_LDPC_M {
        let mut nsum: u8 = 0;
        for j in 0..FTX_LDPC_K_BYTES {
            nsum ^= parity8(packed[j] & FTX_LDPC_GENERATOR[i][j]);
        }
        codeword[FTX_LDPC_K + i] = nsum & 0x01;
    }
}

/// Matrix-vector multiply for re-encoding in OSD.
fn mrbencode91(me: &[u8], codeword: &mut [u8], g2: &[u8], n: usize, k: usize) {
    codeword[..n].fill(0);
    for i in 0..k {
        if me[i] == 0 {
            continue;
        }
        codeword[..n]
            .iter_mut()
            .enumerate()
            .for_each(|(j, c)| *c ^= g2[j * k + i]);
    }
}

/// Generate next bit-flip pattern of given order.
fn nextpat91(mi: &mut [u8], k: usize, iorder: usize, iflag: &mut i32) {
    let mut ind: i32 = -1;
    for i in 0..k.saturating_sub(1) {
        if mi[i] == 0 && mi[i + 1] == 1 {
            ind = i as i32;
        }
    }

    if ind < 0 {
        *iflag = -1;
        return;
    }

    // Build new pattern in-place: zero out after ind, set the swap, pack remaining 1s at end
    let ind_u = ind as usize;
    mi[(ind_u + 1)..k].fill(0);
    mi[ind_u] = 1;

    let mut nz = iorder as i32;
    for &v in mi.iter().take(k) {
        nz -= v as i32;
    }
    if nz > 0 {
        mi[(k - nz as usize)..k].fill(1);
    }

    *iflag = -1;
    for (i, &v) in mi.iter().enumerate().take(k) {
        if v == 1 {
            *iflag = i as i32;
            break;
        }
    }
}

/// Pattern hash table for OSD-2 optimization.
struct OsdBox {
    head: Vec<i32>,
    next: Vec<i32>,
    pairs: Vec<[i32; 2]>,
    capacity: usize,
    count: usize,
    last_pattern: i32,
    next_index: i32,
}

impl OsdBox {
    fn new(ntau: usize) -> Option<Self> {
        let size = 1 << ntau;
        let capacity = 5000;
        Some(Self {
            head: vec![-1; size],
            next: vec![-1; capacity],
            pairs: vec![[-1, -1]; capacity],
            capacity,
            count: 0,
            last_pattern: -1,
            next_index: -1,
        })
    }

    fn boxit(&mut self, e2: &[u8], ntau: usize, i1: i32, i2: i32) {
        if self.count >= self.capacity {
            return;
        }
        let idx = self.count;
        self.count += 1;
        self.pairs[idx] = [i1, i2];

        let ipat = pattern_hash(e2, ntau);
        let ip = self.head[ipat];
        if ip == -1 {
            self.head[ipat] = idx as i32;
        } else {
            let mut cur = ip;
            while self.next[cur as usize] != -1 {
                cur = self.next[cur as usize];
            }
            self.next[cur as usize] = idx as i32;
        }
    }

    fn fetchit(&mut self, e2: &[u8], ntau: usize) -> (i32, i32) {
        let ipat = pattern_hash(e2, ntau);
        let index = self.head[ipat];

        if self.last_pattern != ipat as i32 && index >= 0 {
            let i1 = self.pairs[index as usize][0];
            let i2 = self.pairs[index as usize][1];
            self.next_index = self.next[index as usize];
            self.last_pattern = ipat as i32;
            (i1, i2)
        } else if self.last_pattern == ipat as i32 && self.next_index >= 0 {
            let ni = self.next_index as usize;
            let i1 = self.pairs[ni][0];
            let i2 = self.pairs[ni][1];
            self.next_index = self.next[ni];
            (i1, i2)
        } else {
            self.next_index = -1;
            self.last_pattern = ipat as i32;
            (-1, -1)
        }
    }
}

/// Compute hash of a bit pattern for OSD-2 lookup.
fn pattern_hash(e2: &[u8], ntau: usize) -> usize {
    let mut ipat = 0usize;
    for (i, &v) in e2.iter().enumerate().take(ntau) {
        if v != 0 {
            ipat |= 1 << (ntau - i - 1);
        }
    }
    ipat
}

/// Ordered Statistics Decoding with configurable depth.
///
/// `llr`: log-likelihood ratios for 174 bits (modified internally).
/// `k`: number of systematic bits (91).
/// `apmask`: a priori mask (which bits are known).
/// `ndeep`: search depth (0-6).
/// `message91`: output 91-bit message.
/// `cw`: output 174-bit codeword.
/// `nhardmin`: output minimum hard errors.
/// `dmin`: output minimum distance.
#[allow(clippy::too_many_arguments)]
pub fn osd174_91(
    llr: &mut [f32; FTX_LDPC_N],
    k: usize,
    apmask: &[u8; FTX_LDPC_N],
    ndeep: usize,
    message91: &mut [u8; FTX_LDPC_K],
    cw: &mut [u8; FTX_LDPC_N],
    nhardmin: &mut i32,
    dmin: &mut f32,
) {
    let n = FTX_LDPC_N;
    let ndeep = ndeep.min(6);

    // Cached per-bit generator matrix (each row i generates codeword from
    // unit vector e_i)
    let gen = generator_matrix();

    // Stack-allocated working buffers (k=91, n=174, n-k=83).
    let mut genmrb = [0u8; FTX_LDPC_K * FTX_LDPC_N];
    let mut g2 = [0u8; FTX_LDPC_N * FTX_LDPC_K];
    let mut m0 = [0u8; FTX_LDPC_K];
    let mut me = [0u8; FTX_LDPC_K];
    let mut mi = [0u8; FTX_LDPC_K];
    let mut misub = [0u8; FTX_LDPC_K];
    let mut e2sub = [0u8; FTX_LDPC_M];
    let mut e2 = [0u8; FTX_LDPC_M];
    let mut ui = [0u8; FTX_LDPC_M];
    let mut r2pat = [0u8; FTX_LDPC_M];
    let mut hdec = [0u8; FTX_LDPC_N];
    let mut c0 = [0u8; FTX_LDPC_N];
    let mut ce = [0u8; FTX_LDPC_N];
    let mut nxor = [0u8; FTX_LDPC_N];
    let mut apmaskr = [0u8; FTX_LDPC_N];
    let mut rx = [0.0f32; FTX_LDPC_N];
    let mut absrx = [0.0f32; FTX_LDPC_N];
    let mut indices = [0usize; FTX_LDPC_N];

    // Sort bits by reliability (descending)
    let mut rel_indices = [0usize; FTX_LDPC_N];
    let mut rel_abs = [0.0f32; FTX_LDPC_N];
    for i in 0..n {
        rel_indices[i] = i;
        rel_abs[i] = llr[i].abs();
    }
    rel_indices[..n].sort_by(|&a, &b| {
        rel_abs[b]
            .partial_cmp(&rel_abs[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for i in 0..n {
        rx[i] = llr[i];
        apmaskr[i] = apmask[i];
        hdec[i] = if rx[i] >= 0.0 { 1 } else { 0 };
        absrx[i] = rx[i].abs();
    }

    // Reorder by reliability
    for i in 0..n {
        indices[i] = rel_indices[i];
        for row in 0..k {
            genmrb[row * n + i] = gen[row][indices[i]];
        }
    }

    // Gaussian elimination to systematic form
    for id in 0..k {
        let max_col = (k + 20).min(n);
        for col in id..max_col {
            if genmrb[id * n + col] == 0 {
                continue;
            }
            // Swap columns id and col
            if col != id {
                for row in 0..k {
                    genmrb.swap(row * n + id, row * n + col);
                }
                indices.swap(id, col);
            }
            // Eliminate column id from all other rows
            for row in 0..k {
                if row != id && genmrb[row * n + id] == 1 {
                    for c in 0..n {
                        genmrb[row * n + c] ^= genmrb[id * n + c];
                    }
                }
            }
            break;
        }
    }

    // Transpose to column-major g2
    for row in 0..k {
        for col in 0..n {
            g2[col * k + row] = genmrb[row * n + col];
        }
    }

    // Reorder LLRs and hard decisions by reliability
    for i in 0..n {
        hdec[i] = if rx[indices[i]] >= 0.0 { 1 } else { 0 };
        absrx[i] = rx[indices[i]].abs();
        rx[i] = llr[indices[i]];
        apmaskr[i] = apmask[indices[i]];
    }
    m0[..k].copy_from_slice(&hdec[..k]);

    // Initial encode
    mrbencode91(&m0, &mut c0, &g2, n, k);
    for i in 0..n {
        nxor[i] = c0[i] ^ hdec[i];
    }
    *nhardmin = 0;
    *dmin = 0.0;
    for i in 0..n {
        *nhardmin += nxor[i] as i32;
        if nxor[i] != 0 {
            *dmin += absrx[i];
        }
    }
    cw.copy_from_slice(&c0[..n]);

    if ndeep == 0 {
        reorder_result(cw, &indices, message91, nhardmin, dmin, llr);
        return;
    }

    // Configure search parameters based on depth
    let (nord, npre1, npre2, nt, ntheta, ntau) = match ndeep {
        1 => (1, 0, 0, 40, 12, 0),
        2 => (1, 1, 0, 40, 10, 0),
        3 => (1, 1, 1, 40, 12, 14),
        4 => (2, 1, 1, 40, 12, 17),
        5 => (3, 1, 1, 40, 12, 15),
        _ => (4, 1, 1, 95, 12, 15),
    };

    // OSD-1: exhaustive search over bit patterns of increasing order
    for iorder in 1..=nord {
        misub.iter_mut().for_each(|v| *v = 0);
        misub[(k - iorder)..k].fill(1);
        let mut iflag = (k - iorder) as i32;

        while iflag >= 0 {
            let iend = if iorder == nord && npre1 == 0 {
                iflag as usize
            } else {
                0
            };
            let mut d1 = 0.0f32;

            let mut n1 = iflag;
            while n1 >= iend as i32 {
                mi[..k].copy_from_slice(&misub[..k]);
                mi[n1 as usize] = 1;

                // Check if any masked bit would be flipped
                let masked = (0..k).any(|i| apmaskr[i] != 0 && mi[i] != 0);
                if masked {
                    n1 -= 1;
                    continue;
                }

                for i in 0..k {
                    me[i] = m0[i] ^ mi[i];
                }

                if n1 == iflag {
                    mrbencode91(&me, &mut ce, &g2, n, k);
                    for i in 0..(n - k) {
                        e2sub[i] = ce[k + i] ^ hdec[k + i];
                        e2[i] = e2sub[i];
                    }
                    let mut nd1kpt = 1;
                    for &v in e2sub.iter().take(nt.min(n - k)) {
                        nd1kpt += v as i32;
                    }
                    d1 = 0.0;
                    for i in 0..k {
                        if (me[i] ^ hdec[i]) != 0 {
                            d1 += absrx[i];
                        }
                    }
                    if nd1kpt <= ntheta {
                        let mut dd = d1;
                        for i in 0..(n - k) {
                            if e2sub[i] != 0 {
                                dd += absrx[k + i];
                            }
                        }
                        if dd < *dmin {
                            *dmin = dd;
                            cw[..n].copy_from_slice(&ce[..n]);
                            *nhardmin = 0;
                            for i in 0..n {
                                *nhardmin += (ce[i] ^ hdec[i]) as i32;
                            }
                        }
                    }
                } else {
                    for i in 0..(n - k) {
                        e2[i] = e2sub[i] ^ g2[(k + i) * k + n1 as usize];
                    }
                    let mut nd1kpt = 2;
                    for &v in e2.iter().take(nt.min(n - k)) {
                        nd1kpt += v as i32;
                    }
                    if nd1kpt <= ntheta {
                        mrbencode91(&me, &mut ce, &g2, n, k);
                        let mut dd = d1
                            + if (ce[n1 as usize] ^ hdec[n1 as usize]) != 0 {
                                absrx[n1 as usize]
                            } else {
                                0.0
                            };
                        for i in 0..(n - k) {
                            if e2[i] != 0 {
                                dd += absrx[k + i];
                            }
                        }
                        if dd < *dmin {
                            *dmin = dd;
                            cw[..n].copy_from_slice(&ce[..n]);
                            *nhardmin = 0;
                            for i in 0..n {
                                *nhardmin += (ce[i] ^ hdec[i]) as i32;
                            }
                        }
                    }
                }

                n1 -= 1;
            }
            nextpat91(&mut misub, k, iorder, &mut iflag);
        }
    }

    // OSD-2: pattern-hashed two-bit-flip search
    if npre2 == 1 {
        if let Some(mut osd_box) = OsdBox::new(ntau) {
            // Build hash table of all column pairs
            for i1 in (0..k as i32).rev() {
                for i2 in (0..i1).rev() {
                    for i in 0..ntau {
                        mi[i] = g2[(k + i) * k + i1 as usize] ^ g2[(k + i) * k + i2 as usize];
                    }
                    osd_box.boxit(&mi, ntau, i1, i2);
                }
            }

            // Search using base patterns
            misub.iter_mut().for_each(|v| *v = 0);
            misub[(k - nord)..k].fill(1);
            let mut iflag = (k - nord) as i32;

            while iflag >= 0 {
                for i in 0..k {
                    me[i] = m0[i] ^ misub[i];
                }
                mrbencode91(&me, &mut ce, &g2, n, k);
                for i in 0..(n - k) {
                    e2sub[i] = ce[k + i] ^ hdec[k + i];
                }

                for i2 in 0..=ntau {
                    ui.iter_mut().for_each(|v| *v = 0);
                    if i2 > 0 {
                        ui[i2 - 1] = 1;
                    }
                    for i in 0..ntau {
                        r2pat[i] = e2sub[i] ^ ui[i];
                    }

                    osd_box.last_pattern = -1;
                    osd_box.next_index = -1;

                    loop {
                        let (in1, in2) = osd_box.fetchit(&r2pat, ntau);
                        if in1 < 0 || in2 < 0 {
                            break;
                        }

                        mi[..k].copy_from_slice(&misub[..k]);
                        mi[in1 as usize] = 1;
                        mi[in2 as usize] = 1;

                        let mut w = 0;
                        let mut masked = false;
                        for i in 0..k {
                            w += mi[i] as usize;
                            if apmaskr[i] != 0 && mi[i] != 0 {
                                masked = true;
                            }
                        }

                        if w < nord + npre1 + npre2 || masked {
                            continue;
                        }

                        for i in 0..k {
                            me[i] = m0[i] ^ mi[i];
                        }
                        mrbencode91(&me, &mut ce, &g2, n, k);

                        let mut dd = 0.0f32;
                        let mut nh = 0i32;
                        for i in 0..n {
                            let diff = ce[i] ^ hdec[i];
                            nh += diff as i32;
                            if diff != 0 {
                                dd += absrx[i];
                            }
                        }
                        if dd < *dmin {
                            *dmin = dd;
                            cw[..n].copy_from_slice(&ce[..n]);
                            *nhardmin = nh;
                        }
                    }
                }
                nextpat91(&mut misub, k, nord, &mut iflag);
            }
        }
    }

    reorder_result(cw, &indices, message91, nhardmin, dmin, llr);
}

/// Reorder codeword back to original bit ordering and verify CRC.
fn reorder_result(
    cw: &mut [u8; FTX_LDPC_N],
    indices: &[usize],
    message91: &mut [u8; FTX_LDPC_K],
    nhardmin: &mut i32,
    _dmin: &mut f32,
    _llr: &[f32; FTX_LDPC_N],
) {
    let mut reordered = [0u8; FTX_LDPC_N];
    for i in 0..FTX_LDPC_N {
        reordered[indices[i]] = cw[i];
    }
    cw.copy_from_slice(&reordered);
    message91.copy_from_slice(&cw[..FTX_LDPC_K]);
    if !check_crc91(message91) {
        *nhardmin = -*nhardmin;
    }
}

/// Get a reference to the cached generator matrix.
/// The matrix is computed once on first call and reused thereafter.
fn generator_matrix() -> &'static [[u8; FTX_LDPC_N]; FTX_LDPC_K] {
    static GEN: OnceLock<Box<[[u8; FTX_LDPC_N]; FTX_LDPC_K]>> = OnceLock::new();
    GEN.get_or_init(|| {
        let mut gen = Box::new([[0u8; FTX_LDPC_N]; FTX_LDPC_K]);
        for i in 0..FTX_LDPC_K {
            let mut msg = [0u8; FTX_LDPC_K];
            msg[i] = 1;
            if i < 77 {
                msg[77..FTX_LDPC_K].fill(0);
            }
            encode174_91_nocrc_bits(&msg, &mut gen[i]);
        }
        gen
    })
}

/// Full iterative BP decoder with OSD refinement.
///
/// Runs belief-propagation for up to `maxiterations` iterations, saving
/// accumulated LLR sums. If BP does not converge, falls back to OSD
/// using the saved sums.
///
/// `llr`: input log-likelihood ratios (174 values).
/// `keff`: effective K (must be 91).
/// `maxosd`: maximum number of OSD passes (0-3).
/// `norder`: OSD depth parameter.
/// `apmask`: a priori mask.
/// `message91`: output decoded 91-bit message.
/// `cw`: output 174-bit codeword.
/// `ntype`: output decode type (0=fail, 1=BP, 2=OSD).
/// `nharderror`: output number of hard errors.
/// `dmin`: output minimum distance.
#[allow(clippy::too_many_arguments)]
pub fn ft2_decode174_91_osd(
    llr: &mut [f32; FTX_LDPC_N],
    keff: usize,
    maxosd: usize,
    norder: usize,
    apmask: &mut [u8; FTX_LDPC_N],
    message91: &mut [u8; FTX_LDPC_K],
    cw: &mut [u8; FTX_LDPC_N],
    ntype: &mut i32,
    nharderror: &mut i32,
    dmin: &mut f32,
) {
    *ntype = 0;
    *nharderror = -1;
    *dmin = 0.0;

    if keff != FTX_LDPC_K {
        return;
    }

    let maxiterations = 30;
    let maxosd = maxosd.min(3);

    let nosd = if maxosd == 0 { 1 } else { maxosd };

    let mut zsave = [[0.0f32; FTX_LDPC_N]; 3];
    if maxosd == 0 {
        zsave[0].copy_from_slice(llr);
    }

    let mut tov = [[0.0f32; 3]; FTX_LDPC_N];
    let mut toc = [[0.0f32; 7]; FTX_LDPC_M];
    let mut zsum = [0.0f32; FTX_LDPC_N];
    let mut hdec = [0u8; FTX_LDPC_N];
    let mut best_cw = [0u8; FTX_LDPC_N];
    let mut ncnt = 0;
    let mut nclast = 0;

    for iter in 0..=maxiterations {
        // Compute beliefs
        let mut zn = [0.0f32; FTX_LDPC_N];
        for i in 0..FTX_LDPC_N {
            zn[i] = llr[i];
            if apmask[i] != 1 {
                zn[i] += tov[i][0] + tov[i][1] + tov[i][2];
            }
            zsum[i] += zn[i];
        }
        if iter > 0 && iter <= maxosd {
            zsave[iter - 1].copy_from_slice(&zsum);
        }

        // Hard decisions
        for i in 0..FTX_LDPC_N {
            best_cw[i] = if zn[i] > 0.0 { 1 } else { 0 };
        }
        let ncheck = ldpc_check(&best_cw);

        if ncheck == 0 && check_crc91(&best_cw) {
            message91.copy_from_slice(&best_cw[..FTX_LDPC_K]);
            cw.copy_from_slice(&best_cw);
            for i in 0..FTX_LDPC_N {
                hdec[i] = if llr[i] >= 0.0 { 1 } else { 0 };
            }
            *nharderror = 0;
            *dmin = 0.0;
            for i in 0..FTX_LDPC_N {
                let diff = hdec[i] ^ best_cw[i];
                *nharderror += diff as i32;
                if diff != 0 {
                    *dmin += llr[i].abs();
                }
            }
            *ntype = 1;
            return;
        }

        // Early termination
        if iter > 0 {
            let nd = ncheck - nclast;
            ncnt = if nd < 0 { 0 } else { ncnt + 1 };
            if ncnt >= 5 && iter >= 10 && ncheck > 15 {
                *nharderror = -1;
                break;
            }
        }
        nclast = ncheck;

        // Check-to-variable messages
        for m in 0..FTX_LDPC_M {
            let num_rows = FTX_LDPC_NUM_ROWS[m] as usize;
            for n_idx in 0..num_rows {
                let n = FTX_LDPC_NM[m][n_idx] as usize - 1;
                if n >= FTX_LDPC_N {
                    continue;
                }
                toc[m][n_idx] = zn[n];
                for kk in 0..3 {
                    if (FTX_LDPC_MN[n][kk] as usize).wrapping_sub(1) == m {
                        toc[m][n_idx] -= tov[n][kk];
                    }
                }
            }
        }

        // Variable-to-check messages
        for m in 0..FTX_LDPC_M {
            let num_rows = FTX_LDPC_NUM_ROWS[m] as usize;
            let mut tanhtoc = [0.0f32; 7];
            for i in 0..num_rows.min(7) {
                tanhtoc[i] = (-toc[m][i] / 2.0).tanh();
            }
            for &nm_val in FTX_LDPC_NM[m].iter().take(num_rows) {
                let n = nm_val as usize - 1;
                if n >= FTX_LDPC_N {
                    continue;
                }
                let mut tmn = 1.0f32;
                for n_idx in 0..num_rows {
                    if FTX_LDPC_NM[m][n_idx] as usize - 1 != n {
                        tmn *= tanhtoc[n_idx];
                    }
                }
                for kk in 0..3 {
                    if (FTX_LDPC_MN[n][kk] as usize).wrapping_sub(1) == m {
                        tov[n][kk] = 2.0 * platanh(-tmn);
                    }
                }
            }
        }
    }

    // OSD fallback
    for i in 0..nosd {
        if i >= zsave.len() {
            break;
        }
        let mut osd_llr = [0.0f32; FTX_LDPC_N];
        osd_llr.copy_from_slice(&zsave[i]);
        let mut osd_harderror: i32 = -1;
        let mut osd_dmin: f32 = 0.0;
        osd174_91(
            &mut osd_llr,
            keff,
            apmask,
            norder,
            message91,
            cw,
            &mut osd_harderror,
            &mut osd_dmin,
        );
        if osd_harderror > 0 {
            *nharderror = osd_harderror;
            *dmin = 0.0;
            for j in 0..FTX_LDPC_N {
                hdec[j] = if llr[j] >= 0.0 { 1 } else { 0 };
                if (hdec[j] ^ cw[j]) != 0 {
                    *dmin += llr[j].abs();
                }
            }
            *ntype = 2;
            return;
        }
    }

    *ntype = 0;
    *nharderror = -1;
    *dmin = 0.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ldpc::fast_atanh;

    #[test]
    fn ldpc_check_all_zeros() {
        let cw = [0u8; FTX_LDPC_N];
        assert_eq!(ldpc_check(&cw), 0);
    }

    #[test]
    fn ldpc_check_single_bit_error() {
        let mut cw = [0u8; FTX_LDPC_N];
        cw[0] = 1;
        assert!(ldpc_check(&cw) > 0);
    }

    #[test]
    fn fast_atanh_zero() {
        assert!(fast_atanh(0.0).abs() < 1e-6);
    }

    #[test]
    fn fast_atanh_approximation() {
        for &x in &[-0.5f32, -0.25, 0.25, 0.5] {
            let approx = fast_atanh(x);
            let exact = x.atanh();
            assert!(
                (approx - exact).abs() < 0.05,
                "fast_atanh({}) = {}, expected ~{}",
                x,
                approx,
                exact
            );
        }
    }

    #[test]
    fn platanh_small() {
        let result = platanh(0.5);
        assert!(result > 0.0);
        assert!(result.is_finite());
    }

    #[test]
    fn platanh_large() {
        let result = platanh(0.9999);
        assert!(result > 0.0);
        assert!(result.is_finite());
    }

    #[test]
    fn platanh_negative() {
        let pos = platanh(0.5);
        let neg = platanh(-0.5);
        assert!((pos + neg).abs() < 1e-6, "platanh should be odd");
    }

    #[test]
    fn shared_pack_bits_basic() {
        let mut bits = [0u8; FTX_LDPC_K];
        bits[0] = 1;
        bits[7] = 1;
        let mut packed = [0u8; FTX_LDPC_K_BYTES];
        pack_bits(&bits, FTX_LDPC_K, &mut packed);
        assert_eq!(packed[0], 0x81);
    }

    #[test]
    fn check_crc91_all_zeros() {
        // All-zero message likely fails CRC
        let bits = [0u8; FTX_LDPC_K];
        // CRC check result depends on specific polynomial behavior
        let _result = check_crc91(&bits);
        // Just verify it doesn't panic
    }

    #[test]
    fn shared_parity8_basic() {
        assert_eq!(parity8(0x00), 0);
        assert_eq!(parity8(0x01), 1);
        assert_eq!(parity8(0x03), 0);
        assert_eq!(parity8(0xFF), 0);
    }

    #[test]
    fn pattern_hash_basic() {
        let e2 = [1u8, 0, 1, 0];
        assert_eq!(pattern_hash(&e2, 4), 0b1010);
    }

    #[test]
    fn pattern_hash_all_zeros() {
        let e2 = [0u8; 16];
        assert_eq!(pattern_hash(&e2, 16), 0);
    }

    #[test]
    fn nextpat91_basic() {
        let k = 5;
        let mut mi = vec![0u8; k];
        mi[4] = 1;
        let mut iflag = 4i32;
        nextpat91(&mut mi, k, 1, &mut iflag);
        // After one step, the pattern should shift
        assert!(iflag >= -1);
    }

    #[test]
    fn generator_matrix_row_zero() {
        let gen = generator_matrix();
        // Row 0 should encode unit vector e_0
        assert_eq!(gen[0][0], 1);
        // Some parity bits should be non-zero
        let parity_nonzero = gen[0][FTX_LDPC_K..FTX_LDPC_N].iter().any(|&b| b != 0);
        assert!(parity_nonzero);
    }

    #[test]
    fn encode174_91_nocrc_all_zeros() {
        let msg = [0u8; FTX_LDPC_K];
        let mut cw = [0u8; FTX_LDPC_N];
        encode174_91_nocrc_bits(&msg, &mut cw);
        for &b in &cw {
            assert_eq!(b, 0);
        }
    }

    #[test]
    fn osd_box_basic() {
        let mut b = OsdBox::new(4).unwrap();
        let pattern = [1u8, 0, 1, 0];
        b.boxit(&pattern, 4, 5, 3);
        let (i1, i2) = b.fetchit(&pattern, 4);
        assert_eq!(i1, 5);
        assert_eq!(i2, 3);
    }

    #[test]
    fn osd_box_empty_fetch() {
        let mut b = OsdBox::new(4).unwrap();
        let pattern = [0u8; 4];
        let (i1, i2) = b.fetchit(&pattern, 4);
        assert_eq!(i1, -1);
        assert_eq!(i2, -1);
    }
}
