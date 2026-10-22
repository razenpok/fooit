//! Date helpers: SQLAlchemy-compatible storage formats, due-date parsing
//! (a practical subset of `dateutil.parser.parse`) and recurrence strings.

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Weekday};
use regex::Regex;

const SQL_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.6f";

fn epoch() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(1970, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

/// SQLAlchemy stores `DateTime` on SQLite as `YYYY-MM-DD HH:MM:SS.ffffff`
pub fn datetime_to_sql(dt: NaiveDateTime) -> String {
    dt.format(SQL_FORMAT).to_string()
}

pub fn datetime_from_sql(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim();
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(value, fmt) {
            return Some(dt);
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
}

/// SQLAlchemy stores `Interval` on SQLite as a datetime relative to the unix epoch
pub fn interval_to_sql(td: TimeDelta) -> String {
    datetime_to_sql(epoch() + td)
}

pub fn interval_from_sql(value: &str) -> Option<TimeDelta> {
    datetime_from_sql(value).map(|dt| dt - epoch())
}

pub fn now() -> NaiveDateTime {
    Local::now().naive_local()
}

// ---------------------------------------------------------------------------
// Recurrence

pub fn parse_recurrence(value: &str) -> Result<TimeDelta, String> {
    let re = Regex::new(r"^(\d+)([mhdw])$").unwrap();
    let caps = re
        .captures(value)
        .ok_or_else(|| "Invalid recurrence format".to_string())?;
    let amount: i64 = caps[1]
        .parse()
        .map_err(|_| "Invalid recurrence format".to_string())?;

    let td = match &caps[2] {
        "m" => Duration::try_minutes(amount),
        "h" => Duration::try_hours(amount),
        "d" => Duration::try_days(amount),
        _ => Duration::try_weeks(amount),
    };
    td.ok_or_else(|| "Invalid recurrence format".to_string())
}

/// Port of `Recurrence.timedelta_to_simple_string`, using python's
/// `timedelta.days` / `timedelta.seconds` semantics
pub fn recurrence_to_string(td: TimeDelta) -> String {
    let total = td.num_seconds();
    let days = total.div_euclid(86400);
    let seconds = total.rem_euclid(86400);

    if days >= 7 && days % 7 == 0 {
        format!("{}w", days / 7)
    } else if days > 0 {
        format!("{}d", days)
    } else if seconds >= 3600 {
        format!("{}h", seconds / 3600)
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        "?".to_string()
    }
}

// ---------------------------------------------------------------------------
// Due date parsing

fn month_from_name(token: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = token.to_lowercase();
    if lower.len() < 3 {
        return None;
    }
    const FULL: [&str; 12] = [
        "january", "february", "march", "april", "may", "june", "july", "august", "september",
        "october", "november", "december",
    ];
    for (i, full) in FULL.iter().enumerate() {
        if full.starts_with(&lower) && lower.starts_with(MONTHS[i]) {
            return Some(i as u32 + 1);
        }
    }
    if lower == "sept" {
        return Some(9);
    }
    None
}

fn weekday_from_name(token: &str) -> Option<Weekday> {
    const DAYS: [(&str, Weekday); 7] = [
        ("monday", Weekday::Mon),
        ("tuesday", Weekday::Tue),
        ("wednesday", Weekday::Wed),
        ("thursday", Weekday::Thu),
        ("friday", Weekday::Fri),
        ("saturday", Weekday::Sat),
        ("sunday", Weekday::Sun),
    ];
    let lower = token.to_lowercase();
    if lower.len() < 3 {
        return None;
    }
    DAYS.iter()
        .find(|(name, _)| name.starts_with(&lower))
        .map(|(_, d)| *d)
}

/// Two digit years resolve to the century closest to the current year, like dateutil
fn expand_year(year: i32, digits: usize) -> i32 {
    if digits > 2 {
        return year;
    }
    let current = Local::now().year();
    let mut candidate = current / 100 * 100 + year;
    if candidate >= current + 50 {
        candidate -= 100;
    } else if candidate < current - 50 {
        candidate += 100;
    }
    candidate
}

fn parse_time(token: &str) -> Option<(u32, u32, u32)> {
    let re = Regex::new(r"^(\d{1,2}):(\d{2})(?::(\d{2})(?:\.\d+)?)?$").unwrap();
    let caps = re.captures(token)?;
    let h: u32 = caps[1].parse().ok()?;
    let m: u32 = caps[2].parse().ok()?;
    let s: u32 = caps.get(3).map(|s| s.as_str().parse().unwrap_or(0)).unwrap_or(0);
    Some((h, m, s))
}

#[derive(Default)]
struct Parsed {
    year: Option<i32>,
    month: Option<u32>,
    day: Option<u32>,
    time: Option<(u32, u32, u32)>,
    ampm: Option<bool>, // true = pm
    weekday: Option<Weekday>,
    relative_days: Option<i64>,
}

impl Parsed {
    fn set_date(&mut self, y: Option<i32>, m: u32, d: u32) -> Option<()> {
        if self.month.is_some() || self.day.is_some() {
            return None;
        }
        if let Some(y) = y {
            self.year = Some(y);
        }
        self.month = Some(m);
        self.day = Some(d);
        Some(())
    }

    fn handle_date_token(&mut self, token: &str) -> Option<()> {
        let parts: Vec<&str> = token.split(['-', '/', '.']).collect();
        if parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
            return None;
        }
        let nums: Vec<i64> = parts.iter().map(|p| p.parse().ok()).collect::<Option<_>>()?;

        match parts.len() {
            3 if parts[0].len() == 4 => self.set_date(Some(nums[0] as i32), nums[1] as u32, nums[2] as u32),
            3 => {
                let year = expand_year(nums[2] as i32, parts[2].len());
                // dateutil defaults to month first, but falls back when the month is impossible
                if nums[0] > 12 && nums[1] <= 12 {
                    self.set_date(Some(year), nums[1] as u32, nums[0] as u32)
                } else {
                    self.set_date(Some(year), nums[0] as u32, nums[1] as u32)
                }
            }
            2 if parts[0].len() == 4 => self.set_date(Some(nums[0] as i32), nums[1] as u32, 1),
            2 => {
                if nums[0] > 12 && nums[1] <= 12 {
                    self.set_date(None, nums[1] as u32, nums[0] as u32)
                } else {
                    self.set_date(None, nums[0] as u32, nums[1] as u32)
                }
            }
            _ => None,
        }
    }

    fn handle_token(&mut self, token: &str) -> Option<()> {
        let lower = token.to_lowercase();

        match lower.as_str() {
            "today" | "now" => {
                self.relative_days = Some(0);
                if lower == "now" {
                    let n = now();
                    self.time = Some((n.hour_(), n.minute_(), n.second_()));
                }
                return Some(());
            }
            "tomorrow" => {
                self.relative_days = Some(1);
                return Some(());
            }
            "yesterday" => {
                self.relative_days = Some(-1);
                return Some(());
            }
            "am" | "a.m." => {
                self.ampm = Some(false);
                return Some(());
            }
            "pm" | "p.m." => {
                self.ampm = Some(true);
                return Some(());
            }
            "at" | "on" | "of" | "the" | "," => return Some(()),
            _ => {}
        }

        // ISO style "2024-05-06T10:30"
        if let Some((date, time)) = token.split_once('T') {
            if !date.is_empty() && !time.is_empty() && date.chars().next()?.is_ascii_digit() {
                self.handle_date_token(date)?;
                self.time = Some(parse_time(time)?);
                return Some(());
            }
        }

        // "10:30", "10:30pm"
        let (core, suffix) = split_ampm(&lower);
        if core.contains(':') {
            self.time = Some(parse_time(core)?);
            if let Some(pm) = suffix {
                self.ampm = Some(pm);
            }
            return Some(());
        }

        // "10pm"
        if let Some(pm) = suffix {
            if !core.is_empty() && core.chars().all(|c| c.is_ascii_digit()) {
                self.time = Some((core.parse().ok()?, 0, 0));
                self.ampm = Some(pm);
                return Some(());
            }
        }

        if token.contains(['-', '/', '.']) {
            return self.handle_date_token(token);
        }

        // ordinals like "5th"
        let stripped = ["st", "nd", "rd", "th"]
            .iter()
            .find_map(|s| lower.strip_suffix(s))
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(&lower);

        if stripped.chars().all(|c| c.is_ascii_digit()) && !stripped.is_empty() {
            let n: i64 = stripped.parse().ok()?;
            return match stripped.len() {
                8 => {
                    // 20240506
                    self.set_date(
                        Some(stripped[0..4].parse().ok()?),
                        stripped[4..6].parse().ok()?,
                        stripped[6..8].parse().ok()?,
                    )
                }
                4 if self.year.is_none() => {
                    self.year = Some(n as i32);
                    Some(())
                }
                1 | 2 if self.day.is_none() => {
                    self.day = Some(n as u32);
                    Some(())
                }
                1 | 2 if self.year.is_none() => {
                    self.year = Some(expand_year(n as i32, 2));
                    Some(())
                }
                _ => None,
            };
        }

        if let Some(m) = month_from_name(&lower) {
            if self.month.is_some() {
                return None;
            }
            self.month = Some(m);
            return Some(());
        }

        if let Some(d) = weekday_from_name(&lower) {
            self.weekday = Some(d);
            return Some(());
        }

        None
    }

    fn build(self) -> Option<NaiveDateTime> {
        let today = Local::now().date_naive();
        let base = match self.relative_days {
            Some(days) => today + Duration::days(days),
            None => today,
        };

        let year = self.year.unwrap_or(base.year());
        let month = self.month.unwrap_or(base.month());
        let day = self.day.unwrap_or(base.day());
        let mut date = NaiveDate::from_ymd_opt(year, month, day)?;

        if let Some(weekday) = self.weekday {
            while date.weekday() != weekday {
                date += Duration::days(1);
            }
        }

        let (mut h, m, s) = self.time.unwrap_or((0, 0, 0));
        match self.ampm {
            Some(true) if h < 12 => h += 12,
            Some(false) if h == 12 => h = 0,
            Some(_) if h > 12 => return None,
            _ => {}
        }
        let time = NaiveTime::from_hms_opt(h, m, s)?;
        Some(date.and_time(time))
    }
}

fn split_ampm(token: &str) -> (&str, Option<bool>) {
    for (suffix, pm) in [("a.m.", false), ("p.m.", true), ("am", false), ("pm", true)] {
        if let Some(core) = token.strip_suffix(suffix) {
            return (core, Some(pm));
        }
    }
    (token, None)
}

trait TimeParts {
    fn hour_(&self) -> u32;
    fn minute_(&self) -> u32;
    fn second_(&self) -> u32;
}

impl TimeParts for NaiveDateTime {
    fn hour_(&self) -> u32 {
        chrono::Timelike::hour(self)
    }
    fn minute_(&self) -> u32 {
        chrono::Timelike::minute(self)
    }
    fn second_(&self) -> u32 {
        chrono::Timelike::second(self)
    }
}

/// Parse a free form due date. Missing parts default to today at midnight,
/// matching how dateutil fills in values.
pub fn parse_due(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let mut parsed = Parsed::default();
    let mut any = false;
    for token in value.split(|c: char| c.is_whitespace() || c == ',') {
        if token.is_empty() {
            continue;
        }
        parsed.handle_token(token)?;
        any = true;
    }

    if !any {
        return None;
    }
    parsed.build()
}

/// What a new todo's text asks for: an urgency as `!1` to `!4` anywhere, and a due date in its
/// last words, such as `tomorrow`, `fri 10:30`, `on oct 30` or `at 5pm`. The rest is the
/// description. Plain words that could be dates ("sun", "may") and lone numbers are left alone
pub fn quick_add(text: &str) -> (String, Option<NaiveDateTime>, Option<i64>) {
    let mut words: Vec<&str> = text.split_whitespace().collect();

    let is_urgency = |w: &str| matches!(w, "!1" | "!2" | "!3" | "!4");
    let urgency = words.iter().rev().find(|w| is_urgency(w)).map(|w| w[1..].parse().unwrap());
    words.retain(|w| !is_urgency(w));

    // the longest run of date words at the end that parses, leaving some description
    let mut due = None;
    for k in (1..words.len()).rev() {
        let phrase = &words[words.len() - k..];
        if !is_date_phrase(phrase) {
            continue;
        }
        if let Some(parsed) = parse_due(&phrase.join(" ")) {
            due = Some(parsed);
            words.truncate(words.len() - k);
            break;
        }
    }
    (words.join(" "), due, urgency)
}

fn is_date_phrase(words: &[&str]) -> bool {
    const DAYS: [&str; 10] = ["today", "tomorrow", "mon", "tue", "tues", "thu", "thur", "thurs", "fri", "now"];
    const FULL_DAYS: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];

    let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let is_number = |w: &str| {
        let digits = w.trim_end_matches(['s', 't', 'n', 'd', 'r', 'h']);
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
    };
    // a month only counts with a day: "oct 30", not "may"
    let month = lower.iter().any(|w| w.len() >= 3 && month_from_name(w).is_some())
        && lower.iter().any(|w| is_number(w));
    let mut anchored = false;
    for word in &lower {
        let number = is_number(word);
        let (core, ampm) = split_ampm(word);
        let time_or_date = word.chars().any(|c| c.is_ascii_digit())
            && (word.contains([':', '-', '/']) || (ampm.is_some() && core.chars().all(|c| c.is_ascii_digit())));
        if DAYS.contains(&word.as_str()) || FULL_DAYS.contains(&word.as_str()) || time_or_date || (month && word.len() >= 3 && month_from_name(word).is_some()) {
            anchored = true;
        } else if !(matches!(word.as_str(), "at" | "on") || (number && month)) {
            return false;
        }
    }
    anchored
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn quick_add_reads_due_dates_and_urgency() {
        let today = now().date();
        let (text, due, urgency) = quick_add("buy milk tomorrow !2");
        assert_eq!((text.as_str(), urgency), ("buy milk", Some(2)));
        assert_eq!(due.unwrap().date(), today + Duration::days(1));

        let (text, due, _) = quick_add("call mom on friday at 10:30");
        assert_eq!(text, "call mom");
        let due = due.unwrap();
        assert_eq!((due.weekday(), due.hour(), due.minute()), (Weekday::Fri, 10, 30));

        let (text, due, _) = quick_add("pay rent oct 30");
        assert_eq!(text, "pay rent");
        assert_eq!((due.unwrap().month(), due.unwrap().day()), (10, 30));

        let (text, due, _) = quick_add("standup at 5pm");
        assert_eq!((text.as_str(), due.unwrap().hour()), ("standup", 17));

        // words that only might be dates stay in the description
        for plain in ["read chapter 5", "fix the sun", "it may", "tomorrow", "plan today's meeting", "!5 stars"] {
            let (text, due, urgency) = quick_add(plain);
            assert_eq!((text.as_str(), due, urgency), (plain, None, None), "{plain}");
        }
    }

    #[test]
    fn sql_roundtrip() {
        let dt = NaiveDate::from_ymd_opt(2024, 5, 6).unwrap().and_hms_opt(7, 8, 0).unwrap();
        assert_eq!(datetime_to_sql(dt), "2024-05-06 07:08:00.000000");
        assert_eq!(datetime_from_sql("2024-05-06 07:08:00.000000"), Some(dt));
        assert_eq!(interval_to_sql(Duration::days(7)), "1970-01-08 00:00:00.000000");
        assert_eq!(interval_from_sql("1970-01-01 03:00:00.000000"), Some(Duration::hours(3)));
    }

    #[test]
    fn recurrence() {
        assert_eq!(parse_recurrence("2w").unwrap(), Duration::weeks(2));
        assert!(parse_recurrence("2x").is_err());
        assert!(parse_recurrence("w").is_err());
        assert_eq!(recurrence_to_string(Duration::weeks(2)), "2w");
        assert_eq!(recurrence_to_string(Duration::days(3)), "3d");
        assert_eq!(recurrence_to_string(Duration::hours(5)), "5h");
        assert_eq!(recurrence_to_string(Duration::minutes(5)), "5m");
        assert_eq!(recurrence_to_string(Duration::seconds(5)), "?");
    }

    #[test]
    fn due() {
        let d = parse_due("2024-05-06").unwrap();
        assert_eq!((d.year(), d.month(), d.day(), d.hour()), (2024, 5, 6, 0));
        let d = parse_due("2024-05-06 14:30").unwrap();
        assert_eq!((d.hour(), d.minute()), (14, 30));
        let d = parse_due("05/06/2024").unwrap();
        assert_eq!((d.month(), d.day()), (5, 6));
        let d = parse_due("Jan 5 2025 3pm").unwrap();
        assert_eq!((d.year(), d.month(), d.day(), d.hour()), (2025, 1, 5, 15));
        let d = parse_due("5 january").unwrap();
        assert_eq!((d.month(), d.day()), (1, 5));
        assert!(parse_due("garbage").is_none());
        assert!(parse_due("2024-13-40").is_none());
        let d = parse_due("10:30").unwrap();
        assert_eq!(d.date(), Local::now().date_naive());
        assert_eq!(parse_due("friday").unwrap().weekday(), Weekday::Fri);
    }
}
