// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use libc::{c_float, c_int, c_void};
use std::ffi::CStr;
use std::ptr::NonNull;

const F_MIN_HZ: f32 = 200.0;
const F_MAX_HZ: f32 = 3000.0;
const TIME_OSR: i32 = 2;
const FREQ_OSR: i32 = 2;

#[repr(C)]
struct Ft8DecodeResultRaw {
    text: [libc::c_char; 64],
    snr_db: c_float,
    dt_s: c_float,
    freq_hz: c_float,
}

#[derive(Debug, Clone)]
pub struct Ft8DecodeResult {
    pub text: String,
    pub snr_db: f32,
    pub dt_s: f32,
    pub freq_hz: f32,
}

extern "C" {
    fn ft8_decoder_create(
        sample_rate: c_int,
        f_min: c_float,
        f_max: c_float,
        time_osr: c_int,
        freq_osr: c_int,
    ) -> *mut c_void;
    fn ft8_decoder_free(dec: *mut c_void);
    fn ft8_decoder_block_size(dec: *const c_void) -> c_int;
    fn ft8_decoder_reset(dec: *mut c_void);
    fn ft8_decoder_process(dec: *mut c_void, frame: *const c_float);
    fn ft8_decoder_is_ready(dec: *const c_void) -> c_int;
    fn ft8_decoder_decode(dec: *mut c_void, out: *mut Ft8DecodeResultRaw, max_results: c_int) -> c_int;
}

pub struct Ft8Decoder {
    inner: NonNull<c_void>,
    block_size: usize,
    sample_rate: u32,
}

impl Ft8Decoder {
    pub fn new(sample_rate: u32) -> Result<Self, String> {
        unsafe {
            let ptr = ft8_decoder_create(
                sample_rate as c_int,
                F_MIN_HZ,
                F_MAX_HZ,
                TIME_OSR as c_int,
                FREQ_OSR as c_int,
            );
            let inner = NonNull::new(ptr).ok_or_else(|| "ft8_decoder_create failed".to_string())?;
            let block_size = ft8_decoder_block_size(inner.as_ptr()) as usize;
            if block_size == 0 {
                ft8_decoder_free(inner.as_ptr());
                return Err("invalid FT8 block size".to_string());
            }
            Ok(Self {
                inner,
                block_size,
                sample_rate,
            })
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn reset(&mut self) {
        unsafe {
            ft8_decoder_reset(self.inner.as_ptr());
        }
    }

    pub fn process_block(&mut self, block: &[f32]) {
        if block.len() < self.block_size {
            return;
        }
        unsafe {
            ft8_decoder_process(self.inner.as_ptr(), block.as_ptr());
        }
    }

    pub fn decode_if_ready(&mut self, max_results: usize) -> Vec<Ft8DecodeResult> {
        unsafe {
            if ft8_decoder_is_ready(self.inner.as_ptr()) == 0 {
                return Vec::new();
            }
            let mut raw = vec![Ft8DecodeResultRaw {
                text: [0; 64],
                snr_db: 0.0,
                dt_s: 0.0,
                freq_hz: 0.0,
            }; max_results];
            let count = ft8_decoder_decode(self.inner.as_ptr(), raw.as_mut_ptr(), max_results as c_int);
            let count = count.max(0) as usize;
            let mut out = Vec::with_capacity(count);
            for item in raw.into_iter().take(count) {
                let text = unsafe { CStr::from_ptr(item.text.as_ptr()) }
                    .to_string_lossy()
                    .to_string();
                out.push(Ft8DecodeResult {
                    text,
                    snr_db: item.snr_db,
                    dt_s: item.dt_s,
                    freq_hz: item.freq_hz,
                });
            }
            out
        }
    }
}

impl Drop for Ft8Decoder {
    fn drop(&mut self) {
        unsafe {
            ft8_decoder_free(self.inner.as_ptr());
        }
    }
}
