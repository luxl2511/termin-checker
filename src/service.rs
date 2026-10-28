use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "linux")]
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

/// Run a command without opening a console window and fail with its output if it exits with an error.
#[cfg(target_os = "windows")]
fn run_hidden(program: &str, args: &[&str]) -> Result<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .with_context(|| format!("Failed to run {}", program))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        anyhow::bail!(
            "{} {} failed: {}{}",
            program,
            args.join(" "),
            text.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(text)
}

/// Build the Task Scheduler XML that starts the watcher every 15 minutes if it is not running.
#[cfg(target_os = "windows")]
fn task_xml(bin: &str) -> String {
    let bin = bin.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    // NOTE: schtasks defaults would kill the watcher after 72 hours and on battery, so we need the XML to switch both off, IgnoreNew makes the 15 minute repetition a restart if the watcher died.
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Termin-Checker watcher</Description></RegistrationInfo>
  <Triggers>
    <CalendarTrigger>
      <Repetition><Interval>PT15M</Interval><Duration>P1D</Duration><StopAtDurationEnd>false</StopAtDurationEnd></Repetition>
      <StartBoundary>2026-01-01T00:00:00</StartBoundary>
      <Enabled>true</Enabled>
      <ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay>
    </CalendarTrigger>
  </Triggers>
  <Principals><Principal id="Author"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{bin}</Command><Arguments>watch</Arguments></Exec></Actions>
</Task>"#,
        bin = bin
    )
}

#[cfg(target_os = "windows")]
pub fn install() -> Result<()> {
    let bin = binary_path()?;
    let xml_path = log_dir()?.join("task.xml");

    // NOTE: schtasks only reads the XML as UTF-16 with BOM, so we need to encode it by hand.
    let mut bytes = vec![0xFF, 0xFE];
    for unit in task_xml(&bin).encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(&xml_path, bytes)?;

    let xml_arg = xml_path.to_string_lossy().to_string();
    run_hidden("schtasks", &["/Create", "/TN", TASK_NAME, "/XML", &xml_arg, "/F"])?;
    let _ = fs::remove_file(&xml_path);
    run_hidden("schtasks", &["/Run", "/TN", TASK_NAME])?;
    Ok(())
}

/// Delete the scheduled task without stopping a running watcher, used by the watcher itself at the deadline.
#[cfg(target_os = "windows")]
pub fn delete_task() -> Result<()> {
    run_hidden("schtasks", &["/Delete", "/TN", TASK_NAME, "/F"])?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn uninstall() -> Result<()> {
    let _ = run_hidden("schtasks", &["/End", "/TN", TASK_NAME]);
    delete_task()
}

#[cfg(target_os = "windows")]
pub fn is_installed() -> bool {
    run_hidden("schtasks", &["/Query", "/TN", TASK_NAME]).is_ok()
}

#[cfg(target_os = "windows")]
pub fn is_running() -> bool {
    // NOTE: schtasks prints the status localized ("Wird ausgeführt" on German Windows), Get-ScheduledTask gives the plain enum name.
    run_hidden(
        "powershell",
        &["-NoProfile", "-Command", &format!("(Get-ScheduledTask -TaskName {}).State", TASK_NAME)],
    )
    .map(|state| state.trim() == "Running")
    .unwrap_or(false)
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn task_xml_escapes_path_and_disables_time_limit() {
        let xml = task_xml(r"C:\Users\A&B\termin-checker.exe");
        assert!(xml.contains(r"<Command>C:\Users\A&amp;B\termin-checker.exe</Command>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    }
}
