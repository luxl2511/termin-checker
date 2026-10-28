use anyhow::Result;
use reqwest::blocking::Client;

use crate::scraper::AppointmentSlot;

pub fn send_ntfy(
    topic: &str,
    service_name: &str,
    location_name: &str,
    base_url: &str,
    slots: &[AppointmentSlot],
) -> Result<()> {
    let details = slots
        .iter()
        .map(|s| format!("{}, {}: {}", s.day_name, s.date, s.times.join(" ")))
        .collect::<Vec<_>>()
        .join("\n");

    let body = format!(
        "{} @ {}:\n{}\nJetzt buchen!",
        service_name, location_name, details
    );

    let client = Client::new();
    client
        .post(format!("https://ntfy.sh/{}", topic))
        .header("Title", format!("Termin: {}", service_name))
        .header("Priority", "urgent")
        .header("Tags", "rotating_light")
        .header("Click", format!("{}/", base_url))
        .body(body)
        .send()?;

    Ok(())
}

pub fn send_ntfy_error(topic: &str, message: &str) -> Result<()> {
    let client = Client::new();
    client
        .post(format!("https://ntfy.sh/{}", topic))
        .header("Title", "Termin-Checker: Fehler")
        .header("Priority", "default")
        .header("Tags", "warning")
        .body(message.to_string())
        .send()?;

    Ok(())
}

pub fn send_desktop_notification(title: &str, subtitle: &str, message: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(format!(
                "display notification \"{}\" with title \"{}\" subtitle \"{}\" sound name \"Glass\"",
                message.replace('"', "\\\""),
                title.replace('"', "\\\""),
                subtitle.replace('"', "\\\""),
            ))
            .output()?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("notify-send")
            .arg(title)
            .arg(format!("{}\n{}", subtitle, message))
            .arg("--urgency=critical")
            .output()?;
    }

    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null; \
             $template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
             $textNodes = $template.GetElementsByTagName('text'); \
             $textNodes.Item(0).AppendChild($template.CreateTextNode('{}')) > $null; \
             $textNodes.Item(1).AppendChild($template.CreateTextNode('{}')) > $null; \
             $toast = [Windows.UI.Notifications.ToastNotification]::new($template); \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('termin-checker').Show($toast)",
            title.replace('\'', "''"),
            format!("{} - {}", subtitle, message).replace('\'', "''"),
        );
        std::process::Command::new("powershell")
            .args(["-Command", &script])
            .output()?;
    }

    Ok(())
}

pub fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(url).spawn()?;

    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open").arg(url).spawn()?;

    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()?;

    Ok(())
}
