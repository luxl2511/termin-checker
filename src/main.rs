mod bochum;
mod config;
mod service;
mod notify;
mod scraper;
mod tui;
mod watcher;
mod web;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "termin-checker", about = "Termin-Checker für Herne (tevis.krzn.de) und Bochum (termine.bochum.de)")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the background watcher
    Watch,
    /// Show current config and watcher state
    Status,
    /// Stop the watcher
    Stop,
    /// Run a single check and print result
    Check,
    /// Install the launchd / systemd agent and start it
    Install,
    /// Open the web interface in the browser (default)
    Web,
    /// Open the terminal setup wizard
    Tui,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Web) => web::run(),
        Some(Commands::Tui) => tui::run_tui(),
        Some(Commands::Watch) => watcher::run(),
        Some(Commands::Status) => cmd_status(),
        Some(Commands::Stop) => cmd_stop(),
        Some(Commands::Check) => cmd_check(),
        Some(Commands::Install) => cmd_install(),
    }
}

fn cmd_install() -> Result<()> {
    config::Config::load()?; // fail loudly if no config
    if service::is_installed() {
        service::uninstall()?;
    }
    service::install()?;
    println!("Watcher installiert und gestartet.");
    Ok(())
}

fn cmd_status() -> Result<()> {
    match config::Config::load() {
        Ok(cfg) => {
            println!("Konfiguration:");
            println!("  Provider:   {}", cfg.provider);
            if cfg.provider == "bochum" {
                println!(
                    "  Kalender:   {} (uid={})",
                    cfg.calendar_path.as_deref().unwrap_or("?"),
                    cfg.calendar_uid.as_deref().unwrap_or("?"),
                );
                println!(
                    "  Anliegen:   {} (uid={})",
                    cfg.service_name,
                    cfg.service_uid.as_deref().unwrap_or("?"),
                );
                println!("  Standort:   {}", cfg.location_name);
            } else {
                println!("  Abteilung:  {} (id={})", cfg.department_name, cfg.department_id);
                println!("  Anliegen:   {} (id={})", cfg.service_name, cfg.service_id);
                println!("  Standort:   {} (id={})", cfg.location_name, cfg.location_id);
                println!("  Base URL:   {}", cfg.base_url);
            }
            if let Some(e) = &cfg.earliest_date {
                println!("  Frühestens: {}", e);
            }
            println!(
                "  Spätestens: {}",
                cfg.latest_date.as_deref().unwrap_or("beliebig")
            );
            println!("  ntfy Topic: {}", cfg.ntfy_topic);
            println!("  Deadline:   {}", cfg.deadline);
            println!();
        }
        Err(_) => {
            println!("Keine Konfiguration gefunden. Starte 'termin-checker' zum Einrichten.");
            return Ok(());
        }
    }

    if service::is_installed() {
        if service::is_running() {
            println!("Watcher: aktiv");
        } else {
            println!("Watcher: installiert, aber nicht aktiv");
        }
    } else {
        println!("Watcher: nicht installiert");
    }

    // Show log tail if available
    let log_path = config::Config::config_dir()
        .map(|d| d.join("watcher.log"));
    if let Ok(path) = log_path {
        if path.exists() {
            println!();
            println!("Letzte Log-Einträge ({}):", path.display());
            if let Ok(content) = std::fs::read_to_string(&path) {
                let lines: Vec<&str> = content.lines().collect();
                let start = lines.len().saturating_sub(10);
                for line in &lines[start..] {
                    println!("  {}", line);
                }
            }
        }
    }

    Ok(())
}

fn cmd_check() -> Result<()> {
    let cfg = config::Config::load()?;
    let result = watcher::run_check(
        &cfg,
        cfg.earliest_date_parsed().as_ref(),
        cfg.latest_date_parsed().as_ref(),
    )?;
    println!("{}", result.summary());
    Ok(())
}

fn cmd_stop() -> Result<()> {
    if service::is_installed() {
        service::uninstall()?;
        println!("Watcher gestoppt und deinstalliert.");
    } else {
        println!("Watcher ist nicht installiert.");
    }
    Ok(())
}
