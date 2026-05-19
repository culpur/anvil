//! systemd-user backend (Linux with systemd).
//!
//! Writes a paired `.service` + `.timer` unit under
//! `~/.config/systemd/user/` and enables the timer with
//! `systemctl --user enable --now <name>.timer`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use super::{
    Backend, InstalledSchedule, Interval, Schedule, ScheduleError, ScheduleStatus,
    home_dir_for_test,
};

pub(crate) fn unit_dir() -> Result<PathBuf, ScheduleError> {
    let home = home_dir_for_test()
        .ok_or_else(|| ScheduleError::Io("HOME env not set".to_string()))?;
    Ok(home.join(".config").join("systemd").join("user"))
}

pub(crate) fn service_path(schedule: &Schedule) -> Result<PathBuf, ScheduleError> {
    Ok(unit_dir()?.join(format!("{}.service", schedule.name)))
}

pub(crate) fn timer_path(schedule: &Schedule) -> Result<PathBuf, ScheduleError> {
    Ok(unit_dir()?.join(format!("{}.timer", schedule.name)))
}

pub(crate) fn render_service(schedule: &Schedule) -> String {
    format!(
        "[Unit]\nDescription=Anvil scheduled job: {name}\n\n\
         [Service]\nType=oneshot\nExecStart=/bin/sh -c {cmd}\n",
        name = schedule.name,
        cmd = sh_single_quote(&schedule.command),
    )
}

pub(crate) fn render_timer(schedule: &Schedule) -> (String, Option<String>) {
    let mut note: Option<String> = None;
    let trigger = match &schedule.interval {
        Interval::Every15Min => "OnUnitActiveSec=15min\nOnBootSec=2min".to_string(),
        Interval::Hourly => "OnCalendar=hourly".to_string(),
        Interval::Every4Hours => "OnUnitActiveSec=4h\nOnBootSec=5min".to_string(),
        Interval::Daily { hour } => format!("OnCalendar=*-*-* {hour:02}:00:00"),
        Interval::Custom(expr) => {
            note = Some(format!(
                "Custom cron expression `{expr}` is not native to systemd; \
                 mapped to hourly. Use the `cron` backend for verbatim cron syntax."
            ));
            "OnCalendar=hourly".to_string()
        }
    };
    let body = format!(
        "[Unit]\nDescription=Anvil scheduled timer: {name}\n\n\
         [Timer]\n{trigger}\nPersistent=true\nUnit={name}.service\n\n\
         [Install]\nWantedBy=timers.target\n",
        name = schedule.name,
    );
    (body, note)
}

fn sh_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn run_systemctl(args: &[&str]) -> Result<(bool, String), ScheduleError> {
    let out = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| ScheduleError::Subprocess(format!("systemctl exec failed: {e}")))?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    Ok((out.status.success(), stderr))
}

pub(crate) fn install(schedule: &Schedule) -> Result<InstalledSchedule, ScheduleError> {
    let service = service_path(schedule)?;
    let timer = timer_path(schedule)?;
    if let Some(parent) = service.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| ScheduleError::Io(format!("create_dir_all {}: {e}", parent.display())))?;
    }
    fs::write(&service, render_service(schedule))
        .map_err(|e| ScheduleError::Io(format!("write {}: {e}", service.display())))?;
    let (timer_body, note) = render_timer(schedule);
    fs::write(&timer, timer_body)
        .map_err(|e| ScheduleError::Io(format!("write {}: {e}", timer.display())))?;
    let mut notes: Vec<String> = note.into_iter().collect();
    let timer_unit = format!("{}.timer", schedule.name);
    let reload_cmd = format!("systemctl --user enable --now {timer_unit}");

    match run_systemctl(&["--user", "daemon-reload"]) {
        Ok((true, _)) => {}
        Ok((false, stderr)) if !stderr.is_empty() => {
            notes.push(format!("daemon-reload: {}", stderr.trim()))
        }
        Ok((false, _)) => {}
        Err(e) => notes.push(format!("daemon-reload failed: {e}")),
    }
    match run_systemctl(&["--user", "enable", "--now", &timer_unit]) {
        Ok((true, _)) => {}
        Ok((false, stderr)) => {
            if !stderr.is_empty() {
                notes.push(format!("enable --now: {}", stderr.trim()));
            }
        }
        Err(e) => notes.push(format!("enable --now failed: {e}")),
    }
    Ok(InstalledSchedule {
        backend: Backend::SystemdUser,
        artifacts: vec![service, timer],
        reload_cmd: Some(reload_cmd),
        notes,
    })
}

