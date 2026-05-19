//! macOS LaunchAgent backend.
//!
//! Writes a `~/Library/LaunchAgents/<label>.plist` and loads it via
//! `launchctl load -w`. The label is derived from the schedule name
//! (`net.qmd.<name>`).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use super::{
    Backend, InstalledSchedule, Interval, Schedule, ScheduleError, ScheduleStatus,
    home_dir_for_test,
};

pub(crate) fn label_for(schedule: &Schedule) -> String {
    format!("net.qmd.{}", schedule.name)
}

pub(crate) fn plist_path(schedule: &Schedule) -> Result<PathBuf, ScheduleError> {
    let home = home_dir_for_test()
        .ok_or_else(|| ScheduleError::Io("HOME env not set".to_string()))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", label_for(schedule))))
}

pub(crate) fn render_plist(schedule: &Schedule) -> (String, Option<String>) {
    let label = label_for(schedule);
    let mut note: Option<String> = None;
    let interval_block = match &schedule.interval {
        Interval::Every15Min => "<key>StartInterval</key><integer>900</integer>".to_string(),
        Interval::Hourly => "<key>StartInterval</key><integer>3600</integer>".to_string(),
        Interval::Every4Hours => {
            "<key>StartInterval</key><integer>14400</integer>".to_string()
        }
        Interval::Daily { hour } => format!(
            "<key>StartCalendarInterval</key>\n    <dict>\n      <key>Hour</key><integer>{hour}</integer>\n      <key>Minute</key><integer>0</integer>\n    </dict>"
        ),
        Interval::Custom(_) => {
            note = Some(
                "launchd does not support cron expressions; falling back to Hourly. \
                 Pick the `cron` backend if you need a precise schedule."
                    .to_string(),
            );
            "<key>StartInterval</key><integer>3600</integer>".to_string()
        }
    };
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>
      <string>/bin/sh</string>
      <string>-c</string>
      <string>{cmd}</string>
    </array>
    {interval_block}
    <key>RunAtLoad</key><false/>
    <key>StandardOutPath</key><string>/tmp/{label}.out.log</string>
    <key>StandardErrorPath</key><string>/tmp/{label}.err.log</string>
  </dict>
</plist>
"#,
        label = label,
        cmd = xml_escape(&schedule.command),
        interval_block = interval_block,
    );
    (body, note)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn run_launchctl(args: &[&str]) -> Result<(bool, String), ScheduleError> {
    let out = Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| ScheduleError::Subprocess(format!("launchctl exec failed: {e}")))?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    Ok((out.status.success(), stderr))
}

pub(crate) fn install(schedule: &Schedule) -> Result<InstalledSchedule, ScheduleError> {
    if !cfg!(target_os = "macos") {
        return Err(ScheduleError::BackendUnavailable(
            "launchd is macOS-only".to_string(),
        ));
    }
    let path = plist_path(schedule)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| ScheduleError::Io(format!("create_dir_all {}: {e}", parent.display())))?;
    }
    let (body, note) = render_plist(schedule);
    fs::write(&path, body).map_err(|e| {
        ScheduleError::Io(format!("write plist {}: {e}", path.display()))
    })?;
    let load_args: [&str; 3] = ["load", "-w", path.to_str().unwrap_or_default()];
    let mut notes: Vec<String> = note.into_iter().collect();
    let reload_cmd = format!("launchctl load -w {}", path.display());
    match run_launchctl(&load_args) {
        Ok((true, _)) => {}
        Ok((false, stderr)) => {
            if stderr.contains("already loaded") || stderr.contains("Service is disabled") {
                notes.push(format!(
                    "launchctl reported `{}`; treating as already-installed",
                    stderr.trim()
                ));
            } else if !stderr.is_empty() {
                notes.push(format!("launchctl load stderr: {}", stderr.trim()));
            }
        }
        Err(e) => notes.push(format!("launchctl missing or unusable: {e}")),
    }
    Ok(InstalledSchedule {
        backend: Backend::Launchd,
        artifacts: vec![path],
        reload_cmd: Some(reload_cmd),
        notes,
    })
}

pub(crate) fn uninstall(schedule: &Schedule) -> Result<(), ScheduleError> {
    let path = plist_path(schedule)?;
    if path.exists() {
        let _ = run_launchctl(&["unload", "-w", path.to_str().unwrap_or_default()]);
        fs::remove_file(&path)
            .map_err(|e| ScheduleError::Io(format!("remove plist {}: {e}", path.display())))?;
    }
    Ok(())
}

pub(crate) fn status(schedule: &Schedule) -> Result<ScheduleStatus, ScheduleError> {
    let path = plist_path(schedule)?;
    if !path.exists() {
        return Ok(ScheduleStatus::NotInstalled);
    }
    let label = label_for(schedule);
    let listed = Command::new("launchctl")
        .arg("list")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&label))
        .unwrap_or(false);
    if listed {
        Ok(ScheduleStatus::Installed)
    } else {
        Ok(ScheduleStatus::InstalledButDisabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Interval;
    use serial_test::serial;
    use tempfile::TempDir;

    fn with_home<F: FnOnce(&std::path::Path)>(f: F) {
        let prev = std::env::var_os("HOME");
        let tmp = TempDir::new().expect("tempdir");
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        f(tmp.path());
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn label_for_uses_net_qmd_namespace() {
        let s = Schedule::new("qmd-refresh", "echo ok", Interval::Hourly).unwrap();
        assert_eq!(label_for(&s), "net.qmd.qmd-refresh");
    }

    #[test]
    #[serial]
    fn plist_path_lives_under_library_launch_agents() {
        with_home(|home| {
            let s = Schedule::new("qmd-refresh", "echo ok", Interval::Hourly).unwrap();
            let p = plist_path(&s).unwrap();
            assert!(p.starts_with(home));
            assert!(p
                .to_string_lossy()
                .ends_with("Library/LaunchAgents/net.qmd.qmd-refresh.plist"));
        });
    }

    #[test]
    fn render_plist_emits_start_interval_for_hourly() {
        let s = Schedule::new("qmd-refresh", "qmd update", Interval::Hourly).unwrap();
        let (body, note) = render_plist(&s);
        assert!(body.contains("<key>StartInterval</key>"));
        assert!(body.contains("<integer>3600</integer>"));
        assert!(body.contains("net.qmd.qmd-refresh"));
        assert!(note.is_none());
    }

    #[test]
    fn render_plist_emits_calendar_interval_for_daily() {
        let s = Schedule::new("daily-refresh", "qmd update", Interval::Daily { hour: 3 })
            .unwrap();
        let (body, _note) = render_plist(&s);
        assert!(body.contains("StartCalendarInterval"));
        assert!(body.contains("<key>Hour</key><integer>3</integer>"));
    }

    #[test]
    fn render_plist_warns_on_custom_interval() {
        let s = Schedule::new(
            "custom",
            "echo ok",
            Interval::Custom("*/5 * * * *".to_string()),
        )
        .unwrap();
        let (body, note) = render_plist(&s);
        assert!(body.contains("<integer>3600</integer>"));
        assert!(note.is_some());
        assert!(note.unwrap().to_lowercase().contains("cron"));
    }

    #[test]
    fn render_plist_escapes_ampersand_in_command() {
        let s = Schedule::new("q", "qmd update && qmd embed", Interval::Hourly).unwrap();
        let (body, _) = render_plist(&s);
        assert!(body.contains("&amp;&amp;"));
        assert!(!body.contains(" && "));
    }
}
