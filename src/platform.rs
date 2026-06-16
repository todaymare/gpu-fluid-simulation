#[cfg(target_family = "wasm")]
mod wasm;

#[cfg(target_family = "wasm")]
pub use wasm::*;

#[cfg(not(target_family = "wasm"))]
mod native;

#[cfg(not(target_family = "wasm"))]
pub use native::*;

pub use std::io::Result;

#[cfg(target_os = "windows")]
pub const SAVE_FILE: &str = "C:/Users/User/AppData/Roaming/molasses/save.bin";

#[cfg(target_os = "macos")]
pub const SAVE_FILE: &str = "~/Library/Application Support/molasses/save.bin";

#[cfg(target_os = "linux")]
pub const SAVE_FILE: &str = "~/.local/share/molasses/save.bin";

#[cfg(target_family = "wasm")]
pub const SAVE_FILE: &str = "molasses_save";
