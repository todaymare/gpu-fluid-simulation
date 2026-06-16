pub use instant::Instant;
pub use instant::SystemTime;
pub const UNIX_EPOCH: SystemTime = SystemTime::UNIX_EPOCH;

pub mod fs {
    use crate::platform::Result;
    use std::path::Path;
    use include_dir::{include_dir, Dir, DirEntry};
    use wasm_bindgen::prelude::*;

    pub static ASSETS: Dir<'static> = include_dir!("shaders");

    pub fn read_to_string(path: impl AsRef<std::path::Path>) -> Result<String> {
        String::from_utf8(read(path)?).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }


    pub fn read(path: impl AsRef<Path>) -> Result<Vec<u8>> {
        // ASSETS is bundled from the `shaders/` dir, so paths rooted at
        // `shaders/` (used as cwd-relative paths on native) need the prefix
        // stripped to match the bundle's root.
        let path = strip_shaders_prefix(path.as_ref());
        ASSETS
            .get_file(path)
            .map(|f| f.contents().to_vec())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "File not found"))
    }


    pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> Result<()> {
        let window = web_sys::window().unwrap();
        let storage = window.local_storage().unwrap().unwrap();
        let encoded = base64(contents.as_ref());
        storage.set_item(&path.as_ref().display().to_string(), &encoded).unwrap();

        Ok(())
    }


    pub fn read_local<P: AsRef<Path>>(path: P) -> Result<Vec<u8>> {
        let window = web_sys::window().unwrap();
        let storage = window.local_storage().unwrap().unwrap();
        let encoded = storage.get_item(&path.as_ref().display().to_string()).unwrap();
        if let Some(encoded) = encoded {
            // decode base64
            let decoded = hex::decode(encoded).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(decoded)
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "Local file not found"))
        }
    }


    pub fn read_dir<'a, P: AsRef<Path>>(path: P) -> Result<impl Iterator<Item = Result<&'a DirEntry<'a>>>> {
        let Some(dir) = ASSETS.get_dir(&path)
        else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "Directory not found"));
        };

        Ok(
            dir
            .entries()
            .iter()
            .map(|entry| Ok(entry))
        )
    }


    pub fn is_dir(path: impl AsRef<Path>) -> bool {
        let path = strip_shaders_prefix(path.as_ref());
        ASSETS.get_dir(path).is_some()
    }


    fn strip_shaders_prefix(path: &Path) -> &Path {
        path.strip_prefix("shaders/").unwrap_or(path)
    }


    fn base64(data: &[u8]) -> String {
        // js-sys has btoa but it only takes strings
        // encode as base64 manually or use a crate
        let encoded = data.iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        encoded
    }

}