pub(crate) fn uninstall(schedule: &Schedule) -> Result<(), ScheduleError> {
    let service = service_path(schedule)?;
    let timer = timer_path(schedule)?;
    let timer_unit = format!("{}.timer", schedule.name);
    if timer.exists() {
        let _ = run_systemctl(&["--user", "disable", "--now", &timer_unit]);
    }
    for p in [&timer, &service] {
        if p.exists() {
            fs::remove_file(p)
                .map_err(|e| ScheduleError::Io(format!("remove {}: {e}", p.display())))?;
        }
    }
    let _ = run_systemctl(&["--user", "daemon-reload"]);
    Ok(())
}

pub(crate) fn status(schedule: &Schedule) -> Result<ScheduleStatus, ScheduleError> {
    let service = service_path(schedule)?;
    let timer = timer_path(schedule)?;
    if !service.exists() || !timer.exists() {
        return Ok(ScheduleStatus::NotInstalled);
    }
    let timer_unit = format!("{}.timer", schedule.name);
    let active = Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", &timer_unit])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if active {
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
    #[serial]
    fn unit_dir_lives_under_config_systemd_user() {
        with_home(|home| {
            let p = unit_dir().unwrap();
            assert!(p.starts_with(home));
            assert!(p.ends_with(".config/systemd/user"));
        });
    }

    #[test]
    fn render_service_uses_oneshot_type() {
        let s = Schedule::new("qmd-refresh", "qmd update", Interval::Hourly).unwrap();
        let body = render_service(&s);
        assert!(body.contains("Type=oneshot"));
        assert!(body.contains("ExecStart=/bin/sh -c 'qmd update'"));
    }

    #[test]
    fn render_service_escapes_embedded_single_quotes() {
        let s = Schedule::new("q", "echo 'hi' && echo 'world'", Interval::Hourly).unwrap();
        let body = render_service(&s);
        assert!(body.contains("'\\''hi'\\''"));
    }

    #[test]
    fn render_timer_uses_oncalendar_for_hourly_and_daily() {
        let s = Schedule::new("h", "qmd update", Interval::Hourly).unwrap();
        let (body, _) = render_timer(&s);
        assert!(body.contains("OnCalendar=hourly"));

        let d = Schedule::new("d", "qmd update", Interval::Daily { hour: 3 }).unwrap();
        let (body, _) = render_timer(&d);
        assert!(body.contains("OnCalendar=*-*-* 03:00:00"));
    }

    #[test]
    fn render_timer_uses_onunitactivesec_for_fixed_interval() {
        let s = Schedule::new("e15", "qmd update", Interval::Every15Min).unwrap();
        let (body, _) = render_timer(&s);
        assert!(body.contains("OnUnitActiveSec=15min"));
    }

    #[test]
    fn render_timer_warns_on_custom_expression() {
        let s = Schedule::new("c", "qmd update", Interval::Custom("*/5 * * * *".to_string())).unwrap();
        let (_, note) = render_timer(&s);
        assert!(note.is_some());
        assert!(note.unwrap().to_lowercase().contains("custom"));
    }

    #[test]
    fn render_timer_carries_install_section_for_persistence() {
        let s = Schedule::new("p", "qmd update", Interval::Hourly).unwrap();
        let (body, _) = render_timer(&s);
        assert!(body.contains("WantedBy=timers.target"));
        assert!(body.contains("Persistent=true"));
    }

    #[test]
    #[serial]
    fn paths_use_schedule_name() {
        with_home(|_home| {
            let s = Schedule::new("qmd-refresh", "x", Interval::Hourly).unwrap();
            assert!(service_path(&s).unwrap().ends_with("qmd-refresh.service"));
            assert!(timer_path(&s).unwrap().ends_with("qmd-refresh.timer"));
        });
    }
}
