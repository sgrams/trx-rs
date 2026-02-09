// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

fn main() {
    let base = "../../external/ft8_lib";
    let mut build = cc::Build::new();
    build
        .include(base)
        .include(format!("{base}/common"))
        .include(format!("{base}/fft"))
        .include(format!("{base}/ft8"))
        .define("_GNU_SOURCE", None)
        .define("_POSIX_C_SOURCE", "200809L")
        .file("src/ft8_wrapper.c")
        .file(format!("{base}/common/monitor.c"))
        .file(format!("{base}/fft/kiss_fft.c"))
        .file(format!("{base}/fft/kiss_fftr.c"))
        .file(format!("{base}/ft8/constants.c"))
        .file(format!("{base}/ft8/crc.c"))
        .file(format!("{base}/ft8/decode.c"))
        .file(format!("{base}/ft8/ldpc.c"))
        .file(format!("{base}/ft8/message.c"))
        .file(format!("{base}/ft8/text.c"))
        .flag_if_supported("-std=c99")
        .compile("trx_ft8");

    println!("cargo:rustc-link-lib=m");

    println!("cargo:rerun-if-changed=src/ft8_wrapper.c");
    println!("cargo:rerun-if-changed={base}/common/monitor.c");
    println!("cargo:rerun-if-changed={base}/fft/kiss_fft.c");
    println!("cargo:rerun-if-changed={base}/fft/kiss_fftr.c");
    println!("cargo:rerun-if-changed={base}/ft8/constants.c");
    println!("cargo:rerun-if-changed={base}/ft8/crc.c");
    println!("cargo:rerun-if-changed={base}/ft8/decode.c");
    println!("cargo:rerun-if-changed={base}/ft8/ldpc.c");
    println!("cargo:rerun-if-changed={base}/ft8/message.c");
    println!("cargo:rerun-if-changed={base}/ft8/text.c");
}
