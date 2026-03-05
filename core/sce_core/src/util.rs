use chrono::{SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn is_audio_file(path: &Path) -> bool {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
    {
        Some(ext) => matches!(
            ext.as_str(),
            "wav" | "aiff" | "aif" | "flac" | "mp3" | "m4a"
        ),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::now_rfc3339;

    #[test]
    fn now_is_utc_millis_rfc3339() {
        let now = now_rfc3339();
        assert!(now.ends_with('Z'));
        let parsed = chrono::DateTime::parse_from_rfc3339(&now).expect("valid RFC3339");
        assert_eq!(parsed.offset().local_minus_utc(), 0);
    }
}
