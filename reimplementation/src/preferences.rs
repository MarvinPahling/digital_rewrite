//! Small, bounded, versioned UI preferences. Preferences are deliberately separate from
//! circuit sessions and history; paths are encoded without lossy Unicode conversion.
use std::path::PathBuf;

pub const MAX_RECENT: usize = 10;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const MAX_PATH_BYTES: usize = 1024;
const HEADER: &str = "digital-rust-ui-preferences\t1\n";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiPreferences {
    pub recent: Vec<PathBuf>,
}

impl UiPreferences {
    pub fn promote(&mut self, path: PathBuf) -> Result<(), PreferencesError> {
        if !path.is_absolute() {
            return Err(PreferencesError::Path);
        }
        if encoded_path_len(&path)? > MAX_PATH_BYTES {
            return Err(PreferencesError::Limit);
        }
        self.recent.retain(|entry| !lexically_equal(entry, &path));
        self.recent.insert(0, path);
        self.recent.truncate(MAX_RECENT);
        Ok(())
    }

    pub fn remove(&mut self, path: &std::path::Path) {
        self.recent.retain(|entry| !lexically_equal(entry, path));
    }

    pub fn encode(&self) -> Result<String, PreferencesError> {
        if self.recent.len() > MAX_RECENT {
            return Err(PreferencesError::Limit);
        }
        let mut output = HEADER.to_owned();
        for path in &self.recent {
            if !path.is_absolute() {
                return Err(PreferencesError::Path);
            }
            if encoded_path_len(path)? > MAX_PATH_BYTES {
                return Err(PreferencesError::Limit);
            }
            let (tag, bytes) = encode_path(path)?;
            output.push_str(tag);
            output.push('\t');
            for byte in bytes {
                output.push_str(&format!("{byte:02x}"));
            }
            output.push('\n');
            if output.len() > MAX_RECORD_BYTES {
                return Err(PreferencesError::Limit);
            }
        }
        Ok(output)
    }

    pub fn decode(record: &str) -> Result<Self, PreferencesError> {
        if record.len() > MAX_RECORD_BYTES {
            return Err(PreferencesError::Limit);
        }
        let Some(body) = record.strip_prefix(HEADER) else {
            return Err(PreferencesError::Version);
        };
        let mut recent: Vec<PathBuf> = Vec::new();
        for line in body.lines() {
            if recent.len() == MAX_RECENT {
                return Err(PreferencesError::Limit);
            }
            let (tag, hex) = line.split_once('\t').ok_or(PreferencesError::Malformed)?;
            if hex.len() % 2 != 0 || hex.len() / 2 > MAX_PATH_BYTES {
                return Err(PreferencesError::Limit);
            }
            let mut bytes = Vec::with_capacity(hex.len() / 2);
            let digit = |b: u8| -> Option<u8> {
                match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    _ => None,
                }
            };
            let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
            debug_assert!(remainder.is_empty());
            for [high, low] in pairs {
                bytes.push(
                    digit(*high)
                        .and_then(|a| digit(*low).map(|b| a * 16 + b))
                        .ok_or(PreferencesError::Malformed)?,
                );
            }
            let path = decode_path(tag, &bytes)?;
            if !path.is_absolute() {
                return Err(PreferencesError::Path);
            }
            if !recent.iter().any(|entry| lexically_equal(entry, &path)) {
                recent.push(path);
            }
        }
        Ok(Self { recent })
    }
}

fn lexically_equal(left: &std::path::Path, right: &std::path::Path) -> bool {
    left.as_os_str() == right.as_os_str()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferencesError {
    Version,
    Malformed,
    Path,
    Limit,
    UnsupportedPlatformPath,
}
impl std::fmt::Display for PreferencesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Version => "unknown UI preferences version",
            Self::Malformed => "malformed UI preferences record",
            Self::Path => "recent file path must be absolute",
            Self::Limit => "UI preferences exceed the size/count/path limit",
            Self::UnsupportedPlatformPath => "this platform path cannot be persisted losslessly",
        })
    }
}
impl std::error::Error for PreferencesError {}

