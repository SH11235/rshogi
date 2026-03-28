fn main() {
    cc::Build::new()
        .file("kernel.c")
        .opt_level(3)
        .flag("-msse2")
        .flag("-mno-avx")
        .flag("-mno-avx2")
        .flag("-fno-lto")
        .compile("bitboard_kernel");
}
