use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[cfg(any(target_os = "linux", target_os = "windows"))]
const SERVICE_NAME: &str = "termin-checker";

fn binary_path() -> Result<String> {
    std::env::current_exe()?
        .to_str()
        .map(|s| s.to_string())
        .context("Could not determine binary path")
}

fn log_dir() -> Result<PathBuf> {
    let dir = crate::config::Config::config_dir()?;
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ── macOS: launchd ──────────────────────────────────────────────────

#[cfg(target_os = "macos")]
const LABEL: &str = "com.lukas.termin-checker";

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", LABEL)))
}

#[cfg(target_os = "macos")]
pub fn install() -> Result<()> {
    let bin = binary_path()?;
    let log_path = log_dir()?.join("watcher.log");

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>

    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/caffeinate</string>
        <string>-dimsu</string>
        <string>{bin}</string>
        <string>watch</string>
    </array>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>

    <key>ProcessType</key>
    <string>Interactive</string>

    <key>StartCalendarInterval</key>
    <array>
        <dict><key>Weekday</key><integer>1</integer><key>Hour</key><integer>6</integer><key>Minute</key><integer>30</integer></dict>
        <dict><key>Weekday</key><integer>2</integer><key>Hour</key><integer>6</integer><key>Minute</key><integer>30</integer></dict>
        <dict><key>Weekday</key><integer>3</integer><key>Hour</key><integer>6</integer><key>Minute</key><integer>30</integer></dict>
        <dict><key>Weekday</key><integer>4</integer><key>Hour</key><integer>6</integer><key>Minute</key><integer>30</integer></dict>
        <dict><key>Weekday</key><integer>5</integer><key>Hour</key><integer>6</integer><key>Minute</key><integer>30</integer></dict>
    </array>

    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>"#,
        label = LABEL,
        bin = bin,
        log = log_path.display(),
    );

    let path = plist_path()?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, plist)?;

    Command::new("launchctl")
        .args(["load", path.to_str().unwrap()])
        .output()
        .context("Failed to load launchd agent")?;

    Ok(())
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> Result<()> {
    let path = plist_path()?;
    if path.exists() {
        Command::new("launchctl")
            .args(["unload", path.to_str().unwrap()])
            .output()
            .context("Failed to unload launchd agent")?;
        fs::remove_file(&path)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn is_installed() -> bool {
    plist_path().map(|p| p.exists()).unwrap_or(false)
}

#[cfg(target_os = "macos")]
pub fn is_running() -> bool {
    Command::new("launchctl")
        .args(["list", LABEL])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── Linux: systemd user service ─────────────────────────────────────

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join(format!("{}.service", SERVICE_NAME)))
}

#[cfg(target_os = "linux")]
pub fn install() -> Result<()> {
    let bin = binary_path()?;
    let log_path = log_dir()?.join("watcher.log");

    let unit = format!(
        r#"[Unit]
Description=Termin-Checker Appointment Watcher

[Service]
Type=simple
ExecStart={bin} watch
StandardOutput=append:{log}
StandardError=append:{log}
Restart=on-failure
RestartSec=30

[Install]
WantedBy=default.target"#,
        bin = bin,
        log = log_path.display(),
    );

    let path = unit_path()?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, unit)?;

    Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .context("Failed to reload systemd")?;
    Command::new("systemctl")
        .args(["--user", "enable", "--now", SERVICE_NAME])
        .output()
        .context("Failed to enable systemd service")?;

    Ok(())
}

#[cfg(target_os = "linux")]
pub fn uninstall() -> Result<()> {
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", SERVICE_NAME])
        .output();
    let path = unit_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn is_installed() -> bool {
    unit_path().map(|p| p.exists()).unwrap_or(false)
}

#[cfg(target_os = "linux")]
pub fn is_running() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", SERVICE_NAME])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── Windows: Task Scheduler ─────────────────────────────────────────

#[cfg(target_os = "windows")]
const TASK_NAME: &str = "TerminChecker";

#[cfg(target_os = "windows")]
pub fn install() -> Result<()> {
    let bin = binary_path()?;

    // Create a task that runs at logon and restarts daily at 06:30
    Command::new("schtasks")
        .args([
            "/Create",
            "/TN", TASK_NAME,
            "/TR", &format!("\"{}\" watch", bin),
            "/SC", "DAILY",
            "/ST", "06:30",
            "/F",
        ])
        .output()
        .context("Failed to create scheduled task")?;

    // Also start it immediately
    Command::new("schtasks")
        .args(["/Run", "/TN", TASK_NAME])
        .output()
        .context("Failed to start scheduled task")?;

    Ok(())
}

#[cfg(target_os = "windows")]
pub fn uninstall() -> Result<()> {
    // End the running task
    let _ = Command::new("schtasks")
        .args(["/End", "/TN", TASK_NAME])
        .output();
    Command::new("schtasks")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .output()
        .context("Failed to delete scheduled task")?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn is_installed() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
pub fn is_running() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME, "/FO", "CSV", "/V"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).contains("Running")
        })
        .unwrap_or(false)
}
