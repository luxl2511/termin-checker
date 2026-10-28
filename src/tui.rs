use anyhow::Result;
use chrono::{Local, NaiveDate};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::*,
};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, stdout};

use crate::config::Config;
use crate::scraper::{Department, Location, Service, Session};
use crate::service;

const BASE_URL: &str = "https://tevis.krzn.de/tevisweb2000";

enum Step {
    Dashboard(Config),
    CheckingNow(Config),
    FetchingDepartments,
    SelectDepartment(Vec<Department>, ListState),
    FetchingServices(String, String),
    SelectService(String, String, Vec<Service>, ListState),
    FetchingLocations(String, String, String, String),
    SelectLocation(String, String, String, String, Vec<Location>, ListState),
    InputLatestDate(SetupState, String),
    InputNtfyTopic(SetupState, String),
    InputDeadline(SetupState, String),
    Confirm(SetupState),
    Done(DoneState),
    Error(String),
}

#[derive(Clone)]
enum DoneState {
    AskStartWatcher,
    WatcherStarted,
    WatcherFailed(String),
    ConfigSavedOnly,
}

#[derive(Clone)]
struct SetupState {
    dept_id: String,
    dept_name: String,
    service_id: String,
    service_name: String,
    location: Location,
    latest_date: Option<String>,
    ntfy_topic: String,
    deadline: String,
}

