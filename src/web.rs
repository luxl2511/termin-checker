use anyhow::{bail, Context, Result};
use chrono::{Local, NaiveDate};
use reqwest::Url;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use crate::config::Config;
use crate::scraper::{Session, BASE_URL};
use crate::{notify, service, tui, watcher};

const MAX_BODY: usize = 64 * 1024;

const STYLE: &str = r#"
:root{--bg:#f2f2f7;--card:#fff;--text:#000;--muted:#6c6c70;--sep:rgba(60,60,67,.2);--accent:#0066cc;--green:#248a3d;--dot-green:#34c759;
--orange:#ff9500;--red:#d70015;--fill:rgba(118,118,128,.12);color-scheme:light dark;
font-family:-apple-system,BlinkMacSystemFont,"SF Pro Text","Segoe UI",system-ui,sans-serif;font-size:17px;line-height:1.35;-webkit-font-smoothing:antialiased}
@media (prefers-color-scheme:dark){:root{--bg:#000;--card:#1c1c1e;--text:#fff;--muted:#98989f;--sep:rgba(84,84,88,.65);--accent:#0a84ff;
--green:#30d158;--dot-green:#30d158;--orange:#ff9f0a;--red:#ff453a;--fill:rgba(118,118,128,.24)}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text)}
main{max-width:36rem;margin:0 auto;padding:12px 16px 48px}
.back{display:inline-flex;align-items:center;min-height:44px;color:var(--accent);text-decoration:none}
h1{font-size:34px;font-weight:700;margin:12px 0 4px}
.lead{color:var(--muted);margin:0 0 8px}
.step{color:var(--muted);font-size:13px;text-transform:uppercase;margin:0 0 16px}
.section{font-size:13px;font-weight:400;color:var(--muted);text-transform:uppercase;margin:28px 16px 6px}
.group{background:var(--card);border-radius:12px;overflow:hidden}
.row{display:flex;align-items:center;justify-content:space-between;gap:12px;min-height:44px;padding:11px 16px;color:inherit;text-decoration:none;position:relative}
.row+.row::before{content:"";position:absolute;top:0;left:16px;right:0;border-top:1px solid var(--sep)}
a.row:active{background:var(--fill)}
.row small{display:block;color:var(--muted);font-size:15px}
.value{color:var(--muted);text-align:right;overflow-wrap:anywhere}
.value a{color:var(--accent);text-decoration:none}
.chev{color:var(--muted);font-size:22px;line-height:1;flex:none}
a.row.busy .chev{font-size:0;width:18px;height:18px;border:2px solid var(--sep);border-top-color:var(--muted);border-radius:50%;animation:spin .8s linear infinite}
@keyframes spin{to{transform:rotate(360deg)}}
.row input{flex:1;min-width:0;border:0;padding:0;background:transparent;font:inherit;color:var(--text);text-align:right;outline:none}
.row input::placeholder{color:var(--muted)}
.row:focus-within{background:var(--fill)}
.footnote{font-size:13px;color:var(--muted);margin:6px 16px 0}
.hero{background:var(--card);border-radius:12px;padding:28px 20px 24px;text-align:center;margin-top:16px}
.hero h2{font-size:22px;margin:14px 0 4px}
.hero p{margin:2px 0}
.dot{width:14px;height:14px;border-radius:50%;margin:0 auto;background:var(--muted)}
.dot.ok{background:var(--dot-green);animation:pulse 2s infinite}
.dot.warn{background:var(--orange)}
@keyframes pulse{0%{box-shadow:0 0 0 0 rgba(52,199,89,.5)}70%,100%{box-shadow:0 0 0 12px rgba(52,199,89,0)}}
@media (prefers-reduced-motion:reduce){.dot.ok,a.row.busy .chev{animation:none}}
.muted{color:var(--muted)}
.buttons{display:grid;gap:10px;margin-top:20px}
form{margin:0}
button{width:100%;min-height:50px;border:0;border-radius:12px;font:inherit;font-weight:600;background:var(--card);color:var(--accent);cursor:pointer}
button.primary{background:var(--accent);color:#fff}
button.danger{color:var(--red)}
button:disabled{opacity:.5;cursor:wait}
.banner{background:var(--card);border-radius:12px;padding:14px 16px;margin-top:16px;white-space:pre-line}
.banner.ok{color:var(--green);font-weight:600}
.banner.bad{color:var(--red)}
details{margin-top:28px}
summary{font-size:13px;color:var(--muted);text-transform:uppercase;margin:0 16px;cursor:pointer}
pre{background:var(--card);border-radius:12px;padding:12px 16px;margin:6px 0 0;font:12px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--muted);white-space:pre-wrap;overflow-wrap:anywhere}
"#;

const SCRIPT: &str = "
document.addEventListener('submit',e=>{const b=e.target.querySelector('button');if(b){b.dataset.label=b.textContent;b.disabled=true;b.textContent='Bitte warten…'}});
document.addEventListener('click',e=>{const a=e.target.closest('a.row');if(a&&a.querySelector('.chev'))a.classList.add('busy')});
addEventListener('pageshow',()=>{document.querySelectorAll('.busy').forEach(a=>a.classList.remove('busy'));document.querySelectorAll('button[data-label]').forEach(b=>{b.disabled=false;b.textContent=b.dataset.label})});
";

struct Request {
    method: String,
    path: String,
    host: String,
    params: Vec<(String, String)>,
}

impl Request {
    /// Get a query or form value by name, empty string if missing.
    fn param(&self, name: &str) -> String {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default()
    }
}

/// Serve the web interface on localhost and open it in the browser.
pub fn run() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let host = listener.local_addr()?.to_string();
    let token = format!("{:016x}", RandomState::new().build_hasher().finish());
    let url = format!("http://{}/", host);
    println!("Web-Oberfläche läuft auf {} (Strg+C zum Beenden)", url);
    let _ = notify::open_url(&url);

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let (status, body) = match read_request(&stream) {
            Ok(req) => handle(&req, &host, &token).unwrap_or_else(|e| {
                let msg = format!("{:#}", e);
                ("500 Internal Server Error", page(Some("/"), "Fehler", &banner(&msg, "bad")))
            }),
            Err(e) => ("400 Bad Request", page(Some("/"), "Fehler", &banner(&e.to_string(), "bad"))),
        };
        let _ = write_response(&mut stream, status, &body);
    }
    Ok(())
}

