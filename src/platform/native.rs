pub use std::time::Instant;
pub use std::time::SystemTime;
pub use std::time::UNIX_EPOCH;

pub mod fs {
    use crate::platform::Result;
    pub fn read_to_string(path: impl AsRef<std::path::Path>) -> Result<String> {
        std::fs::read_to_string(path)
    }


    pub fn read(path: impl AsRef<std::path::Path>) -> Result<Vec<u8>> {
        std::fs::read(path)
    }


    pub fn write<P: AsRef<std::path::Path>, C: AsRef<[u8]>>(path: P, contents: C) -> Result<()> {
        let path = parse_tilde(path);
        std::fs::create_dir_all(path.parent().unwrap_or_else(|| std::path::Path::new("")))?;
        std::fs::write(path, contents)
    }


    pub fn read_dir(path: impl AsRef<std::path::Path>) -> Result<std::fs::ReadDir> {
        std::fs::read_dir(path)
    }


    pub fn is_dir(path: impl AsRef<std::path::Path>) -> bool {
        std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
    }


    pub fn read_local<P: AsRef<std::path::Path>>(path: P) -> Result<Vec<u8>> {
        read(parse_tilde(path))
    }


    fn parse_tilde(path: impl AsRef<std::path::Path>) -> std::path::PathBuf {
        if let Ok(stripped) = path.as_ref().strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                return std::path::Path::new(&home).join(stripped);
            }
        }

        path.as_ref().to_path_buf()
    }
}
