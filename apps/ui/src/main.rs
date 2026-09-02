//! The desktop binary.
//!
//! Everything is in the library beside this file. That is not tidiness for its own sake:
//! Android never calls a `main`, it loads a shared object and looks for `android_main`,
//! so the application has to live somewhere a `cdylib` can expose.

// A GUI application should not also open a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    vds_admin::run()
}