/// Read method, path, host header and query plus form values from the stream.
fn read_request(stream: &TcpStream) -> Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().context("Leere Anfrage")?.to_string();
    let target = parts.next().context("Anfrage ohne Pfad")?.to_string();

    let mut host = String::new();
    let mut length = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "host" => host = value.trim().to_string(),
                "content-length" => length = value.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if length > MAX_BODY {
        bail!("Anfrage zu groß");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;

    let url = Url::parse(&format!("http://localhost{}", target))?;
    let form = Url::parse(&format!("http://localhost/?{}", String::from_utf8_lossy(&body)))?;
    let params = url.query_pairs().into_owned().chain(form.query_pairs().into_owned()).collect();
    Ok(Request {
        method,
        path: url.path().to_string(),
        host,
        params,
    })
}

/// Route one request to the matching page or action.
fn handle(req: &Request, host: &str, token: &str) -> Result<(&'static str, String)> {
    // NOTE: we only answer for our own host and want the token on every POST, otherwise any website open in the browser could start a watcher via DNS rebinding or a hidden form.
    if req.host != host || (req.method == "POST" && req.param("token") != token) {
        return Ok(("403 Forbidden", page(Some("/"), "Nicht erlaubt", &banner("Ungültige Anfrage.", "bad"))));
    }

    let body = match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => home(token, None),
        ("GET", "/setup/herne") => herne_setup(req, token)?,
        ("GET", "/setup/bochum") => bochum_setup(token),
        ("POST", "/check") => {
            let cfg = Config::load()?;
            let result = watcher::run_check(
                &cfg,
                cfg.earliest_date_parsed().as_ref(),
                cfg.latest_date_parsed().as_ref(),
            )?;
            home(token, Some(result.summary()))
        }
        ("POST", "/start") => {
            restart_watcher()?;
            home(token, Some("Suche gestartet.".into()))
        }
        ("POST", "/stop") => {
            if service::is_installed() {
                service::uninstall()?;
            }
            home(token, Some("Suche beendet.".into()))
        }
        ("POST", "/save") => {
            config_from_form(req)?.save()?;
            restart_watcher()?;
            home(token, Some("Gespeichert. Die Suche läuft jetzt im Hintergrund.".into()))
        }
        _ => return Ok(("404 Not Found", page(Some("/"), "Nicht gefunden", ""))),
    };
    Ok(("200 OK", body))
}

