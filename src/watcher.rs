use anyhow::Result;
use chrono::{Local, Timelike};
use std::thread;
use std::time::{Duration, Instant};

use crate::bochum::BochumSession;
use crate::config::Config;
use crate::notify;
use crate::scraper::{CheckResult, Session};

const NOTIFY_COOLDOWN: Duration = Duration::from_secs(600); // 10 min
const ERROR_COOLDOWN: Duration = Duration::from_secs(1800); // 30 min
const ERROR_THRESHOLD: u32 = 5;

pub fn run() -> Result<()> {
    let config = Config::load()?;
    let deadline = config.deadline_parsed();
    let latest_date = config.latest_date_parsed();
    let earliest_date = config.earliest_date_parsed();

    let mut last_notify = Instant::now() - NOTIFY_COOLDOWN;
    let mut last_error_notify = Instant::now() - ERROR_COOLDOWN;
    let mut consecutive_errors: u32 = 0;

    eprintln!(
        "[termin-checker] Provider: {} — Watching {} @ {}",
        config.provider, config.service_name, config.location_name
    );
    if let Some(d) = &config.latest_date {
        eprintln!("[termin-checker] Latest date: {}", d);
    }
    eprintln!("[termin-checker] Deadline: {}", config.deadline);
    eprintln!("[termin-checker] ntfy topic: {}", config.ntfy_topic);

    loop {
        // Check deadline
        if let Some(dl) = deadline {
            if Local::now().date_naive() > dl {
                eprintln!("[termin-checker] Deadline reached, stopping.");
                break;
            }
        }

        let result = run_check(&config, earliest_date.as_ref(), latest_date.as_ref());

        match result {
            Ok(CheckResult::NoAppointments) => {
                consecutive_errors = 0;
                let now = Local::now();
                eprintln!(
                    "[{}] Keine Termine verfügbar.",
                    now.format("%H:%M:%S")
                );
            }
            Ok(CheckResult::Appointments(slots)) => {
                consecutive_errors = 0;
                let slot_summary: String = slots
                    .iter()
                    .map(|s| format!("  {} {}: {}", s.day_name, s.date, s.times.join(", ")))
                    .collect::<Vec<_>>()
                    .join("\n");
                eprintln!(
                    "[{}] TERMINE GEFUNDEN!\n{}",
                    Local::now().format("%H:%M:%S"),
                    slot_summary
                );

                if last_notify.elapsed() >= NOTIFY_COOLDOWN {
                    last_notify = Instant::now();

                    let booking_url = booking_url(&config);

                    let _ = notify::send_ntfy(
                        &config.ntfy_topic,
                        &config.service_name,
                        &config.location_name,
                        &booking_url,
                        &slots,
                    );

                    let _ = notify::send_desktop_notification(
                        "Termin verfügbar!",
                        &config.location_name,
                        &format!(
                            "Termine für {} verfügbar!",
                            config.service_name
                        ),
                    );

                    let _ = notify::open_url(&booking_url);
                }
            }
            Ok(CheckResult::AllTooLate(slots)) => {
                consecutive_errors = 0;
                let dates: Vec<String> = slots.iter().map(|s| s.date.clone()).collect();
                eprintln!(
                    "[{}] Keine passenden Termine. Früheste Slots: {}",
                    Local::now().format("%H:%M:%S"),
                    dates.join(", ")
                );
            }
            Ok(CheckResult::Error(msg)) => {
                consecutive_errors += 1;
                eprintln!(
                    "[{}] Fehler: {} (#{} in Folge)",
                    Local::now().format("%H:%M:%S"),
                    msg,
                    consecutive_errors
                );
                maybe_notify_error(
                    &config.ntfy_topic,
                    &msg,
                    consecutive_errors,
                    &mut last_error_notify,
                );
            }
            Err(e) => {
                consecutive_errors += 1;
                eprintln!(
                    "[{}] Request-Fehler: {} (#{} in Folge)",
                    Local::now().format("%H:%M:%S"),
                    e,
                    consecutive_errors
                );
                maybe_notify_error(
                    &config.ntfy_topic,
                    &e.to_string(),
                    consecutive_errors,
                    &mut last_error_notify,
                );
            }
        }

        sleep_interval();
    }

    Ok(())
}

fn sleep_interval() {
    let now = Local::now();
    let minutes = now.hour() * 60 + now.minute();

    let interval = if (390..=540).contains(&minutes) {
        // 6:30 - 9:00: aggressive checking
        Duration::from_secs(20)
    } else {
        Duration::from_secs(30)
    };

    thread::sleep(interval);
}

pub fn run_check(
    config: &Config,
    earliest_date: Option<&chrono::NaiveDate>,
    latest_date: Option<&chrono::NaiveDate>,
) -> anyhow::Result<CheckResult> {
    match config.provider.as_str() {
        "bochum" => {
            let calendar_path = config
                .calendar_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("calendar_path missing"))?;
            let calendar_uid = config
                .calendar_uid
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("calendar_uid missing"))?;
            let service_uid = config
                .service_uid
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("service_uid missing"))?;
            let session = BochumSession::new()?;
            session.check(
                calendar_path,
                calendar_uid,
                service_uid,
                earliest_date,
                latest_date,
            )
        }
        _ => {
            let session = Session::with_base_url(config.base_url.clone())?;
            session.check(
                &config.department_id,
                &config.service_id,
                &config.location_id,
                &config.gps_lat,
                &config.gps_long,
                earliest_date,
                latest_date,
            )
        }
    }
}

fn booking_url(config: &Config) -> String {
    match config.provider.as_str() {
        "bochum" => format!(
            "https://termine.bochum.de/m/{}/extern/calendar/?uid={}",
            config.calendar_path.as_deref().unwrap_or("kfz-angelegenheiten"),
            config.calendar_uid.as_deref().unwrap_or(""),
        ),
        _ => format!("{}/", config.base_url),
    }
}

fn maybe_notify_error(
    topic: &str,
    message: &str,
    consecutive: u32,
    last_notify: &mut Instant,
) {
    if consecutive >= ERROR_THRESHOLD && last_notify.elapsed() >= ERROR_COOLDOWN {
        *last_notify = Instant::now();
        let _ = notify::send_ntfy_error(
            topic,
            &format!(
                "termin-checker: {} aufeinanderfolgende Fehler.\nLetzter Fehler: {}",
                consecutive, message
            ),
        );
    }
}