pub fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = run_app(&mut terminal);

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    // If config exists, start with dashboard; otherwise start setup
    let mut step = match Config::load() {
        Ok(cfg) => Step::Dashboard(cfg),
        Err(_) => Step::FetchingDepartments,
    };
    let mut status_msg = String::from("Lade Abteilungen...");
    let mut needs_fetch = matches!(step, Step::FetchingDepartments);
    let mut check_result_msg: Option<String> = None;

    // Session is created lazily — only when we actually need to scrape
    let mut session: Option<Session> = None;

    fn ensure_session(s: &mut Option<Session>) -> Result<&Session> {
        if s.is_none() {
            let sess = Session::new()?;
            sess.init_session()?;
            *s = Some(sess);
        }
        Ok(s.as_ref().unwrap())
    }

    loop {
        // Do fetching outside the draw closure
        if needs_fetch {
            needs_fetch = false;
            match &step {
                Step::FetchingDepartments => {
                    terminal.draw(|f| draw_loading(f, &status_msg))?;
                    let sess = ensure_session(&mut session)?;
                    match sess.fetch_departments() {
                        Ok(depts) if depts.is_empty() => {
                            step = Step::Error(
                                "Keine Abteilungen gefunden. Die Seite hat möglicherweise ein anderes Format.".into(),
                            );
                        }
                        Ok(depts) => {
                            let mut state = ListState::default();
                            state.select(Some(0));
                            step = Step::SelectDepartment(depts, state);
                        }
                        Err(e) => {
                            step = Step::Error(format!("Fehler beim Laden: {}", e));
                        }
                    }
                }
                Step::FetchingServices(dept_id, dept_name) => {
                    let dept_id = dept_id.clone();
                    let dept_name = dept_name.clone();
                    terminal.draw(|f| draw_loading(f, &status_msg))?;
                    let sess = ensure_session(&mut session)?;
                    match sess.select_department(&dept_id) {
                        Ok(services) if services.is_empty() => {
                            step = Step::Error(
                                "Keine Anliegen gefunden für diese Abteilung.".into(),
                            );
                        }
                        Ok(services) => {
                            let mut state = ListState::default();
                            state.select(Some(0));
                            step = Step::SelectService(dept_id, dept_name, services, state);
                        }
                        Err(e) => {
                            step = Step::Error(format!("Fehler beim Laden: {}", e));
                        }
                    }
                }
                Step::FetchingLocations(dept_id, dept_name, svc_id, svc_name) => {
                    let dept_id = dept_id.clone();
                    let dept_name = dept_name.clone();
                    let svc_id = svc_id.clone();
                    let svc_name = svc_name.clone();
                    terminal.draw(|f| draw_loading(f, &status_msg))?;
                    let sess = ensure_session(&mut session)?;
                    match sess.select_service(&svc_id) {
                        Ok(locs) if locs.is_empty() => {
                            step = Step::Error("Keine Standorte gefunden.".into());
                        }
                        Ok(locs) if locs.len() == 1 => {
                            let loc = locs.into_iter().next().unwrap();
                            let default_deadline = (Local::now().date_naive()
                                + chrono::Duration::weeks(2))
                            .format("%d.%m.%Y")
                            .to_string();
                            step = Step::InputLatestDate(
                                SetupState {
                                    dept_id,
                                    dept_name,
                                    service_id: svc_id,
                                    service_name: svc_name,
                                    location: loc,
                                    latest_date: None,
                                    ntfy_topic: String::new(),
                                    deadline: default_deadline,
                                },
                                String::new(),
                            );
                        }
                        Ok(locs) => {
                            let mut state = ListState::default();
                            state.select(Some(0));
                            step = Step::SelectLocation(
                                dept_id, dept_name, svc_id, svc_name, locs, state,
                            );
                        }
                        Err(e) => {
                            step = Step::Error(format!("Fehler beim Laden: {}", e));
                        }
                    }
                }
                Step::CheckingNow(cfg) => {
                    let cfg = cfg.clone();
                    terminal.draw(|f| draw_loading(f, "Prüfe Termine..."))?;
                    let latest_date = cfg.latest_date_parsed();
                    let earliest_date = cfg.earliest_date_parsed();
                    let sess = Session::with_base_url(cfg.base_url.clone())?;
                    let result = sess.check(
                        &cfg.department_id,
                        &cfg.service_id,
                        &cfg.location_id,
                        &cfg.gps_lat,
                        &cfg.gps_long,
                        earliest_date.as_ref(),
                        latest_date.as_ref(),
                    );
                    check_result_msg = Some(match result {
                        Ok(crate::scraper::CheckResult::NoAppointments) => {
                            "Keine Termine verfügbar.".into()
                        }
                        Ok(crate::scraper::CheckResult::Appointments(slots)) => {
                            let details: Vec<String> = slots
                                .iter()
                                .map(|s| format!("  {} {}: {}", s.day_name, s.date, s.times.join(", ")))
                                .collect();
                            format!("TERMINE GEFUNDEN!\n{}", details.join("\n"))
                        }
                        Ok(crate::scraper::CheckResult::AllTooLate(slots)) => {
                            format!("Termine vorhanden, aber alle nach Wunschdatum ({} Slots).", slots.len())
                        }
                        Ok(crate::scraper::CheckResult::Error(msg)) => {
                            format!("Fehler: {}", msg)
                        }
                        Err(e) => format!("Fehler: {}", e),
                    });
                    step = Step::Dashboard(cfg);
                }
                _ => {}
            }
        }

        terminal.draw(|f| draw_step(f, &step, check_result_msg.as_deref()))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Global quit
            if key.code == KeyCode::Esc {
                return Ok(());
            }

            match &mut step {
                Step::Dashboard(cfg) => match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('n') => {
                        // New setup — reset session
                        session = None;
                        status_msg = "Lade Abteilungen...".into();
                        step = Step::FetchingDepartments;
                        needs_fetch = true;
                    }
                    KeyCode::Char('s') => {
                        match service::install() {
                            Ok(()) => {
                                check_result_msg = Some("Watcher gestartet!".into());
                                step = Step::Dashboard(cfg.clone());
                            }
                            Err(e) => {
                                check_result_msg = Some(format!("Watcher-Start fehlgeschlagen: {}", e));
                                step = Step::Dashboard(cfg.clone());
                            }
                        }
                    }
                    KeyCode::Char('x') => {
                        match service::uninstall() {
                            Ok(()) => {
                                check_result_msg = Some("Watcher gestoppt.".into());
                                step = Step::Dashboard(cfg.clone());
                            }
                            Err(e) => {
                                check_result_msg = Some(format!("Stoppen fehlgeschlagen: {}", e));
                                step = Step::Dashboard(cfg.clone());
                            }
                        }
                    }
                    KeyCode::Char('c') => {
                        check_result_msg = None;
                        step = Step::CheckingNow(cfg.clone());
                        needs_fetch = true;
                    }
                    _ => {}
                },
                Step::SelectDepartment(depts, state) => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        let i = state.selected().unwrap_or(0);
                        state.select(Some(if i == 0 { depts.len() - 1 } else { i - 1 }));
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let i = state.selected().unwrap_or(0);
                        state.select(Some((i + 1) % depts.len()));
                    }
                    KeyCode::Enter => {
                        if let Some(i) = state.selected() {
                            let dept = &depts[i];
                            let dept_id = dept.id.clone();
                            status_msg = format!("Lade Anliegen für {}...", dept.name);
                            step = Step::FetchingServices(dept_id, dept.name.clone());
                            needs_fetch = true;
                        }
                    }
                    _ => {}
                },
                Step::SelectService(dept_id, dept_name, services, state) => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        let i = state.selected().unwrap_or(0);
                        state.select(Some(if i == 0 {
                            services.len() - 1
                        } else {
                            i - 1
                        }));
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let i = state.selected().unwrap_or(0);
                        state.select(Some((i + 1) % services.len()));
                    }
                    KeyCode::Enter => {
                        if let Some(i) = state.selected() {
                            let svc = &services[i];
                            let svc_id = svc.id.clone();
                            let svc_name = svc.name.clone();
                            status_msg = format!("Lade Standorte für {}...", svc.name);
                            step = Step::FetchingLocations(
                                dept_id.clone(),
                                dept_name.clone(),
                                svc_id,
                                svc_name,
                            );
                            needs_fetch = true;
                        }
                    }
                    _ => {}
                },
                Step::SelectLocation(dept_id, dept_name, svc_id, svc_name, locs, state) => {
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            let i = state.selected().unwrap_or(0);
                            state.select(Some(if i == 0 { locs.len() - 1 } else { i - 1 }));
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let i = state.selected().unwrap_or(0);
                            state.select(Some((i + 1) % locs.len()));
                        }
                        KeyCode::Enter => {
                            if let Some(i) = state.selected() {
                                let loc = locs[i].clone();
                                let default_deadline = (Local::now().date_naive()
                                    + chrono::Duration::weeks(2))
                                .format("%d.%m.%Y")
                                .to_string();
                                step = Step::InputLatestDate(
                                    SetupState {
                                        dept_id: dept_id.clone(),
                                        dept_name: dept_name.clone(),
                                        service_id: svc_id.clone(),
                                        service_name: svc_name.clone(),
                                        location: loc,
                                        latest_date: None,
                                        ntfy_topic: String::new(),
                                        deadline: default_deadline,
                                    },
                                    String::new(),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                Step::InputLatestDate(state, input) => match key.code {
                    KeyCode::Char(c) => {
                        input.push(c);
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Enter => {
                        let trimmed = input.trim().to_string();
                        if trimmed.is_empty() || trimmed.to_lowercase() == "any" {
                            state.latest_date = None;
                        } else if NaiveDate::parse_from_str(&trimmed, "%d.%m.%Y").is_ok() {
                            state.latest_date = Some(trimmed);
                        } else {
                            continue;
                        }
                        let s = state.clone();
                        step = Step::InputNtfyTopic(s, String::new());
                    }
                    _ => {}
                },
                Step::InputNtfyTopic(state, input) => match key.code {
                    KeyCode::Char(c) => {
                        input.push(c);
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Enter => {
                        let trimmed = input.trim().to_string();
                        if trimmed.is_empty() {
                            state.ntfy_topic = random_ntfy_topic();
                        } else {
                            state.ntfy_topic = trimmed;
                        }
                        let s = state.clone();
                        step = Step::InputDeadline(s, String::new());
                    }
                    _ => {}
                },
                Step::InputDeadline(state, input) => match key.code {
                    KeyCode::Char(c) => {
                        input.push(c);
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Enter => {
                        let trimmed = input.trim().to_string();
                        if !trimmed.is_empty() {
                            if NaiveDate::parse_from_str(&trimmed, "%d.%m.%Y").is_ok() {
                                state.deadline = trimmed;
                            } else {
                                continue;
                            }
                        }
                        let s = state.clone();
                        step = Step::Confirm(s);
                    }
                    _ => {}
                },
                Step::Confirm(state) => match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => {
                        let s = state.clone();

                        let config = Config {
                            provider: "herne".to_string(),
                            base_url: BASE_URL.to_string(),
                            department_id: s.dept_id.clone(),
                            department_name: s.dept_name.clone(),
                            service_id: s.service_id.clone(),
                            service_name: s.service_name.clone(),
                            location_id: s.location.id.clone(),
                            location_name: s.location.name.clone(),
                            gps_lat: s.location.gps_lat.clone(),
                            gps_long: s.location.gps_long.clone(),
                            latest_date: s.latest_date.clone(),
                            earliest_date: None,
                            ntfy_topic: s.ntfy_topic.clone(),
                            deadline: s.deadline.clone(),
                            calendar_path: None,
                            calendar_uid: None,
                            service_uid: None,
                        };

                        match config.save() {
                            Ok(()) => {
                                step = Step::Done(DoneState::AskStartWatcher);
                            }
                            Err(e) => {
                                step = Step::Error(format!("Fehler beim Speichern: {}", e));
                            }
                        }
                    }
                    KeyCode::Char('n') => {
                        return Ok(());
                    }
                    _ => {}
                },
                Step::Done(done_state) => match key.code {
                    KeyCode::Char('y') => {
                        if matches!(done_state, DoneState::AskStartWatcher) {
                            match service::install() {
                                Ok(()) => {
                                    *done_state = DoneState::WatcherStarted;
                                }
                                Err(e) => {
                                    *done_state = DoneState::WatcherFailed(e.to_string());
                                }
                            }
                        }
                    }
                    KeyCode::Char('n') => {
                        if matches!(done_state, DoneState::AskStartWatcher) {
                            *done_state = DoneState::ConfigSavedOnly;
                        }
                    }
                    KeyCode::Char('q') | KeyCode::Enter => {
                        if !matches!(done_state, DoneState::AskStartWatcher) {
                            return Ok(());
                        }
                    }
                    _ => {}
                },
                Step::Error(_) => match key.code {
                    KeyCode::Char('q') | KeyCode::Enter => {
                        return Ok(());
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

fn draw_step(f: &mut Frame, step: &Step, check_result: Option<&str>) {
    let area = f.area();

    // Header
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let title_text = match step {
        Step::Dashboard(_) | Step::CheckingNow(_) => " Termin-Checker ",
        _ => " Termin-Checker Setup ",
    };

    let title = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title_text);
    f.render_widget(title, chunks[0]);

    let content_area = chunks[1];

    match step {
        Step::Dashboard(cfg) => {
            draw_dashboard(f, content_area, cfg, check_result);
        }
        Step::FetchingDepartments | Step::FetchingServices(..) | Step::FetchingLocations(..) | Step::CheckingNow(_) => {
            draw_loading(f, "Lade...");
        }
        Step::SelectDepartment(depts, state) => {
            draw_list(
                f,
                content_area,
                "Schritt 1/6: Abteilung wählen",
                &depts.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
                state,
                "↑↓ navigieren  Enter bestätigen  Esc abbrechen",
            );
        }
        Step::SelectService(_, _, services, state) => {
            draw_list(
                f,
                content_area,
                "Schritt 2/6: Anliegen wählen",
                &services.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                state,
                "↑↓ navigieren  Enter bestätigen  Esc abbrechen",
            );
        }
        Step::SelectLocation(_, _, _, _, locs, state) => {
            let items: Vec<String> = locs
                .iter()
                .map(|l| {
                    if l.address.is_empty() {
                        l.name.clone()
                    } else {
                        format!("{} — {}", l.name, l.address)
                    }
                })
                .collect();
            draw_list(
                f,
                content_area,
                "Schritt 3/6: Standort wählen",
                &items.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                state,
                "↑↓ navigieren  Enter bestätigen  Esc abbrechen",
            );
        }
        Step::InputLatestDate(_, input) => {
            draw_input(
                f,
                content_area,
                "Schritt 4/6: Spätester Termin",
                "Nur Termine bis zu diesem Datum anzeigen.\nFormat: DD.MM.YYYY, oder leer lassen für beliebig.",
                input,
            );
        }
        Step::InputNtfyTopic(_, input) => {
            draw_input(
                f,
                content_area,
                "Schritt 5/6: ntfy.sh Push-Benachrichtigungen",
                "Topic-Name für ntfy.sh Benachrichtigungen auf dem Handy.\nEnter für ein zufälliges Topic.",
                input,
            );
        }
        Step::InputDeadline(state, input) => {
            draw_input(
                f,
                content_area,
                "Schritt 6/6: Deadline",
                &format!(
                    "Watcher stoppt automatisch nach diesem Datum.\nFormat: DD.MM.YYYY, Enter für Standard: {}",
                    state.deadline
                ),
                input,
            );
        }
        Step::Confirm(state) => {
            draw_confirm(f, content_area, state);
        }
        Step::Done(done_state) => {
            draw_done(f, content_area, done_state);
        }
        Step::Error(msg) => {
            let p = Paragraph::new(msg.as_str())
                .style(Style::default().fg(Color::Red))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Fehler "),
                )
                .wrap(Wrap { trim: true });
            f.render_widget(p, content_area);
        }
    }
}

fn draw_dashboard(f: &mut Frame, area: Rect, cfg: &Config, check_result: Option<&str>) {
    let is_running = service::is_running();
    let is_installed = service::is_installed();

    // Split into: config, status, log/check result, actions
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),  // config
            Constraint::Length(5),  // status
            Constraint::Min(5),    // log / check result
            Constraint::Length(3), // actions
        ])
        .split(area);

    // Config section
    let config_text = vec![
        Line::from(vec![
            Span::styled("  Abteilung:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(&cfg.department_name),
        ]),
        Line::from(vec![
            Span::styled("  Anliegen:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(&cfg.service_name),
        ]),
        Line::from(vec![
            Span::styled("  Standort:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(&cfg.location_name),
        ]),
        Line::from(vec![
            Span::styled("  Spätestens: ", Style::default().fg(Color::DarkGray)),
            Span::raw(cfg.latest_date.as_deref().unwrap_or("beliebig")),
        ]),
        Line::from(vec![
            Span::styled("  ntfy Topic: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&cfg.ntfy_topic),
        ]),
        Line::from(vec![
            Span::styled("  Deadline:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(&cfg.deadline),
        ]),
    ];
    let config_block = Paragraph::new(config_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Konfiguration "),
        );
    f.render_widget(config_block, chunks[0]);

    // Status section
    let (status_text, status_color) = if is_running {
        ("AKTIV — Watcher prüft regelmäßig auf Termine", Color::Green)
    } else if is_installed {
        ("INSTALLIERT — Watcher ist installiert aber nicht aktiv", Color::Yellow)
    } else {
        ("INAKTIV — Kein Watcher läuft", Color::Red)
    };

    let status_lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", status_text),
            Style::default().fg(status_color).add_modifier(Modifier::BOLD),
        )),
    ];
    let status_block = Paragraph::new(status_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(status_color))
                .title(" Watcher Status "),
        );
    f.render_widget(status_block, chunks[1]);

    // Log / check result section
    let log_content = if let Some(result) = check_result {
        let mut lines: Vec<Line> = result
            .lines()
            .map(|l| {
                let color = if l.contains("GEFUNDEN") {
                    Color::Green
                } else if l.starts_with("Fehler") {
                    Color::Red
                } else {
                    Color::White
                };
                Line::from(Span::styled(format!("  {}", l), Style::default().fg(color)))
            })
            .collect();
        lines.insert(0, Line::from(""));
        lines
    } else {
        get_log_lines()
    };

    let log_title = if check_result.is_some() {
        " Ergebnis "
    } else {
        " Letzte Log-Einträge "
    };
    let log_block = Paragraph::new(log_content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(log_title),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(log_block, chunks[2]);

    // Actions bar
    let actions = if is_running {
        vec![
            Span::styled(" c ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(" Jetzt prüfen  "),
            Span::styled(" x ", Style::default().fg(Color::Black).bg(Color::Red)),
            Span::raw(" Watcher stoppen  "),
            Span::styled(" n ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" Neu einrichten  "),
            Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::DarkGray)),
            Span::raw(" Beenden"),
        ]
    } else {
        vec![
            Span::styled(" s ", Style::default().fg(Color::Black).bg(Color::Green)),
            Span::raw(" Watcher starten  "),
            Span::styled(" c ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(" Jetzt prüfen  "),
            Span::styled(" n ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" Neu einrichten  "),
            Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::DarkGray)),
            Span::raw(" Beenden"),
        ]
    };
    let actions_line = Paragraph::new(Line::from(actions))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(actions_line, chunks[3]);
}

fn get_log_lines() -> Vec<Line<'static>> {
    let log_path = match Config::config_dir() {
        Ok(d) => d.join("watcher.log"),
        Err(_) => return vec![Line::from("  Keine Logs gefunden.")],
    };
    if !log_path.exists() {
        return vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Noch keine Logs. Starte den Watcher, um Logs zu sehen.",
                Style::default().fg(Color::DarkGray),
            )),
        ];
    }
    match std::fs::read_to_string(&log_path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(8);
            let mut result = vec![Line::from("")];
            for line in &lines[start..] {
                let color = if line.contains("TERMINE") || line.contains("APPOINTMENTS") {
                    Color::Green
                } else if line.contains("ERROR") || line.contains("Fehler") {
                    Color::Red
                } else {
                    Color::DarkGray
                };
                result.push(Line::from(Span::styled(
                    format!("  {}", line),
                    Style::default().fg(color),
                )));
            }
            result
        }
        Err(_) => vec![Line::from("  Logs nicht lesbar.")],
    }
}

