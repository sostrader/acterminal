use chrono::DateTime;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum UserTimezone {
    #[default]
    Utc,
    Local,
}

/// Specifies the *purpose* of a timestamp label when requesting a formatted
/// string from a `UserTimezone` instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeLabelKind<'a> {
    /// Formatting suitable for axis ticks.  Will choose the appropriate
    /// `HH:MM`, `MM:SS`, or `D` style based on the timeframe.
    Axis { timeframe: exchange::Timeframe },
    /// Formatting for the crosshair tooltip.
    /// Sub-10-second intervals will show `HH:MM:SS.mmm`,
    /// while larger intervals will show `Day Mon D HH:MM`.
    Crosshair { show_millis: bool },
    /// Arbitrary formatting using the given `chrono` specifier string.
    Custom(&'a str),
}

impl UserTimezone {
    pub fn to_user_datetime(
        &self,
        datetime: DateTime<chrono::Utc>,
    ) -> DateTime<chrono::FixedOffset> {
        self.with_user_timezone(datetime, |time_with_zone| time_with_zone)
    }

    /// Formats a Unix timestamp (milliseconds) according to the kind.
    pub fn format_with_kind(&self, timestamp_ms: i64, kind: TimeLabelKind<'_>) -> Option<String> {
        DateTime::from_timestamp_millis(timestamp_ms).map(|datetime| {
            self.with_user_timezone(datetime, |time_with_zone| match kind {
                TimeLabelKind::Axis { timeframe } => {
                    Self::format_by_timeframe(&time_with_zone, timeframe)
                }
                TimeLabelKind::Crosshair { show_millis } => {
                    if show_millis {
                        time_with_zone.format("%H:%M:%S.%3f").to_string()
                    } else {
                        time_with_zone.format("%a %b %-d %H:%M").to_string()
                    }
                }
                TimeLabelKind::Custom(fmt) => time_with_zone.format(fmt).to_string(),
            })
        })
    }

    /// IANA name of the OS-configured timezone (e.g. `"America/Sao_Paulo"`),
    /// read without consulting the process environment: first the
    /// `/etc/localtime` symlink (a plain `zoneinfo/<Area>/<City>` path on
    /// macOS and Linux, immune to `TZ`), then the platform API. `chrono::Local`
    /// trusts `TZ`/tzdata, which is how a correctly configured GMT-3 machine
    /// can still report UTC inside the app.
    fn system_tz_name() -> Option<String> {
        if let Ok(link) = std::fs::read_link("/etc/localtime")
            && let Some(path) = link.to_str()
            && let Some((_, zone)) = path.split_once("zoneinfo/")
            && !zone.is_empty()
        {
            return Some(zone.to_owned());
        }
        iana_time_zone::get_timezone().ok()
    }

    /// Offset of the system timezone at `datetime` (DST-aware), falling back
    /// to `chrono::Local` and finally to UTC when the zone is unresolvable.
    fn system_offset(datetime: DateTime<chrono::Utc>) -> chrono::FixedOffset {
        use chrono::{Offset, TimeZone};
        use std::str::FromStr;

        if let Some(name) = Self::system_tz_name()
            && let Ok(tz) = chrono_tz::Tz::from_str(&name)
        {
            return tz.offset_from_utc_datetime(&datetime.naive_utc()).fix();
        }
        let secs = datetime
            .with_timezone(&chrono::Local)
            .offset()
            .local_minus_utc();
        // `local_minus_utc` always fits; the fallback only placates the type system.
        chrono::FixedOffset::east_opt(secs)
            .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("zero offset is valid"))
    }

    /// Offset in milliseconds to add to a UTC timestamp to display it in this
    /// timezone (DST-aware for Local; 0 for UTC or unparseable input).
    pub fn offset_ms_at(&self, timestamp_ms: i64) -> i64 {
        match self {
            UserTimezone::Utc => 0,
            UserTimezone::Local => DateTime::from_timestamp_millis(timestamp_ms)
                .map(|dt| Self::system_offset(dt).local_minus_utc() as i64 * 1000)
                .unwrap_or(0),
        }
    }

    /// Converts a UTC `DateTime` into the user's configured timezone and normalizes it to
    /// `DateTime<FixedOffset>` so downstream formatting can use one concrete type.
    fn with_user_timezone<T>(
        &self,
        datetime: DateTime<chrono::Utc>,
        formatter: impl FnOnce(DateTime<chrono::FixedOffset>) -> T,
    ) -> T {
        let time_with_zone = match self {
            UserTimezone::Local => datetime.with_timezone(&Self::system_offset(datetime)),
            UserTimezone::Utc => datetime.fixed_offset(),
        };

        formatter(time_with_zone)
    }

    /// Formats an already timezone-adjusted timestamp for axis labels.
    ///
    /// `timeframe` controls whether output is second-level (`MM:SS`) or minute-level (`HH:MM`).
    /// At exact midnight for non-sub-10s intervals, this returns the day-of-month (`D`) to
    /// emphasize date boundaries on the chart.
    fn format_by_timeframe(
        datetime: &DateTime<chrono::FixedOffset>,
        timeframe: exchange::Timeframe,
    ) -> String {
        let interval = timeframe.to_milliseconds();

        if interval < 10_000 {
            datetime.format("%M:%S").to_string()
        } else if datetime.format("%H:%M").to_string() == "00:00" {
            datetime.format("%-d").to_string()
        } else {
            datetime.format("%H:%M").to_string()
        }
    }
}

impl fmt::Display for UserTimezone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UserTimezone::Utc => write!(f, "UTC"),
            UserTimezone::Local => {
                let now = chrono::Utc::now();
                let local_offset = Self::system_offset(now).local_minus_utc();
                let hours = local_offset.div_euclid(3600);
                let minutes = (local_offset.rem_euclid(3600)) / 60;
                let sign = if local_offset < 0 { "-" } else { "+" };
                write!(f, "Local (UTC {sign}{:02}:{:02})", hours.abs(), minutes)
            }
        }
    }
}

impl<'de> Deserialize<'de> for UserTimezone {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let timezone_str = String::deserialize(deserializer)?;
        match timezone_str.to_lowercase().as_str() {
            "utc" => Ok(UserTimezone::Utc),
            "local" => Ok(UserTimezone::Local),
            _ => Err(serde::de::Error::custom("Invalid UserTimezone")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_variant_needs_no_system_zone() {
        assert_eq!(UserTimezone::Utc.offset_ms_at(1_700_000_000_000), 0);
        assert_eq!(
            UserTimezone::Utc
                .format_with_kind(0, TimeLabelKind::Custom("%Y-%m-%d %H:%M"))
                .as_deref(),
            Some("1970-01-01 00:00")
        );
        assert_eq!(UserTimezone::Utc.to_string(), "UTC");
    }

    #[test]
    fn local_display_shows_a_well_formed_offset() {
        // Format contract only ("Local (UTC ±HH:MM)"); the actual zone comes
        // from the OS and varies per machine.
        let label = UserTimezone::Local.to_string();
        assert!(label.starts_with("Local (UTC "), "{label}");
        assert!(label.ends_with(')'), "{label}");
        assert_eq!(label.len(), "Local (UTC +00:00)".len(), "{label}");
    }
}

impl Serialize for UserTimezone {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            UserTimezone::Utc => serializer.serialize_str("UTC"),
            UserTimezone::Local => serializer.serialize_str("Local"),
        }
    }
}