/// Install the watcher service fresh so it picks up the current config.
fn restart_watcher() -> Result<()> {
    Config::load()?;
    if service::is_installed() {
        service::uninstall()?;
    }
    service::install()
}

/// Load the config only if its deadline is not over yet.
fn active_config() -> Option<Config> {
    let today = Local::now().date_naive();
    Config::load()
        .ok()
        .filter(|c| c.deadline_parsed().map_or(true, |d| d >= today))
}

/// Build the start page, search status only when a search is set up, otherwise the city choice.
fn home(token: &str, message: Option<String>) -> String {
    let mut html = String::new();
    if let Some(m) = message {
        let kind = if m.starts_with("TERMINE GEFUNDEN") {
            "ok"
        } else if m.starts_with("Fehler") {
            "bad"
        } else {
            ""
        };
        html += &banner(&m, kind);
    }

    let cities = city_list();
    match active_config() {
        Some(cfg) if service::is_installed() => {
            let running = service::is_running();
            let (kind, title, text) = if running {
                ("ok", "Suche läuft", "Meldet sich, sobald ein passender Termin frei ist.")
            } else {
                ("warn", "Suche pausiert", "Der Hintergrunddienst ist eingerichtet, läuft aber gerade nicht.")
            };
            html += &format!(
                "<section class=\"hero\"><div class=\"dot {}\"></div><h2>{}</h2><p>{} · {}</p><p class=\"muted\">{}</p></section>",
                kind,
                title,
                escape_html(&cfg.service_name),
                escape_html(&cfg.location_name),
                text
            );

            let topic = escape_html(&cfg.ntfy_topic);
            html += &format!(
                "<h3 class=\"section\">Details</h3><div class=\"group\">{}{}{}</div><p class=\"footnote\">Installiere die App ntfy und abonniere das Topic, dann kommen die Termine aufs Handy.</p>",
                info_row("Zeitraum", &escape_html(&date_range(&cfg))),
                info_row("Suche endet am", &escape_html(&cfg.deadline)),
                info_row("Push", &format!("<a href=\"https://ntfy.sh/{t}\">{t}</a>", t = topic)),
            );

            html += "<div class=\"buttons\">";
            html += &post_button(token, "/check", "Jetzt prüfen", "primary");
            if !running {
                html += &post_button(token, "/start", "Suche fortsetzen", "");
            }
            html += &post_button(token, "/stop", "Suche beenden", "danger");
            html += "</div>";

            html += &format!("<h3 class=\"section\">Neue Suche</h3>{}", cities);
            html += &format!(
                "<details><summary>Protokoll</summary><pre>{}</pre></details>",
                escape_html(&log_tail(15))
            );
            page(None, "Termin-Checker", &html)
        }
        cfg => {
            html += "<p class=\"lead\">Wähle deine Stadt. Der Termin-Checker sucht im Hintergrund und meldet sich, sobald ein Termin frei ist.</p>";
            html += &format!("<h3 class=\"section\">Stadt</h3>{}", cities);
            if let Some(cfg) = cfg {
                html += &format!(
                    "<div class=\"buttons\">{}</div><p class=\"footnote\">{} · {}</p>",
                    post_button(token, "/start", "Letzte Suche fortsetzen", ""),
                    escape_html(&cfg.service_name),
                    escape_html(&cfg.location_name)
                );
            }
            page(None, "Termin finden", &html)
        }
    }
}