fn draw_loading(f: &mut Frame, msg: &str) {
    let area = f.area();
    let p = Paragraph::new(msg)
        .style(Style::default().fg(Color::Yellow))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn draw_list(f: &mut Frame, area: Rect, title: &str, items: &[&str], state: &ListState, hint: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|i| ListItem::new(*i))
        .collect();

    let list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", title)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = state.clone();
    f.render_stateful_widget(list, chunks[0], &mut state);

    let hint_p = Paragraph::new(hint)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint_p, chunks[1]);
}

fn draw_input(f: &mut Frame, area: Rect, title: &str, description: &str, input: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let desc_p = Paragraph::new(description)
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", title)),
        );
    f.render_widget(desc_p, chunks[0]);

    let input_p = Paragraph::new(format!("▶ {}_", input))
        .style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(input_p, chunks[1]);

    let hint_p = Paragraph::new("Enter bestätigen  Esc abbrechen")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint_p, chunks[3]);
}

fn draw_confirm(f: &mut Frame, area: Rect, state: &SetupState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(5)])
        .split(area);

    let text = vec![
        Line::from(vec![
            Span::styled("  Abteilung:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.dept_name),
        ]),
        Line::from(vec![
            Span::styled("  Anliegen:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.service_name),
        ]),
        Line::from(vec![
            Span::styled("  Standort:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.location.name),
        ]),
        Line::from(vec![
            Span::styled("  Spätestens: ", Style::default().fg(Color::DarkGray)),
            Span::raw(state.latest_date.as_deref().unwrap_or("beliebig")),
        ]),
        Line::from(vec![
            Span::styled("  ntfy Topic: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.ntfy_topic),
        ]),
        Line::from(vec![
            Span::styled("  Deadline:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.deadline),
        ]),
    ];

    let p = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green))
                .title(" Zusammenfassung "),
        );
    f.render_widget(p, chunks[0]);

    let info = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Nach dem Speichern wird der Watcher als Hintergrunddienst installiert.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  Er prüft automatisch auf neue Termine und benachrichtigt per ntfy.sh.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let actions = Paragraph::new(info)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" [y/Enter] Speichern  [n] Abbrechen "),
        );
    f.render_widget(actions, chunks[1]);
}

