fn main() {
    #[cfg(feature = "slint-ui")]
    compile_slint();
}

#[cfg(feature = "slint-ui")]
fn compile_slint() {
    let debug = std::env::var("PROFILE").as_deref() != Ok("release");
    println!("cargo:rustc-env=SLINT_EMIT_DEBUG_INFO={}", u8::from(debug));
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_debug_info(debug),
    )
    .expect("SnenkBot UI must compile");
}
