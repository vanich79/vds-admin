//! Compiles the `.slint` files into Rust at build time.

fn main() {
    // The software renderer is the portable choice: it works identically on a desktop
    // with a GPU, a headless CI runner and an ARM board with no working driver, which is
    // exactly the range this project has to cover.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".to_owned());

    if let Err(error) = slint_build::compile_with_config("ui/app.slint", config) {
        // The build genuinely cannot continue. Reported and exited rather than panicked,
        // so the developer sees the compiler's message and not a backtrace through
        // `slint-build`.
        eprintln!("could not compile the Slint interface: {error}");
        std::process::exit(1);
    }
}