/// Describe earliest and latest date as one short text.
fn date_range(cfg: &Config) -> String {
    match (&cfg.earliest_date, &cfg.latest_date) {
        (None, None) => "jederzeit".into(),
        (Some(e), None) => format!("ab {}", e),
        (None, Some(l)) => format!("bis {}", l),
        (Some(e), Some(l)) if e == l => format!("am {}", e),
        (Some(e), Some(l)) => format!("{} bis {}", e, l),
    }
}

/// Build the list with the supported cities.
fn city_list() -> String {
    link_list(&[
        ("/setup/herne".into(), "Herne / Kreis Recklinghausen".into(), "tevis.krzn.de".into()),
        ("/setup/bochum".into(), "Bochum".into(), "termine.bochum.de".into()),
    ])
}

/// Read the last lines of the watcher log.
fn log_tail(count: usize) -> String {
    Config::config_dir()
        .ok()
        .and_then(|d| std::fs::read_to_string(d.join("watcher.log")).ok())
        .map(|content| {
            let lines: Vec<&str> = content.lines().collect();
            lines[lines.len().saturating_sub(count)..].join("\n")
        })
        .unwrap_or_else(|| "Noch kein Protokoll vorhanden.".into())
}

/// Walk through department, service and location on tevis using query params, then show the date form.
fn herne_setup(req: &Request, token: &str) -> Result<String> {
    let (dept, svc, loc) = (req.param("dept"), req.param("svc"), req.param("loc"));
    let session = Session::new()?;
    session.init_session()?;

    let depts = session.fetch_departments()?;
    if dept.is_empty() {
        let rows: Vec<_> = depts
            .iter()
            .map(|d| (herne_url(&[("dept", &d.id)]), d.name.clone(), String::new()))
            .collect();
        return Ok(herne_step(1, "Abteilung", "/", &link_list(&rows)));
    }
    let dept_name = &depts.iter().find(|d| d.id == dept).context("Abteilung nicht gefunden")?.name;

    let services = session.select_department(&dept)?;
    if svc.is_empty() {
        let rows: Vec<_> = services
            .iter()
            .map(|s| (herne_url(&[("dept", &dept), ("svc", &s.id)]), s.name.clone(), String::new()))
            .collect();
        return Ok(herne_step(2, "Anliegen", &herne_url(&[]), &link_list(&rows)));
    }
    let svc_name = &services.iter().find(|s| s.id == svc).context("Anliegen nicht gefunden")?.name;

    let locs = session.select_service(&svc)?;
    if loc.is_empty() && locs.len() > 1 {
        let rows: Vec<_> = locs
            .iter()
            .map(|l| {
                let href = herne_url(&[("dept", &dept), ("svc", &svc), ("loc", &l.id)]);
                (href, l.name.clone(), l.address.clone())
            })
            .collect();
        let back = herne_url(&[("dept", &dept)]);
        return Ok(herne_step(3, "Standort", &back, &link_list(&rows)));
    }
    let location = locs
        .iter()
        .find(|l| loc.is_empty() || l.id == loc)
        .context("Standort nicht gefunden")?;

    let hidden = [
        ("provider", "herne"),
        ("department_id", &dept),
        ("department_name", dept_name),
        ("service_id", &svc),
        ("service_name", svc_name),
        ("location_id", &location.id),
        ("location_name", &location.name),
        ("gps_lat", &location.gps_lat),
        ("gps_long", &location.gps_long),
    ];
    let back = if locs.len() > 1 {
        herne_url(&[("dept", &dept), ("svc", &svc)])
    } else {
        herne_url(&[("dept", &dept)])
    };
    let summary = format!(
        "<p class=\"lead\">{} · {}</p>",
        escape_html(svc_name),
        escape_html(&location.name)
    );
    Ok(herne_step(4, "Zeitraum", &back, &format!("{}{}", summary, save_form(token, &hidden, ""))))
}

