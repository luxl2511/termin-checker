use anyhow::{Context, Result};
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::sync::Arc;

pub const BASE_URL: &str = "https://tevis.krzn.de/tevisweb2000";

pub struct Session {
    client: Client,
    base_url: String,
}

#[derive(Debug, Clone)]
pub struct Department {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Service {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Location {
    pub id: String,
    pub name: String,
    pub address: String,
    pub gps_lat: String,
    pub gps_long: String,
}

#[derive(Debug, Clone)]
pub struct AppointmentSlot {
    pub date: String,
    pub day_name: String,
    pub times: Vec<String>,
}

#[derive(Debug)]
pub enum CheckResult {
    NoAppointments,
    Appointments(Vec<AppointmentSlot>),
    AllTooLate(Vec<AppointmentSlot>),
    Error(String),
}

impl CheckResult {
    /// Describe the result as German text for the terminal and the web page.
    pub fn summary(&self) -> String {
        match self {
            CheckResult::NoAppointments => "Keine Termine verfügbar.".into(),
            CheckResult::Appointments(slots) => {
                let lines: Vec<String> = slots
                    .iter()
                    .map(|s| format!("  {} {}: {}", s.day_name, s.date, s.times.join(", ")))
                    .collect();
                format!("TERMINE GEFUNDEN!\n{}", lines.join("\n"))
            }
            CheckResult::AllTooLate(slots) => {
                let dates: Vec<&str> = slots.iter().map(|s| s.date.as_str()).collect();
                format!("Keine passenden Termine. Früheste Slots: {}", dates.join(", "))
            }
            CheckResult::Error(msg) => format!("Fehler: {}", msg),
        }
    }
}

impl Session {
    pub fn new() -> Result<Self> {
        Self::with_base_url(BASE_URL.to_string())
    }

    pub fn with_base_url(base_url: String) -> Result<Self> {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let client = Client::builder()
            .cookie_provider(jar)
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .build()?;
        Ok(Self { client, base_url })
    }

    fn get(&self, path: &str) -> Result<String> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url} failed"))?;
        resp.text().context("Failed to read response body")
    }

    fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<String> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .post(&url)
            .form(form)
            .send()
            .with_context(|| format!("POST {url} failed"))?;
        resp.text().context("Failed to read response body")
    }

    pub fn init_session(&self) -> Result<()> {
        self.get("/")?;
        Ok(())
    }

    pub fn fetch_departments(&self) -> Result<Vec<Department>> {
        let html = self.get("/")?;
        let doc = Html::parse_document(&html);
        let mut departments = Vec::new();

        // Actual HTML: <button onclick="window.location.href='select2?md=2'" name="Name">Name</button>
        let sel = Selector::parse("button.select_mdt_btn").unwrap();
        for el in doc.select(&sel) {
            if let Some(onclick) = el.value().attr("onclick") {
                if let Some(raw_id) = onclick.split("md=").nth(1) {
                    let id: String = raw_id.chars().take_while(|c| c.is_ascii_digit()).collect();
                    let name = el.text().collect::<String>().trim().to_string();
                    if !id.is_empty() && !name.is_empty() {
                        departments.push(Department { id, name });
                    }
                }
            }
        }

        Ok(departments)
    }

    pub fn select_department(&self, dept_id: &str) -> Result<Vec<Service>> {
        let html = self.get(&format!("/select2?md={}", dept_id))?;
        let doc = Html::parse_document(&html);

        let mut services = Vec::new();

        // Bash pattern: for="input-{ID}"[^>]*>[^<]*<b>Service Name</b>
        // Use label[for^='input-'] selector, extract ID from `for` attr, name from inner <b>
        let sel = Selector::parse("label[for^='input-']").unwrap();
        let b_sel = Selector::parse("b").unwrap();
        for el in doc.select(&sel) {
            if let Some(for_attr) = el.value().attr("for") {
                let id = for_attr.trim_start_matches("input-").to_string();
                let name = el
                    .select(&b_sel)
                    .next()
                    .map(|b| b.text().collect::<String>().trim().to_string())
                    .unwrap_or_default();
                if !id.is_empty() && !name.is_empty() {
                    services.push(Service { id, name });
                }
            }
        }

        Ok(services)
    }

    pub fn select_service(&self, service_id: &str) -> Result<Vec<Location>> {
        let html = self.get(&format!(
            "/location?select_cnc=1&cnc-{}=1",
            service_id
        ))?;
        parse_locations(&html)
    }

    pub fn fetch_suggestions(&self) -> Result<String> {
        self.get("/suggest")
    }

    /// Full check flow: init session, navigate to suggestions, parse results
    pub fn check(
        &self,
        dept_id: &str,
        service_id: &str,
        loc_id: &str,
        gps_lat: &str,
        gps_long: &str,
        earliest_date: Option<&chrono::NaiveDate>,
        latest_date: Option<&chrono::NaiveDate>,
    ) -> Result<CheckResult> {
        // Step 1: Init session
        self.get("/")?;

        // Step 2: Select department
        self.get(&format!("/select2?md={}", dept_id))?;

        // Step 3: Select service
        self.get(&format!(
            "/location?select_cnc=1&cnc-{}=1",
            service_id
        ))?;

        // Step 4: Select location
        self.post(
            &format!("/location?select_cnc=1&cnc-{}=1", service_id),
            &[
                ("loc", loc_id),
                ("gps_lat", gps_lat),
                ("gps_long", gps_long),
                ("select_location", "Weiter"),
            ],
        )?;

        // Step 5: Get suggestions
        let html = self.fetch_suggestions()?;

        // Validate we got the right page
        if html.contains("Fehlermeldung") || html.contains("exception_msg") {
            return Ok(CheckResult::Error("Session error detected".into()));
        }
        if !html.contains("Terminverwaltung") && !html.contains("tevisweb") {
            return Ok(CheckResult::Error("Unexpected page response".into()));
        }

        // Check for no appointments
        if html.contains("Keine Zeiten") || html.contains("keine Termine zur Verf") {
            return Ok(CheckResult::NoAppointments);
        }

        // Parse appointments
        let slots = parse_appointment_slots(&html);

        if slots.is_empty() {
            // Page loaded but no parseable slots - might be a different format
            if html.contains("suggestion_form") || html.contains("Auswahl der Zeit") {
                return Ok(CheckResult::Error(
                    "Appointments page detected but could not parse slots".into(),
                ));
            }
            return Ok(CheckResult::NoAppointments);
        }

        if earliest_date.is_some() || latest_date.is_some() {
            let matching: Vec<_> = slots
                .iter()
                .filter(|s| {
                    let Ok(d) = chrono::NaiveDate::parse_from_str(&s.date, "%d.%m.%Y") else {
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

fn parse_locations(html: &str) -> Result<Vec<Location>> {
    let doc = Html::parse_document(html);
    let mut locations = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    // Bash pattern: grep -A3 'name="loc"' to find radio inputs with loc IDs,
    // then gps_lat/gps_long hidden inputs nearby
    let sel = Selector::parse("input[name='loc']").unwrap();
    let lat_sel = Selector::parse("input[name='gps_lat']").unwrap();
    let long_sel = Selector::parse("input[name='gps_long']").unwrap();

    let gps_lat = doc
        .select(&lat_sel)
        .next()
        .and_then(|e| e.value().attr("value"))
        .unwrap_or("0")
        .to_string();
    let gps_long = doc
        .select(&long_sel)
        .next()
        .and_then(|e| e.value().attr("value"))
        .unwrap_or("0")
        .to_string();

    for el in doc.select(&sel) {
        if let Some(value) = el.value().attr("value") {
            let id = value.to_string();
            if !seen_ids.insert(id.clone()) {
                continue;
            }

            // Get name from map_marker_content-{id} div's <b> tag
            // Bash: grep -A5 "map_marker_content-${id}" | grep '<b>' | sed 's/.*<b>//;s/<\/b>.*//'
            let b_sel = Selector::parse("b").unwrap();
            let marker_sel_str = format!("[id='map_marker_content-{}']", id);
            let name = Selector::parse(&marker_sel_str)
                .ok()
                .and_then(|s| doc.select(&s).next())
                .and_then(|marker| marker.select(&b_sel).next())
                .map(|b| b.text().collect::<String>().trim().to_string())
                .unwrap_or_else(|| format!("Standort {}", id));

            // Try to extract address (postal code + city) from the marker content
            let address = Selector::parse(&marker_sel_str)
                .ok()
                .and_then(|s| doc.select(&s).next())
                .map(|marker| {
                    let text = marker.text().collect::<String>();
                    let re = Regex::new(r"\d{5}\s+[A-Za-zÄÖÜäöüß]+").unwrap();
                    re.find(&text)
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default()
                })
                .unwrap_or_default();

            locations.push(Location {
                id,
                name,
                address,
                gps_lat: gps_lat.clone(),
                gps_long: gps_long.clone(),
            });
        }
    }

    Ok(locations)
}

fn parse_appointment_slots(html: &str) -> Vec<AppointmentSlot> {
    let mut slots = Vec::new();

    // Mirrors the bash approach:
    // 1. Find all "(DayName), DD.MM.YYYY" occurrences with positions in raw HTML
    // 2. For each date section, find type="submit".*suggest_btn.*title="HH:MM" patterns
    let date_re = Regex::new(
        r"(Montag|Dienstag|Mittwoch|Donnerstag|Freitag|Samstag|Sonntag), (\d{2}\.\d{2}\.\d{4})",
    )
    .unwrap();
    let time_re = Regex::new(r#"type="submit"[^>]*suggest_btn[^>]*title="(\d{1,2}:\d{2})""#)
        .unwrap();

    // Collect all date matches with their positions
    let date_matches: Vec<_> = date_re
        .captures_iter(html)
        .filter_map(|cap| {
            let m = cap.get(0)?;
            Some((
                m.start(),
                cap[1].to_string(), // day name
                cap[2].to_string(), // date
            ))
        })
        .collect();

    for (i, (start, day_name, date)) in date_matches.iter().enumerate() {
        // Section: from this date to the next date (or end of HTML)
        let end = date_matches
            .get(i + 1)
            .map(|(s, _, _)| *s)
            .unwrap_or(html.len());
        let section = &html[*start..end];

        let times: Vec<String> = time_re
            .captures_iter(section)
            .map(|cap| cap[1].to_string())
            .collect();

        if !times.is_empty() {
            slots.push(AppointmentSlot {
                date: date.clone(),
                day_name: day_name.clone(),
                times,
            });
        }
    }

    slots
}

