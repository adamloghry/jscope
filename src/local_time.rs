use time::{OffsetDateTime, UtcOffset};

/// Format journal realtime using the viewer's time zone at the instant of the entry.
pub(crate) fn format(realtime_us: Option<u64>) -> String {
    let local = realtime_us.and_then(|us| {
        let utc = OffsetDateTime::from_unix_timestamp_nanos(i128::from(us) * 1_000).ok()?;
        let offset = UtcOffset::local_offset_at(utc).ok()?;
        utc.checked_to_offset(offset)
    });
    match local {
        Some(timestamp) => {
            let (h, m, s, ms) = timestamp.to_hms_milli();
            format!("{h:02}:{m:02}:{s:02}.{ms:03}")
        }
        None => "--:--:--.---".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::format;
    use std::{env, process::Command};

    #[test]
    fn missing_or_unrepresentable_time() {
        assert_eq!(format(None), "--:--:--.---");
        assert_eq!(format(Some(u64::MAX)), "--:--:--.---");
    }

    #[test]
    fn daylight_saving_boundaries() {
        // A POSIX rule works in the sandbox without installed IANA zone files.
        // Set TZ only in a new process, leaving other tests' time zones alone.
        const ZONE: &str = "EST5EDT,M3.2.0/2,M11.1.0/2";
        if env::var("TZ").as_deref() != Ok(ZONE) {
            let child = Command::new(env::current_exe().unwrap())
                .args(["--exact", "local_time::tests::daylight_saving_boundaries"])
                .env("TZ", ZONE)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&child.stdout);
            assert!(
                child.status.success() && stdout.contains("1 passed"),
                "{stdout}"
            );
            return;
        }

        // Spring skips 02:00, and autumn repeats 01:00.
        assert_eq!(format(Some(1_710_053_999_987_654)), "01:59:59.987");
        assert_eq!(format(Some(1_710_054_000_000_000)), "03:00:00.000");
        assert_eq!(format(Some(1_730_613_599_999_999)), "01:59:59.999");
        assert_eq!(format(Some(1_730_613_600_000_000)), "01:00:00.000");
    }
}