/// Wrap one Herne setup step in a page with back link and step counter.
fn herne_step(step: u8, title: &str, back: &str, body: &str) -> String {
    page(
        Some(back),
        title,
        &format!("<p class=\"step\">Herne · Schritt {} von 4</p>{}", step, body),
    )
}

/// Build the path and query for a Herne setup step.
fn herne_url(params: &[(&str, &str)]) -> String {
    let url = Url::parse_with_params("http://localhost/setup/herne", params).expect("static url");
    match url.query().filter(|q| !q.is_empty()) {
        Some(q) => format!("{}?{}", url.path(), q),
        None => url.path().to_string(),
    }
}

/// Show the Bochum form, prefilled from the current config if it is a Bochum one.
fn bochum_setup(token: &str) -> String {
    let cfg = Config::load().ok().filter(|c| c.provider == "bochum");
    let value = |pick: fn(&Config) -> Option<String>| cfg.as_ref().and_then(pick).unwrap_or_default();
    let fields = format!(
        "<h3 class=\"section\">Termin</h3><div class=\"group\">{}{}</div>\
         <h3 class=\"section\">Kalender</h3><div class=\"group\">{}{}{}</div>\
         <p class=\"footnote\">Kalender und Kalender-UID stehen im Buchungslink termine.bochum.de/m/KALENDER/extern/calendar/?uid=UID. Die Anliegen-UID zeigen die Entwicklertools des Browsers im Tab Netzwerk, Anfrage appointment/search, Wert services.</p>",
        input_row("Anliegen", "text", "service_name", &value(|c| Some(c.service_name.clone())), "z. B. Kurzzeitkennzeichen"),
        input_row("Standort", "text", "location_name", &value(|c| Some(c.location_name.clone())), "z. B. Kfz-Zulassung"),
        input_row("Kalender", "text", "calendar_path", &value(|c| c.calendar_path.clone()), "kfz-angelegenheiten"),
        input_row("Kalender-UID", "text", "calendar_uid", &value(|c| c.calendar_uid.clone()), "uid aus dem Link"),
        input_row("Anliegen-UID", "text", "service_uid", &value(|c| c.service_uid.clone()), "services"),
    );
    page(Some("/"), "Bochum", &save_form(token, &[("provider", "bochum")], &fields))
}

/// Build a Config from the setup form, check dates, ntfy topic and ids.
fn config_from_form(req: &Request) -> Result<Config> {
    let provider = req.param("provider");
    let optional = |name: &str| Some(req.param(name)).filter(|v| !v.is_empty());

    let mut ntfy_topic = req.param("ntfy_topic");
    if ntfy_topic.is_empty() {
        ntfy_topic = tui::random_ntfy_topic();
    }
    if !is_plain_id(&ntfy_topic) {
        bail!("Das ntfy Topic darf nur Buchstaben, Ziffern, - und _ enthalten.");
    }

    let config = Config {
        base_url: if provider == "herne" { BASE_URL.to_string() } else { String::new() },
        department_id: req.param("department_id"),
        department_name: req.param("department_name"),
        service_id: req.param("service_id"),
        service_name: req.param("service_name"),
        location_id: req.param("location_id"),
        location_name: req.param("location_name"),
        gps_lat: req.param("gps_lat"),
        gps_long: req.param("gps_long"),
        latest_date: parse_form_date(&req.param("latest_date"))?,
        earliest_date: parse_form_date(&req.param("earliest_date"))?,
        ntfy_topic,
        deadline: parse_form_date(&req.param("deadline"))?.context("Bitte angeben, wann die Suche enden soll.")?,
        calendar_path: optional("calendar_path"),
        calendar_uid: optional("calendar_uid"),
        service_uid: optional("service_uid"),
        provider,
    };

    let complete = match config.provider.as_str() {
        "herne" => [&config.department_id, &config.service_id, &config.location_id]
            .iter()
            .all(|v| is_plain_id(v)),
        "bochum" => [&config.calendar_path, &config.calendar_uid, &config.service_uid]
            .iter()
            .all(|v| v.as_deref().is_some_and(is_plain_id)),
        _ => false,
    };
    if !complete {
        bail!("Angaben unvollständig oder ungültig, bitte das Formular prüfen.");
    }
    Ok(config)
}

