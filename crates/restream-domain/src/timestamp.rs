//! The RFC 3339 forms an instant takes on the wire: API JSON, SQLite rows,
//! app-log lines and harness artifacts. Every writer formats through here,
//! so a stored or served timestamp keeps one shape whatever produced it.

use std::time::SystemTime;

use jiff::{Timestamp, fmt::temporal::DateTimePrinter, tz::Offset};

/// Fractional-second digits of a timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    /// None: the fraction is truncated.
    Seconds,
    /// Always three digits, truncated.
    Millis,
    /// None, 3, 6 or 9 digits: the fewest whole groups that keep every
    /// nonzero digit.
    Auto,
}

/// `2026-01-01T08:30:45.123456789+00:00`: [`Precision::Auto`] digits and a
/// `+00:00` offset. Job rows, alert `firstSeen`/`lastSeen`, `generatedAt`.
pub fn rfc3339(at: SystemTime) -> String {
    let (at, printer) = printer(at, Precision::Auto);
    printer.timestamp_with_offset_to_string(&at, Offset::UTC)
}

/// [`rfc3339`] of now.
pub fn now_rfc3339() -> String {
    rfc3339(SystemTime::now())
}

/// `2026-01-01T08:30:45.123Z`: `precision` digits and a `Z` offset.
pub fn rfc3339_utc(at: SystemTime, precision: Precision) -> String {
    let (at, printer) = printer(at, precision);
    printer.timestamp_to_string(&at)
}

/// Serde `serialize_with` for an instant field: [`rfc3339_utc`] with
/// [`Precision::Auto`].
pub fn serialize<S: serde::Serializer>(at: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&rfc3339_utc(*at, Precision::Auto))
}

/// `at` as a [`Timestamp`] (clamped to its years -9999..=9999) and a printer
/// for `precision`. A pre-epoch fraction is negative; whole groups divide
/// it the same either way.
fn printer(at: SystemTime, precision: Precision) -> (Timestamp, DateTimePrinter) {
    let at = Timestamp::try_from(at).unwrap_or(if at < SystemTime::UNIX_EPOCH {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    });
    let digits = match precision {
        Precision::Seconds => 0,
        Precision::Millis => 3,
        Precision::Auto => match at.subsec_nanosecond() {
            0 => 0,
            nanos if nanos % 1_000_000 == 0 => 3,
            nanos if nanos % 1_000 == 0 => 6,
            _ => 9,
        },
    };
    (at, DateTimePrinter::new().precision(Some(digits)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    /// 2026-01-01T08:30:45Z plus `nanos`.
    fn at(nanos: u32) -> SystemTime {
        UNIX_EPOCH + Duration::new(1_767_256_245, nanos)
    }

    const AUTO_DIGITS: [(u32, &str); 7] = [
        (0, ""),
        (123_000_000, ".123"),
        (123_400_000, ".123400"),
        (123_456_000, ".123456"),
        (123_456_789, ".123456789"),
        (1_000, ".000001"),
        (1, ".000000001"),
    ];

    #[test]
    fn rfc3339_prints_whole_digit_groups_and_a_numeric_utc_offset() {
        for (nanos, fraction) in AUTO_DIGITS {
            assert_eq!(
                rfc3339(at(nanos)),
                format!("2026-01-01T08:30:45{fraction}+00:00")
            );
        }
        assert_eq!(
            rfc3339(UNIX_EPOCH - Duration::from_nanos(1)),
            "1969-12-31T23:59:59.999999999+00:00"
        );
    }

    #[test]
    fn rfc3339_utc_auto_prints_whole_digit_groups_and_z() {
        for (nanos, fraction) in AUTO_DIGITS {
            assert_eq!(
                rfc3339_utc(at(nanos), Precision::Auto),
                format!("2026-01-01T08:30:45{fraction}Z")
            );
        }
    }

    #[test]
    fn rfc3339_utc_seconds_and_millis_truncate() {
        assert_eq!(
            rfc3339_utc(at(999_999_999), Precision::Seconds),
            "2026-01-01T08:30:45Z"
        );
        assert_eq!(
            rfc3339_utc(at(0), Precision::Millis),
            "2026-01-01T08:30:45.000Z"
        );
        assert_eq!(
            rfc3339_utc(at(123_456_789), Precision::Millis),
            "2026-01-01T08:30:45.123Z"
        );
        assert_eq!(
            rfc3339_utc(at(999_999_999), Precision::Millis),
            "2026-01-01T08:30:45.999Z"
        );
    }
}
