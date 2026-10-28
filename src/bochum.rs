use anyhow::{Context, Result};
use chrono::NaiveDate;
use regex::Regex;
use reqwest::blocking::Client;
use std::sync::Arc;

use crate::scraper::{AppointmentSlot, CheckResult};

const BASE: &str = "https://termine.bochum.de";
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub struct BochumSession {
    client: Client,
}

impl BochumSession {
    pub fn new() -> Result<Self> {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let client = Client::builder()
            .cookie_provider(jar)
            .user_agent(UA)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;
        Ok(Self { client })
    }

    /// Full check flow for Bochum booking system.
    ///
    /// 1. GET the calendar page — the server assigns a `wsid` via 302 redirect.
    /// 2. Call the JSON `/api/appointment/search` endpoint with that wsid.
    /// 3. Parse `items[*].dt` (ISO8601) and filter by `latest_date`.
    pub fn check(
        &self,
        calendar_path: &str,
        calendar_uid: &str,
        service_uid: &str,
        earliest_date: Option<&NaiveDate>,
        latest_date: Option<&NaiveDate>,
    ) -> Result<CheckResult> {
        let calendar_url = format!(
            "{}/m/{}/extern/calendar/?uid={}",
            BASE, calendar_path, calendar_uid
        );

        let resp = self
            .client
            .get(&calendar_url)
            .header("Accept", "text/html,application/xhtml+xml")
            .header("Accept-Language", "de-DE,de;q=0.9,en;q=0.8")
            .header("Referer", "https://www.bochum.de/")
            .send()
            .with_context(|| format!("GET {calendar_url} failed"))?;

        // After redirect the final URL carries the wsid. Fall back to scanning the body.
        let final_url = resp.url().clone();
        let body = resp.text().context("Failed to read calendar page body")?;

        let wsid = extract_wsid(final_url.as_str())
            .or_else(|| extract_wsid(&body))
            .ok_or_else(|| anyhow::anyhow!("Could not extract wsid from calendar page"))?;

        let api_url = format!(
            "{}/m/{}/api/appointment/search?services={}&search_mode=earliest&wsid={}",
            BASE, calendar_path, service_uid, wsid
        );

        let api_resp = self
            .client
            .get(&api_url)
            .header("Accept", "application/json")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", &calendar_url)
            .send()
            .with_context(|| format!("GET {api_url} failed"))?;

        let status = api_resp.status();
        let json = api_resp.text().context("Failed to read API response body")?;

        if !status.is_success() {
            return Ok(CheckResult::Error(format!(
                "Bochum API HTTP {}: {}",
                status,
                truncate(&json, 200)
            )));
        }

        if json.contains("\"ACCESS_DENIED\"") {
            return Ok(CheckResult::Error("Bochum API access denied".into()));
        }

        if !json.contains("\"SUCCESS\"") {
            return Ok(CheckResult::Error(format!(
                "Unexpected API response: {}",
                truncate(&json, 200)
            )));
        }

        let slots = parse_slots(&json);

        if slots.is_empty() {
            return Ok(CheckResult::NoAppointments);
        }

        if earliest_date.is_some() || latest_date.is_some() {
            let matching: Vec<_> = slots
                .iter()
                .filter(|s| {
                    let Ok(d) = NaiveDate::parse_from_str(&s.date, "%d.%m.%Y") else {
                        return false;
                    };
                    earliest_date.map_or(true, |e| d >= *e)
                        && latest_date.map_or(true, |l| d <= *l)
                })
                .cloned()
                .collect();
            if matching.is_empty() {
                return Ok(CheckResult::AllTooLate(slots));
            }
            return Ok(CheckResult::Appointments(matching));
        }

        Ok(CheckResult::Appointments(slots))
    }
}

fn extract_wsid(s: &str) -> Option<String> {
    let re = Regex::new(r"wsid=([a-f0-9-]{36})").ok()?;
    re.captures(s).map(|c| c[1].to_string())
}