/// Parse a date from the browser date picker or typed as TT.MM.JJJJ and return it as TT.MM.JJJJ.
fn parse_form_date(value: &str) -> Result<Option<String>> {
    if value.is_empty() {
        return Ok(None);
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(value, "%d.%m.%Y"))
        .with_context(|| format!("Das Datum '{}' ist ungültig.", value))?;
    Ok(Some(date.format("%d.%m.%Y").to_string()))
}

/// Check that a value only has letters, digits, - and _ so it is safe inside a URL.
fn is_plain_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Build the POST form with the shared date and push fields after the provider specific fields.
fn save_form(token: &str, hidden: &[(&str, &str)], fields: &str) -> String {
    let deadline = (Local::now().date_naive() + chrono::Duration::weeks(2))
        .format("%Y-%m-%d")
        .to_string();
    format!(
        "<form method=\"post\" action=\"/save\">{}{}{}\
         <h3 class=\"section\">Zeitraum</h3><div class=\"group\">{}{}</div><p class=\"footnote\">Leer lassen, wenn jeder Termin passt.</p>\
         <h3 class=\"section\">Suche endet am</h3><div class=\"group\">{}</div><p class=\"footnote\">Danach hört der Termin-Checker von selbst auf.</p>\
         <h3 class=\"section\">Push aufs Handy</h3><div class=\"group\">{}</div><p class=\"footnote\">Optional. Leer lassen, dann wird ein zufälliges Topic erzeugt.</p>\
         <div class=\"buttons\"><button class=\"primary\">Suche starten</button></div></form>",
        hidden_inputs(&[("token", token)]),
        hidden_inputs(hidden),
        fields,
        input_row("Frühestens", "date", "earliest_date", "", ""),
        input_row("Spätestens", "date", "latest_date", "", ""),
        input_row("Datum", "date", "deadline", &deadline, ""),
        input_row("ntfy Topic", "text", "ntfy_topic", "", "automatisch"),
    )
}

/// Build a list of tappable rows with title, subtitle and chevron.
fn link_list(rows: &[(String, String, String)]) -> String {
    let rows: String = rows
        .iter()
        .map(|(href, title, subtitle)| {
            let subtitle = if subtitle.is_empty() {
                String::new()
            } else {
                format!("<small>{}</small>", escape_html(subtitle))
            };
            format!(
                "<a class=\"row\" href=\"{}\"><span>{}{}</span><span class=\"chev\" aria-hidden=\"true\">›</span></a>",
                escape_html(href),
                escape_html(title),
                subtitle
            )
        })
        .collect();
    format!("<div class=\"group\">{}</div>", rows)
}

/// Build a row with a label and an already escaped value on the right.
fn info_row(label: &str, value_html: &str) -> String {
    format!("<div class=\"row\"><span>{}</span><span class=\"value\">{}</span></div>", label, value_html)
}

