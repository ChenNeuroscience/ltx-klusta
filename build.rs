fn main() {
    println!("cargo:rerun-if-env-changed=PREKK_LAPACK_LIB_DIR");
    if std::env::var_os("CARGO_FEATURE_LAPACK").is_none() {
        return;
    }
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => println!("cargo:rustc-link-lib=framework=Accelerate"),
        Ok("windows") => {
            let directory = std::env::var("PREKK_LAPACK_LIB_DIR")
                .unwrap_or_else(|_| std::env::var("CARGO_MANIFEST_DIR").unwrap());
            println!("cargo:rustc-link-search=native={directory}");
            println!("cargo:rustc-link-lib=dylib=lapack_win64_MT");
            println!("cargo:rustc-link-lib=dylib=blas_win64_MT");
        }
        Ok("linux") => {
            println!("cargo:rustc-link-lib=lapack");
            println!("cargo:rustc-link-lib=blas");
        }
        _ => panic!("LAPACK linking is configured for macOS, Windows x64, and Linux"),
    }
}
