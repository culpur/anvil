//! Cron backend — works on every Unix (Linux, macOS, FreeBSD, NetBSD,
//! OpenBSD). Read the user's crontab via `crontab -l`, splice or remove
//! an Anvil-managed block delimited by `# anvil-schedule:<name>` markers,
//! and write the result back via `crontab -`.

use std::io::Write;
use std::process::{Command, Stdio};

use super::{Backend, InstalledSchedule, Schedule, ScheduleError, ScheduleStatus};

pub(crate) fn begin_marker(name: &str) -> String {
    format!("# anvil-schedule:{name} BEGIN")
}

pub(crate) fn end_marker(name: &str) -> String {
    format!("# anvil-schedule:{name} END")
}

pub(crate) fn render_block(schedule: &Schedule) -> String {
    format!(
        "{begin}\n{expr} {cmd}\n{end}\n",
        begin = begin_marker(&schedule.name),
        end = end_marker(&schedule.name),
        expr = schedule.interval.to_cron_expr(),
        cmd = schedule.command,
    )
}

pub fn splice_block(body: &str, schedule: &Schedule) -> String {
    let begin = begin_marker(&schedule.name);
    let end = end_marker(&schedule.name);
    let new_block = render_block(schedule);

    if let Some(start_idx) = body.find(&begin) {
        if let Some(after_begin) = body[start_idx..].find(&end) {
            let mut span_end = start_idx + after_begin + end.len();
            if body.as_bytes().get(span_end) == Some(&b'\n') {
                span_end += 1;
            }
            let mut out = String::with_capacity(body.len());
            out.push_str(&body[..start_idx]);
            out.push_str(&new_block);
            out.push_str(&body[span_end..]);
            return out;
        }
    }
    let mut out = body.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&new_block);
    out
}

pub fn strip_block(body: &str, schedule_name: &str) -> String {
    let begin = begin_marker(schedule_name);
    let end = end_marker(schedule_name);
    let Some(start_idx) = body.find(&begin) else {
        return body.to_string();
    };
    let Some(after_begin_offset) = body[start_idx..].find(&end) else {
        return body.to_string();
    };
    let mut span_end = start_idx + after_begin_offset + end.len();
    if body.as_bytes().get(span_end) == Some(&b'\n') {
        span_end += 1;
    }
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..start_idx]);
    out.push_str(&body[span_end..]);
    out
}

pub fn has_block(body: &str, schedule_name: &str) -> bool {
    body.contains(&begin_marker(schedule_name))
}