/// Build a row with a label and an input on the right.
fn input_row(label: &str, kind: &str, name: &str, value: &str, placeholder: &str) -> String {
    format!(
        "<label class=\"row\"><span>{}</span><input type=\"{}\" name=\"{}\" value=\"{}\" placeholder=\"{}\"></label>",
        escape_html(label),
        kind,
        name,
        escape_html(value),
        escape_html(placeholder)
    )
}

/// Build hidden inputs for the given name and value pairs.
fn hidden_inputs(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("<input type=\"hidden\" name=\"{}\" value=\"{}\">", k, escape_html(v)))
        .collect()
}

/// Build a POST form with one full width button and the token.
fn post_button(token: &str, path: &str, label: &str, class: &str) -> String {
    format!(
        "<form method=\"post\" action=\"{}\">{}<button class=\"{}\">{}</button></form>",
        path,
        hidden_inputs(&[("token", token)]),
        class,
        label
    )
}

/// Build a message box, kind ok or bad colors it.
fn banner(text: &str, kind: &str) -> String {
    format!("<div class=\"banner {}\" role=\"status\">{}</div>", kind, escape_html(text))
}

/// Wrap the body in the html page with optional back link, large title, style and script.
fn page(back: Option<&str>, title: &str, body: &str) -> String {
    let back = back
        .map(|href| format!("<a class=\"back\" href=\"{}\">‹ Zurück</a>", escape_html(href)))
        .unwrap_or_default();
    format!(
        "<!doctype html><html lang=\"de\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{} · Termin-Checker</title><style>{}</style></head><body><main>{}<h1>{}</h1>{}</main><script>{}</script></body></html>",
        escape_html(title),
        STYLE,
        back,
        escape_html(title),
        body,
        SCRIPT
    )
}

/// Escape text for use in html content and attributes.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Write a html response and close the connection.
fn write_response(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: frame-ancestors 'none'\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(pairs: &[(&str, &str)]) -> Request {
        Request {
            method: "POST".into(),
            path: "/save".into(),
            host: "127.0.0.1:1".into(),
            params: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    const BOCHUM: &[(&str, &str)] = &[
        ("provider", "bochum"),
        ("calendar_path", "kfz-angelegenheiten"),
        ("calendar_uid", "bffa082e-0640-40b4-ad60-4a98355850ff"),
        ("service_uid", "03ae67e1-9222-4c34-bc52-f4037b54878c"),
        ("deadline", "2026-12-01"),
    ];

    #[test]
    fn bochum_form_builds_config_with_random_topic_and_german_dates() {
        let cfg = config_from_form(&form(BOCHUM)).unwrap();
        assert_eq!(cfg.provider, "bochum");
        assert_eq!(cfg.deadline, "01.12.2026");
        assert!(cfg.ntfy_topic.starts_with("termin-checker-"));
        assert_eq!(cfg.latest_date, None);
    }

    #[test]
    fn form_rejects_bad_date_topic_and_missing_ids() {
        let with = |extra: (&'static str, &'static str)| {
            let mut pairs = BOCHUM.to_vec();
            pairs.insert(0, extra);
            config_from_form(&form(&pairs))
        };
        assert!(with(("latest_date", "31.02.2026")).is_err());
        assert!(with(("ntfy_topic", "a/../b")).is_err());
        assert!(config_from_form(&form(&[("provider", "bochum"), ("deadline", "01.12.2026")])).is_err());
    }

    #[test]
    fn post_without_token_is_forbidden() {
        let (status, _) = handle(&form(BOCHUM), "127.0.0.1:1", "secret").unwrap();
        assert_eq!(status, "403 Forbidden");
    }

    #[test]
    fn herne_url_encodes_params() {
        assert_eq!(herne_url(&[]), "/setup/herne");
        assert_eq!(herne_url(&[("dept", "2"), ("svc", "a&b")]), "/setup/herne?dept=2&svc=a%26b");
    }

    #[test]
    fn escapes_html() {
        assert_eq!(escape_html("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
    }
}
