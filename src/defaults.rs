use serde::Deserialize;
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Defaults {
    pub backlog: Option<u32>,
    pub pinned_units: Vec<String>,
    pub muted_units: Vec<String>,
}

impl Defaults {
    pub fn load(path: &Path) -> io::Result<Self> {
        let file = match File::open(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            result => result?,
        };
        Self::read(file, path)
    }

    fn read(reader: impl Read, path: &Path) -> io::Result<Self> {
        const LIMIT: usize = 256 * 1024;
        let mut bytes = Vec::new();
        reader.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
        let invalid = |reason: &str| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {reason}", path.display()),
            )
        };
        if bytes.len() > LIMIT {
            return Err(invalid("configuration exceeds 256 KiB"));
        }
        let defaults: Self = serde_json::from_slice(&bytes)
            .map_err(|e| invalid(&jscope::journal::sanitize(&e.to_string())))?;
        if defaults
            .pinned_units
            .iter()
            .chain(&defaults.muted_units)
            .any(String::is_empty)
        {
            return Err(invalid("unit names must be nonempty"));
        }
        Ok(defaults)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_bound_the_file() {
        let path = Path::new("defaults.json");
        let valid = br#"{"backlog":0,"pinnedUnits":["a.service"],"mutedUnits":["b.service"]}"#;
        let parsed = Defaults::read(&valid[..], path).unwrap();
        assert_eq!(parsed.backlog, Some(0));
        assert_eq!(parsed.pinned_units, ["a.service"]);
        assert_eq!(parsed.muted_units, ["b.service"]);

        for invalid in [r#"{"bogus":2}"#, r#"{"pinnedUnits":[""]}"#, "{"] {
            assert!(Defaults::read(invalid.as_bytes(), path).is_err());
        }
        let mut oversized = std::io::repeat(b' ').take(1024 * 1024);
        assert!(Defaults::read(&mut oversized, path).is_err());
        assert_eq!(oversized.limit(), 1024 * 1024 - 256 * 1024 - 1);
        assert!(
            Defaults::load(Path::new("fixtures/missing-jscope-defaults.json"))
                .unwrap()
                .backlog
                .is_none()
        );
    }
}