#[cfg(unix)]
fn encoded_path_len(path: &std::path::Path) -> Result<usize, PreferencesError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes().len())
}
#[cfg(windows)]
fn encoded_path_len(path: &std::path::Path) -> Result<usize, PreferencesError> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .count()
        .checked_mul(2)
        .ok_or(PreferencesError::Limit)
}
#[cfg(not(any(unix, windows)))]
fn encoded_path_len(_: &std::path::Path) -> Result<usize, PreferencesError> {
    Err(PreferencesError::UnsupportedPlatformPath)
}

#[cfg(unix)]
fn encode_path(path: &std::path::Path) -> Result<(&'static str, Vec<u8>), PreferencesError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(("unix", path.as_os_str().as_bytes().to_vec()))
}
#[cfg(windows)]
fn encode_path(path: &std::path::Path) -> Result<(&'static str, Vec<u8>), PreferencesError> {
    use std::os::windows::ffi::OsStrExt;
    Ok((
        "windows",
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect(),
    ))
}
#[cfg(not(any(unix, windows)))]
fn encode_path(_: &std::path::Path) -> Result<(&'static str, Vec<u8>), PreferencesError> {
    Err(PreferencesError::UnsupportedPlatformPath)
}

#[cfg(unix)]
fn decode_path(tag: &str, bytes: &[u8]) -> Result<PathBuf, PreferencesError> {
    use std::os::unix::ffi::OsStringExt;
    if tag != "unix" {
        return Err(PreferencesError::UnsupportedPlatformPath);
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())))
}
#[cfg(windows)]
fn decode_path(tag: &str, bytes: &[u8]) -> Result<PathBuf, PreferencesError> {
    use std::os::windows::ffi::OsStringExt;
    if tag != "windows" || bytes.len() % 2 != 0 {
        return Err(PreferencesError::UnsupportedPlatformPath);
    }
    Ok(PathBuf::from(std::ffi::OsString::from_wide(
        &bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>(),
    )))
}
#[cfg(not(any(unix, windows)))]
fn decode_path(_: &str, _: &[u8]) -> Result<PathBuf, PreferencesError> {
    Err(PreferencesError::UnsupportedPlatformPath)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_versioned_preferences_roundtrip_exact_lexical_paths_and_promote() {
        let mut prefs = UiPreferences::default();
        let first = PathBuf::from("/tmp/a/../circuit.dig");
        prefs.promote(first.clone()).unwrap();
        prefs.promote(PathBuf::from("/tmp/second.dig")).unwrap();
        prefs.promote(first.clone()).unwrap();
        assert_eq!(prefs.recent[0], first);
        prefs.promote(PathBuf::from("/tmp/circuit.dig")).unwrap();
        assert_eq!(
            prefs.recent.len(),
            3,
            "lexically distinct paths are kept distinct"
        );
        assert_eq!(
            UiPreferences::decode(&prefs.encode().unwrap()).unwrap(),
            prefs
        );
        for i in 0..15 {
            prefs
                .promote(PathBuf::from(format!("/tmp/{i}.dig")))
                .unwrap();
        }
        assert_eq!(prefs.recent.len(), MAX_RECENT);
        assert!(!prefs.recent.contains(&first));
    }
    #[test]
    fn malformed_unknown_version_and_unbounded_preferences_are_rejected() {
        assert_eq!(
            UiPreferences::decode("nope"),
            Err(PreferencesError::Version)
        );
        assert_eq!(
            UiPreferences::decode("digital-rust-ui-preferences\t9\n"),
            Err(PreferencesError::Version)
        );
        assert_eq!(
            UiPreferences::decode("digital-rust-ui-preferences\t1\nnot-a-record\n"),
            Err(PreferencesError::Malformed)
        );
        assert_eq!(
            UiPreferences::decode(&"x".repeat(MAX_RECORD_BYTES + 1)),
            Err(PreferencesError::Limit)
        );
        let too_many = format!(
            "{}{}",
            HEADER,
            (0..=MAX_RECENT)
                .map(|i| format!("unix\t2f746d702f{i}\n"))
                .collect::<String>()
        );
        assert_eq!(
            UiPreferences::decode(&too_many),
            Err(PreferencesError::Limit)
        );
    }
}
