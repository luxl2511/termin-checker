# termin-checker

Watches the online booking calendars of Herne / Kreis Recklinghausen ([tevis.krzn.de](https://tevis.krzn.de/tevisweb2000)) and Bochum ([termine.bochum.de](https://termine.bochum.de)) for free appointments. When a matching slot shows up, it sends a push notification via [ntfy.sh](https://ntfy.sh), shows a desktop notification and opens the booking page.

The user interface is in German, like the booking sites.

## Install

Download the archive for your system from the [releases page](https://github.com/luxl2511/termin-checker/releases):

| System | File |
|--------|------|
| macOS, Apple silicon | `termin-checker-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `termin-checker-x86_64-apple-darwin.tar.gz` |
| Windows | `termin-checker-x86_64-pc-windows-msvc.zip` |
| Linux | `termin-checker-x86_64-unknown-linux-gnu.tar.gz` |

Unpack it to a folder that stays, for example your user folder, and double-click `termin-checker`. A browser tab opens with the setup page. The background service starts the binary from that location, so do not run it from inside the zip or from a temporary folder.

The binaries are not signed. On macOS, Gatekeeper blocks the first start. Open System Settings, Privacy & Security, and click "Open Anyway", or run `xattr -d com.apple.quarantine termin-checker` once. Windows SmartScreen shows a similar warning. There, choose "More info" and then "Run anyway".

To build from source, install Rust and run `cargo build --release`. The binary ends up in `target/release/termin-checker`.

## Usage

Start `termin-checker` without arguments. It serves a small web page on `127.0.0.1` and opens it in the browser:

1. Pick the city.
2. For Herne, pick department, service and location from the lists. For Bochum, enter the calendar and service IDs (see below).
3. Optionally set the earliest and latest acceptable date, the ntfy topic and the end date of the search.
4. Click "Suche starten". The page saves the config and installs the background watcher.

While a search runs, the start page shows its status, a "Jetzt prüfen" button for an immediate check, and a button to end the search. Without a running search, it only shows the city choice. The web server runs until you close the terminal window or press Ctrl+C. The watcher keeps running without it.

To get notifications on the phone, install the ntfy app and subscribe to the topic shown on the start page. An empty topic field creates a random topic, because anybody who knows a topic name can read its messages.

### Bochum IDs

The Bochum booking system has no public list of services, so the IDs come from the browser:

- The calendar name and calendar UID are in the booking link: `termine.bochum.de/m/<calendar>/extern/calendar/?uid=<calendar-uid>`.
- The service UID is the `services` value of the `appointment/search` request. The network tab of the browser developer tools shows it after you select a service on the booking page.

## Commands

| Command | Description |
|---------|-------------|
| *(none)*, `web` | Open the web interface |
| `tui` | Terminal setup wizard (Herne only) |
| `watch` | Run the polling loop in the foreground (the background service calls this) |
| `check` | Run one check, print the result, exit |
| `status` | Show config, watcher state and the last log lines |
| `install` | Install and start the background service with the saved config |
| `stop` | Stop and remove the background service |

## How the watcher works

`watch` loops until the end date of the search:

1. Opens a fresh HTTP session with cookies.
2. Herne: walks the tevis flow (landing page, department, service, location, suggestion page) and parses the HTML. Bochum: loads the calendar page to get a session id (`wsid`), then calls the JSON endpoint `/api/appointment/search`.
3. Filters the slots by the earliest and latest date.
4. Matching slots: sends the ntfy push (priority urgent), a desktop notification, and opens the booking page. Repeats at most every 10 minutes.
5. Slots outside the date range or no slots: writes a log line.
6. Errors: after 5 failures in a row, sends an ntfy warning, at most every 30 minutes.

The loop waits 20 seconds between checks from 06:30 to 09:00, when many offices release same-day slots, and 30 seconds otherwise.

### Background service

| System | Service | Start |
|--------|---------|-------|
| macOS | Launch Agent `~/Library/LaunchAgents/com.lukas.termin-checker.plist`, runs under `caffeinate` so the Mac does not sleep | at login, weekdays 06:30, restart after a crash |
| Linux | systemd user unit `~/.config/systemd/user/termin-checker.service` | at login, restart after a crash |
| Windows | Task Scheduler task `TerminChecker`, no time limit, also on battery | right after install, then every 15 minutes if not running, removed at the end date |

The log goes to `~/.config/termin-checker/watcher.log` on all systems.

## Configuration

The config lives in `~/.config/termin-checker/config.toml` (also on Windows, under the user folder). The web page and the TUI write it, and you can edit it by hand. Dates use the format `DD.MM.YYYY`. `earliest_date` and `latest_date` are optional.

Herne:

```toml
provider = "herne"
base_url = "https://tevis.krzn.de/tevisweb2000"
department_id = "2"
department_name = "Zulassungs- und Fahrerlaubnisbehörde"
service_id = "261"
service_name = "Ersatzfahrzeugpapiere bei Verlust"
location_id = "19"
location_name = "Straßenverkehrsamt"
gps_lat = "51.523033"
gps_long = "7.193934"
latest_date = "20.11.2026"
ntfy_topic = "termin-checker-3f9a1c0d2b7e4a51"
deadline = "30.11.2026"
```

Bochum:

```toml
provider = "bochum"
service_name = "Kfz - Kurzzeitkennzeichen"
location_name = "Kfz-Zulassung, Bulksmühle 17, 44809 Bochum"
calendar_path = "kfz-angelegenheiten"
calendar_uid = "bffa082e-0640-40b4-ad60-4a98355850ff"
service_uid = "03ae67e1-9222-4c34-bc52-f4037b54878c"
earliest_date = "02.11.2026"
latest_date = "13.11.2026"
ntfy_topic = "termin-checker-3f9a1c0d2b7e4a51"
deadline = "30.11.2026"
```

After editing the file by hand, run `termin-checker install` so the watcher picks up the change.

## Web interface security

The web server listens only on `127.0.0.1` with a random port. It answers only requests whose `Host` header matches that address, and every action needs a random token that is embedded in the page. This blocks other websites open in the same browser from starting or changing a search.

## Project structure

```
src/
  main.rs      CLI entry point, status, check, install and stop commands
  web.rs       Local web interface (std TcpListener, server-rendered HTML)
  tui.rs       Terminal setup wizard (ratatui)
  scraper.rs   Herne tevis session and HTML parsing
  bochum.rs    Bochum session and JSON API client
  watcher.rs   Polling loop, cooldowns, error alerts
  config.rs    TOML config load and save
  notify.rs    ntfy.sh, desktop notification, open browser
  service.rs   Background service for launchd, systemd and Task Scheduler
```

## Release

Every push to `main`, every pull request and every tag `v*` runs `.github/workflows/build.yml`. It runs the unit tests and builds on Linux, Windows and macOS (Apple silicon and Intel). `ci/smoke.sh` then tests the built binary on the runner: a live check, the web interface with its token and host checks, and on macOS and Windows installing, checking and removing the background service. A tag also attaches the archives to a GitHub release.

## License

MIT, see [LICENSE](LICENSE).