#[cfg(test)]
fn make_test_config() -> Config {
    Config {
        provider: "herne".into(),
        base_url: "https://example.com".into(),
        department_id: "dept1".into(),
        department_name: "Bürgerservice".into(),
        service_id: "svc1".into(),
        service_name: "Personalausweis".into(),
        location_id: "loc1".into(),
        location_name: "Rathaus Herne".into(),
        gps_lat: "51.5".into(),
        gps_long: "7.2".into(),
        latest_date: Some("15.04.2026".into()),
        earliest_date: None,
        ntfy_topic: "test-topic".into(),
        deadline: "01.05.2026".into(),
        calendar_path: None,
        calendar_uid: None,
        service_uid: None,
    }
}

#[cfg(test)]
fn make_test_setup_state() -> SetupState {
    SetupState {
        dept_id: "dept1".into(),
        dept_name: "Bürgerservice".into(),
        service_id: "svc1".into(),
        service_name: "Personalausweis".into(),
        location: Location {
            id: "loc1".into(),
            name: "Rathaus Herne".into(),
            address: "Berliner Platz 1".into(),
            gps_lat: "51.5".into(),
            gps_long: "7.2".into(),
        },
        latest_date: Some("15.04.2026".into()),
        ntfy_topic: "test-topic".into(),
        deadline: "01.05.2026".into(),
    }
}