pub(crate) fn read_crontab() -> Result<String, ScheduleError> {
    let out = Command::new("crontab")
        .arg("-l")
        .output()
        .map_err(|e| ScheduleError::Subprocess(format!("crontab -l exec failed: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Ok(String::new())
    }
}

pub(crate) fn write_crontab(body: &str) -> Result<(), ScheduleError> {
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ScheduleError::Subprocess(format!("crontab - spawn failed: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| ScheduleError::Subprocess(format!("crontab - stdin write: {e}")))?;
    }
    let status = child
        .wait_with_output()
        .map_err(|e| ScheduleError::Subprocess(format!("crontab - wait: {e}")))?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr).to_string();
        return Err(ScheduleError::Subprocess(format!(
            "crontab - exit {}: {}",
            status.status,
            stderr.trim()
        )));
    }
    Ok(())
}

pub(crate) fn install(schedule: &Schedule) -> Result<InstalledSchedule, ScheduleError> {
    let current = read_crontab()?;
    let new = splice_block(&current, schedule);
    write_crontab(&new)?;
    Ok(InstalledSchedule {
        backend: Backend::Cron,
        artifacts: Vec::new(),
        reload_cmd: None,
        notes: Vec::new(),
    })
}

pub(crate) fn uninstall(schedule: &Schedule) -> Result<(), ScheduleError> {
    let current = read_crontab()?;
    if !has_block(&current, &schedule.name) {
        return Ok(());
    }
    let new = strip_block(&current, &schedule.name);
    write_crontab(&new)?;
    Ok(())
}

pub(crate) fn status(schedule: &Schedule) -> Result<ScheduleStatus, ScheduleError> {
    let body = read_crontab()?;
    if has_block(&body, &schedule.name) {
        Ok(ScheduleStatus::Installed)
    } else {
        Ok(ScheduleStatus::NotInstalled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Interval;

    fn s(name: &str, cmd: &str) -> Schedule {
        Schedule::new(name, cmd, Interval::Hourly).unwrap()
    }

    #[test]
    fn render_block_emits_three_lines_with_markers() {
        let sch = s("qmd-refresh", "qmd update && qmd embed");
        let body = render_block(&sch);
        assert!(body.contains("# anvil-schedule:qmd-refresh BEGIN"));
        assert!(body.contains("0 * * * * qmd update && qmd embed"));
        assert!(body.contains("# anvil-schedule:qmd-refresh END"));
        assert!(body.ends_with('\n'));
    }

    #[test]
    fn splice_block_appends_when_marker_absent() {
        let before = "0 5 * * * /usr/bin/backup\n";
        let after = splice_block(before, &s("qmd", "qmd update"));
        assert!(after.contains("/usr/bin/backup"));
        assert!(after.contains("# anvil-schedule:qmd BEGIN"));
        assert!(after.contains("# anvil-schedule:qmd END"));
        assert!(after.starts_with("0 5 * * * /usr/bin/backup"));
    }

    #[test]
    fn splice_block_replaces_when_marker_present() {
        let before = "\
# anvil-schedule:qmd BEGIN
0 * * * * old-command
# anvil-schedule:qmd END
0 5 * * * backup-after
";
        let after = splice_block(before, &s("qmd", "new-command"));
        assert!(!after.contains("old-command"));
        assert!(after.contains("0 * * * * new-command"));
        assert!(after.contains("backup-after"));
    }

    #[test]
    fn splice_block_idempotent_under_repeat() {
        let sch = s("qmd", "qmd update");
        let once = splice_block("", &sch);
        let twice = splice_block(&once, &sch);
        assert_eq!(once, twice);
    }

    #[test]
    fn splice_block_handles_body_without_trailing_newline() {
        let before = "0 5 * * * backup";
        let after = splice_block(before, &s("qmd", "x"));
        assert!(after.contains("0 5 * * * backup\n"));
        assert!(after.contains("# anvil-schedule:qmd BEGIN"));
    }

    #[test]
    fn strip_block_removes_managed_lines_only() {
        let before = "\
0 5 * * * backup
# anvil-schedule:qmd BEGIN
0 * * * * qmd update
# anvil-schedule:qmd END
30 5 * * * after
";
        let after = strip_block(before, "qmd");
        assert!(!after.contains("anvil-schedule:qmd"));
        assert!(!after.contains("0 * * * * qmd update"));
        assert!(after.contains("0 5 * * * backup"));
        assert!(after.contains("30 5 * * * after"));
    }

    #[test]
    fn strip_block_idempotent_when_marker_absent() {
        let before = "0 5 * * * backup\n";
        let after = strip_block(before, "qmd");
        assert_eq!(after, before);
    }

    #[test]
    fn has_block_reports_presence() {
        assert!(!has_block("", "qmd"));
        let body = "# anvil-schedule:qmd BEGIN\n0 * * * * x\n# anvil-schedule:qmd END\n";
        assert!(has_block(body, "qmd"));
        assert!(!has_block(body, "other"));
    }

    #[test]
    fn splice_then_strip_roundtrip_returns_original_when_starting_clean() {
        let before = "0 5 * * * backup\n";
        let sch = s("qmd", "qmd update");
        let spliced = splice_block(before, &sch);
        let stripped = strip_block(&spliced, "qmd");
        assert_eq!(stripped, before);
    }

    #[test]
    fn splice_uses_correct_cron_expression_for_daily() {
        let sch = Schedule::new("d", "x", Interval::Daily { hour: 3 }).unwrap();
        let body = splice_block("", &sch);
        assert!(body.contains("0 3 * * * x"));
    }
}
