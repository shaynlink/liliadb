//! Proleptic Gregorian calendar arithmetic on day counts since 1970-01-01 (Howard Hinnant's
//! `civil_from_days` and `days_from_civil`). Shared by temporal sharding, rule templates and
//! JSON expressions.

/// Milliseconds in a UTC day.
pub const MS_PER_DAY: i64 = 86_400_000;

/// A date of the proleptic Gregorian calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CivilDate {
    pub year: i64,
    /// 1 to 12.
    pub month: u32,
    /// 1 to 31.
    pub day: u32,
}

impl CivilDate {
    /// The date when `month` and `day` exist in `year`.
    #[must_use]
    pub fn new(year: i64, month: u32, day: u32) -> Option<Self> {
        ((1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day))
            .then_some(Self { year, month, day })
    }

    /// Civil date of the day `days` after 1970-01-01 (before it when negative).
    #[must_use]
    pub fn from_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let day_of_era = z.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        Self {
            year: year_of_era + era * 400 + i64::from(month <= 2),
            month: bounded(month),
            day: bounded(day),
        }
    }

    /// Number of days from 1970-01-01 to this date.
    #[must_use]
    pub fn to_days(self) -> i64 {
        let year = self.year - i64::from(self.month <= 2);
        let era = year.div_euclid(400);
        let year_of_era = year.rem_euclid(400);
        let month = i64::from(self.month);
        let shifted_month = if month > 2 { month - 3 } else { month + 9 };
        let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(self.day) - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }

    /// 1-based position of this date in its year.
    #[must_use]
    pub fn day_of_year(self) -> u32 {
        let january_first = Self {
            year: self.year,
            month: 1,
            day: 1,
        };
        bounded(self.to_days() - january_first.to_days() + 1)
    }

    /// Day of the week, 0 for Sunday to 6 for Saturday.
    #[must_use]
    pub fn day_of_week(self) -> u32 {
        // 1970-01-01 was a Thursday.
        bounded((self.to_days() + 4).rem_euclid(7))
    }
}

/// Whether `year` has a 29 February.
#[must_use]
pub fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Number of days in `month` (1 to 12) of `year`; 0 for any other month.
#[must_use]
pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Months, days and day indexes computed above always fit a `u32`.
fn bounded(value: i64) -> u32 {
    u32::try_from(value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_day_round_trips_and_follows_the_previous_one() {
        let mut previous = CivilDate::from_days(-800_000);
        for days in -799_999..800_000 {
            let date = CivilDate::from_days(days);
            assert_eq!(date.to_days(), days);
            let expected = if previous.day < days_in_month(previous.year, previous.month) {
                CivilDate {
                    day: previous.day + 1,
                    ..previous
                }
            } else if previous.month < 12 {
                CivilDate {
                    month: previous.month + 1,
                    day: 1,
                    ..previous
                }
            } else {
                CivilDate {
                    year: previous.year + 1,
                    month: 1,
                    day: 1,
                }
            };
            assert_eq!(date, expected, "day {days}");
            previous = date;
        }
    }

    #[test]
    fn known_dates() {
        assert_eq!(
            CivilDate::from_days(0),
            CivilDate {
                year: 1970,
                month: 1,
                day: 1
            }
        );
        assert_eq!(
            CivilDate::from_days(9),
            CivilDate {
                year: 1970,
                month: 1,
                day: 10
            }
        );
        assert_eq!(
            CivilDate::from_days(-1),
            CivilDate {
                year: 1969,
                month: 12,
                day: 31
            }
        );
        let leap_day = CivilDate::new(2024, 2, 29).unwrap();
        assert_eq!(leap_day.day_of_year(), 60);
        // 2026-11-17 is a Tuesday.
        assert_eq!(CivilDate::new(2026, 11, 17).unwrap().day_of_week(), 2);
        assert!(CivilDate::new(2023, 2, 29).is_none());
        assert!(CivilDate::new(2026, 13, 1).is_none());
    }
}