fn draw_done(f: &mut Frame, area: Rect, done_state: &DoneState) {
    let (lines, color) = match done_state {
        DoneState::AskStartWatcher => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Konfiguration gespeichert!",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("  Der Watcher prüft im Hintergrund auf verfügbare Termine."),
                Line::from("  Bei Fund bekommst du eine Push-Benachrichtigung und"),
                Line::from("  die Buchungsseite wird automatisch geöffnet."),
                Line::from(""),
                Line::from("  Watcher jetzt starten?"),
                Line::from(""),
                Line::from(Span::styled(
                    "  [y] Ja, Watcher starten    [n] Nein, nur Config speichern",
                    Style::default().fg(Color::Cyan),
                )),
            ];
            (lines, Color::Green)
        }
        DoneState::WatcherStarted => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Watcher läuft!",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("  Der Watcher prüft nun regelmäßig auf Termine:"),
                Line::from("    06:30-09:00: alle 20 Sekunden"),
                Line::from("    Restlicher Tag: jede Minute"),
                Line::from(""),
                Line::from("  Nützliche Befehle:"),
                Line::from(Span::styled(
                    "    termin-checker         Dashboard mit Status anzeigen",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "    termin-checker check   Einzelne Prüfung durchführen",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "    termin-checker stop    Watcher stoppen",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Drücke q oder Enter zum Beenden.",
                    Style::default().fg(Color::Cyan),
                )),
            ];
            (lines, Color::Green)
        }
        DoneState::WatcherFailed(err) => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Konfiguration gespeichert!",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Watcher-Start fehlgeschlagen: {}", err),
                    Style::default().fg(Color::Red),
                )),
                Line::from(""),
                Line::from("  Du kannst den Watcher manuell starten:"),
                Line::from(Span::styled(
                    "    termin-checker watch",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Drücke q oder Enter zum Beenden.",
                    Style::default().fg(Color::Cyan),
                )),
            ];
            (lines, Color::Yellow)
        }
        DoneState::ConfigSavedOnly => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Konfiguration gespeichert!",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("  Watcher wurde nicht gestartet. Du kannst ihn jederzeit starten:"),
                Line::from(Span::styled(
                    "    termin-checker          Dashboard öffnen und dort starten",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "    termin-checker watch    Direkt im Vordergrund starten",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Drücke q oder Enter zum Beenden.",
                    Style::default().fg(Color::Cyan),
                )),
            ];
            (lines, Color::Green)
        }
    };

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(color))
                .title(" Fertig "),
        );
    f.render_widget(p, area);
}

