fn main() {
    // icono del ejecutable de Windows (recurso precompilado, solo x86-64)
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
    {
        let res = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/icon.res");
        println!("cargo:rustc-link-arg-bins={}", res.display());
        println!("cargo:rerun-if-changed=assets/icon.res");
    }
}