/// Parse the SmartCX earliest-appointment response into grouped `AppointmentSlot`s.
///
/// Input items look like `{"dt":"2026-05-07T10:00:00+02:00",...}`. We group times by
/// date and format the date as `DD.MM.YYYY` to match the Herne pipeline.
fn parse_slots(json: &str) -> Vec<AppointmentSlot> {
    let dt_re = Regex::new(r#""dt"\s*:\s*"(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})"#).unwrap();
    let mut by_date: std::collections::BTreeMap<(i32, u32, u32), Vec<String>> =
        std::collections::BTreeMap::new();

    for cap in dt_re.captures_iter(json) {
        let year: i32 = cap[1].parse().unwrap_or(0);
        let month: u32 = cap[2].parse().unwrap_or(0);
        let day: u32 = cap[3].parse().unwrap_or(0);
        let time = format!("{}:{}", &cap[4], &cap[5]);
        by_date.entry((year, month, day)).or_default().push(time);
    }

    by_date
        .into_iter()
        .map(|((y, m, d), times)| {
            let date = format!("{:02}.{:02}.{:04}", d, m, y);
            let day_name = NaiveDate::from_ymd_opt(y, m, d)
                .map(|nd| german_day_name(nd))
                .unwrap_or_default();
            AppointmentSlot {
                date,
                day_name,
                times,
            }
        })
        .collect()
}

fn german_day_name(date: NaiveDate) -> String {
    use chrono::Datelike;
    match date.weekday() {
        chrono::Weekday::Mon => "Montag",
        chrono::Weekday::Tue => "Dienstag",
        chrono::Weekday::Wed => "Mittwoch",
        chrono::Weekday::Thu => "Donnerstag",
        chrono::Weekday::Fri => "Freitag",
        chrono::Weekday::Sat => "Samstag",
        chrono::Weekday::Sun => "Sonntag",
    }
    .to_string()
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_success_with_three_slots() {
        let json = r#"{
            "status":"SUCCESS","items_count":3,
            "items":[
                {"dt":"2026-05-07T10:00:00+02:00"},
                {"dt":"2026-05-11T11:15:00+02:00"},
                {"dt":"2026-04-24T08:30:00+02:00"}
            ]
        }"#;
        let slots = parse_slots(json);
        assert_eq!(slots.len(), 3);
        // Sorted by date ascending
        assert_eq!(slots[0].date, "24.04.2026");
        assert_eq!(slots[0].times, vec!["08:30"]);
        assert_eq!(slots[0].day_name, "Freitag");
    }

    #[test]
    fn extracts_wsid_from_url() {
        let url = "https://termine.bochum.de/m/x/extern/calendar/?uid=abc&wsid=bffa082e-0640-40b4-ad60-4a98355850ff&lang=de";
        assert_eq!(
            extract_wsid(url),
            Some("bffa082e-0640-40b4-ad60-4a98355850ff".into())
        );
    }

    #[test]
    fn empty_items_yields_no_slots() {
        let json = r#"{"status":"SUCCESS","items_count":0,"items":[]}"#;
        assert_eq!(parse_slots(json).len(), 0);
    }

    fn simulate_filter(
        slots: Vec<AppointmentSlot>,
        earliest: Option<NaiveDate>,
        latest: Option<NaiveDate>,
    ) -> CheckResult {
        if slots.is_empty() {
            return CheckResult::NoAppointments;
        }
        if earliest.is_some() || latest.is_some() {
            let matching: Vec<_> = slots
                .iter()
                .filter(|s| {
                    let Ok(d) = NaiveDate::parse_from_str(&s.date, "%d.%m.%Y") else {
                        return false;
                    };
                    earliest.map_or(true, |e| d >= e) && latest.map_or(true, |l| d <= l)
                })
                .cloned()
                .collect();
            if matching.is_empty() {
                return CheckResult::AllTooLate(slots);
            }
            return CheckResult::Appointments(matching);
        }
        CheckResult::Appointments(slots)
    }

    #[test]
    fn friday_only_filter_excludes_thursday() {
        let json = r#"{"items":[
            {"dt":"2026-04-23T12:45:00+02:00"},
            {"dt":"2026-04-24T08:20:00+02:00"},
            {"dt":"2026-05-07T10:00:00+02:00"}
        ]}"#;
        let slots = parse_slots(json);
        let friday = NaiveDate::from_ymd_opt(2026, 4, 24).unwrap();
        match simulate_filter(slots, Some(friday), Some(friday)) {
            CheckResult::Appointments(m) => {
                assert_eq!(m.len(), 1);
                assert_eq!(m[0].date, "24.04.2026");
                assert_eq!(m[0].day_name, "Freitag");
                assert_eq!(m[0].times, vec!["08:20"]);
            }
            other => panic!("expected Appointments, got {:?}", other),
        }
    }

    #[test]
    fn friday_only_filter_skips_when_only_thursday_available() {
        let json = r#"{"items":[
            {"dt":"2026-04-23T12:45:00+02:00"},
            {"dt":"2026-05-07T10:00:00+02:00"}
        ]}"#;
        let slots = parse_slots(json);
        let friday = NaiveDate::from_ymd_opt(2026, 4, 24).unwrap();
        match simulate_filter(slots, Some(friday), Some(friday)) {
            CheckResult::AllTooLate(_) => {}
            other => panic!("expected AllTooLate, got {:?}", other),
        }
    }
}