/// Build a random ntfy topic name using the std hasher seed.
pub fn random_ntfy_topic() -> String {
    let n = std::collections::hash_map::RandomState::new().build_hasher().finish();
    format!("termin-checker-{:016x}", n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn random_ntfy_topics_differ() {
        let a = random_ntfy_topic();
        assert!(a.starts_with("termin-checker-") && a.len() == 31);
        assert_ne!(a, random_ntfy_topic());
    }

    fn render_to_string(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(draw).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut output = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                output.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            output.push('\n');
        }
        output
    }

    #[test]
    fn test_draw_loading() {
        let output = render_to_string(40, 5, |f| {
            draw_loading(f, "Lade Abteilungen...");
        });
        assert!(output.contains("Lade Abteilungen..."), "Loading message should appear: {output}");
    }

    #[test]
    fn test_draw_step_dashboard() {
        let cfg = make_test_config();
        let output = render_to_string(80, 30, |f| {
            draw_step(f, &Step::Dashboard(cfg), None);
        });
        assert!(output.contains("Termin-Checker"), "Title should appear");
        assert!(output.contains("Konfiguration"), "Config section should appear");
        assert!(output.contains("Personalausweis"), "Service name should appear");
        assert!(output.contains("Rathaus Herne"), "Location should appear");
        assert!(output.contains("test-topic"), "ntfy topic should appear");
    }

    #[test]
    fn test_draw_step_dashboard_with_check_result() {
        let cfg = make_test_config();
        let output = render_to_string(80, 30, |f| {
            draw_step(f, &Step::Dashboard(cfg), Some("Keine Termine verfügbar."));
        });
        assert!(output.contains("Ergebnis"), "Result section should appear");
        assert!(output.contains("Keine Termine"), "Check result text should appear");
    }

    #[test]
    fn test_draw_step_select_department() {
        let depts = vec![
            Department { id: "1".into(), name: "Bürgerservice".into() },
            Department { id: "2".into(), name: "Straßenverkehr".into() },
        ];
        let mut state = ListState::default();
        state.select(Some(0));
        let output = render_to_string(60, 15, |f| {
            draw_step(f, &Step::SelectDepartment(depts, state), None);
        });
        assert!(output.contains("Abteilung"), "Step title should mention Abteilung");
        assert!(output.contains("Straßenverkehr"), "Department names should appear: {output}");
    }

    #[test]
    fn test_draw_step_select_service() {
        let services = vec![
            Service { id: "1".into(), name: "Personalausweis".into() },
            Service { id: "2".into(), name: "Reisepass".into() },
        ];
        let mut state = ListState::default();
        state.select(Some(1));
        let output = render_to_string(60, 15, |f| {
            draw_step(
                f,
                &Step::SelectService("d1".into(), "Dept".into(), services, state),
                None,
            );
        });
        assert!(output.contains("Anliegen"), "Step title should mention Anliegen");
        assert!(output.contains("Reisepass"), "Service name should appear");
    }

    #[test]
    fn test_draw_step_select_location() {
        let locs = vec![
            Location {
                id: "1".into(),
                name: "Rathaus".into(),
                address: "Hauptstr. 1".into(),
                gps_lat: "51.5".into(),
                gps_long: "7.2".into(),
            },
        ];
        let mut state = ListState::default();
        state.select(Some(0));
        let output = render_to_string(60, 15, |f| {
            draw_step(
                f,
                &Step::SelectLocation(
                    "d1".into(), "Dept".into(), "s1".into(), "Svc".into(), locs, state,
                ),
                None,
            );
        });
        assert!(output.contains("Standort"), "Step title should mention Standort");
        assert!(output.contains("Rathaus"), "Location name should appear");
        assert!(output.contains("Hauptstr. 1"), "Address should appear");
    }

    #[test]
    fn test_draw_step_input_latest_date() {
        let state = make_test_setup_state();
        let output = render_to_string(60, 15, |f| {
            draw_step(f, &Step::InputLatestDate(state, "12.03".into()), None);
        });
        assert!(output.contains("Termin"), "Title should mention Termin");
        assert!(output.contains("12.03"), "Input text should appear");
    }

    #[test]
    fn test_draw_step_input_ntfy_topic() {
        let state = make_test_setup_state();
        let output = render_to_string(60, 15, |f| {
            draw_step(f, &Step::InputNtfyTopic(state, "my-topic".into()), None);
        });
        assert!(output.contains("ntfy"), "Title should mention ntfy");
        assert!(output.contains("my-topic"), "Input text should appear");
    }

    #[test]
    fn test_draw_step_input_deadline() {
        let state = make_test_setup_state();
        let output = render_to_string(60, 15, |f| {
            draw_step(f, &Step::InputDeadline(state, "".into()), None);
        });
        assert!(output.contains("Deadline"), "Title should mention Deadline");
    }

    #[test]
    fn test_draw_step_confirm() {
        let state = make_test_setup_state();
        let output = render_to_string(70, 20, |f| {
            draw_step(f, &Step::Confirm(state), None);
        });
        assert!(output.contains("Zusammenfassung"), "Should show confirmation title");
        assert!(output.contains("Personalausweis"), "Service name in summary");
        assert!(output.contains("Rathaus Herne"), "Location in summary");
        assert!(output.contains("test-topic"), "ntfy topic in summary");
        assert!(output.contains("Speichern"), "Should show save action");
    }

    #[test]
    fn test_draw_step_done_ask_watcher() {
        let output = render_to_string(70, 20, |f| {
            draw_step(f, &Step::Done(DoneState::AskStartWatcher), None);
        });
        assert!(output.contains("gespeichert"), "Should confirm config saved");
        assert!(output.contains("Watcher"), "Should mention watcher");
    }

    #[test]
    fn test_draw_step_done_watcher_started() {
        let output = render_to_string(70, 20, |f| {
            draw_step(f, &Step::Done(DoneState::WatcherStarted), None);
        });
        assert!(output.contains("Fertig"), "Should show done title");
    }

    #[test]
    fn test_draw_step_done_watcher_failed() {
        let output = render_to_string(70, 20, |f| {
            draw_step(
                f,
                &Step::Done(DoneState::WatcherFailed("permission denied".into())),
                None,
            );
        });
        assert!(output.contains("permission denied"), "Should show error message");
    }

    #[test]
    fn test_draw_step_done_config_saved_only() {
        let output = render_to_string(70, 20, |f| {
            draw_step(f, &Step::Done(DoneState::ConfigSavedOnly), None);
        });
        assert!(output.contains("gespeichert"), "Should confirm saved");
        assert!(output.contains("nicht gestartet"), "Should note watcher not started");
    }

    #[test]
    fn test_draw_step_error() {
        let output = render_to_string(60, 10, |f| {
            draw_step(f, &Step::Error("Verbindung fehlgeschlagen".into()), None);
        });
        assert!(output.contains("Fehler"), "Should show error title");
        assert!(output.contains("Verbindung fehlgeschlagen"), "Should show error message");
    }

    #[test]
    fn test_draw_step_loading_departments() {
        let output = render_to_string(60, 10, |f| {
            draw_step(f, &Step::FetchingDepartments, None);
        });
        assert!(output.contains("Lade"), "Should show loading indicator");
    }

    #[test]
    fn test_dashboard_shows_actions() {
        let cfg = make_test_config();
        let output = render_to_string(80, 30, |f| {
            draw_step(f, &Step::Dashboard(cfg), None);
        });
        assert!(output.contains("q"), "Should show quit action");
        assert!(output.contains("Beenden"), "Should show quit label");
    }

    #[test]
    fn test_location_without_address() {
        let locs = vec![
            Location {
                id: "1".into(),
                name: "Online".into(),
                address: "".into(),
                gps_lat: "".into(),
                gps_long: "".into(),
            },
        ];
        let mut state = ListState::default();
        state.select(Some(0));
        let output = render_to_string(60, 15, |f| {
            draw_step(
                f,
                &Step::SelectLocation(
                    "d1".into(), "D".into(), "s1".into(), "S".into(), locs, state,
                ),
                None,
            );
        });
        assert!(output.contains("Online"), "Location name should appear");
    }
}
