/// Agent management system for Anvil CLI.
///
/// Each agent is a sub-task that runs in its own OS thread with an isolated
/// provider client and conversation context.  Agents do NOT share history
/// with the main session; their results are summarised and injected into the
/// main conversation when they complete.
///
/// The `AgentManager` is owned by `LiveCli` and polled every TUI frame.
/// Completed agent results are returned from `poll()` so the caller can inject
/// them as system messages.
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Category / role of an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentType {
    General,
    Explore,
    Plan,
    BackendExpert,
    FrontendExpert,
    SecurityExpert,
    DevopsExpert,
    Custom(String),
}

impl AgentType {
    /// Short display label used in the panel.
    pub fn label(&self) -> &str {
        match self {
            AgentType::General => "general",
            AgentType::Explore => "explore",
            AgentType::Plan => "plan",
            AgentType::BackendExpert => "backend",
            AgentType::FrontendExpert => "frontend",
            AgentType::SecurityExpert => "security",
            AgentType::DevopsExpert => "devops",
            AgentType::Custom(s) => s.as_str(),
        }
    }

    /// Parse a string into an `AgentType`.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "explore" => AgentType::Explore,
            "plan" => AgentType::Plan,
            "backend" | "backend-expert" => AgentType::BackendExpert,
            "frontend" | "frontend-expert" => AgentType::FrontendExpert,
            "security" | "security-expert" => AgentType::SecurityExpert,
            "devops" | "devops-expert" => AgentType::DevopsExpert,
            "general" => AgentType::General,
            other => AgentType::Custom(other.to_string()),
        }
    }
}

/// Lifecycle state of an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Still executing.
    Running,
    /// Finished successfully.
    Completed,
    /// Terminated with an error message.
    Failed(String),
    /// Queued but not yet started.
    Waiting,
}

impl AgentStatus {
    /// Single-character icon for TUI display.
    pub fn icon(&self) -> &'static str {
        match self {
            AgentStatus::Running => "⟳",
            AgentStatus::Completed => "✓",
            AgentStatus::Failed(_) => "✗",
            AgentStatus::Waiting => "·",
        }
    }
}

/// Final result returned from a completed agent thread.
#[derive(Debug)]
pub struct AgentResult {
    pub output: String,
    pub success: bool,
    pub duration: Duration,
}

/// Line-by-line output message sent from an agent thread to the manager.
#[derive(Debug)]
enum AgentMsg {
    /// One line of captured output.
    Line(String),
    /// Agent finished (may be success or failure).
    Done(AgentResult),
}

// ─── AgentHandle ──────────────────────────────────────────────────────────────

/// Live handle to a spawned agent, kept in `AgentManager::agents`.
pub struct AgentHandle {
    pub id: usize,
    pub name: String,
    pub agent_type: AgentType,
    pub status: AgentStatus,
    /// One-line description of the current task.
    pub task: String,
    /// Accumulated output lines from the agent.
    pub output: Vec<String>,
    /// Wall-clock start time.
    pub started_at: Instant,
    /// Channel from which to receive incremental output / done signal.
    rx: Option<Receiver<AgentMsg>>,
    /// OS thread handle (take()n when the thread completes).
    _thread: Option<JoinHandle<()>>,
}

impl AgentHandle {
    /// Drain any pending messages from the agent thread without blocking.
    /// Returns `Some(AgentResult)` when the agent signals completion.
    fn poll(&mut self) -> Option<AgentResult> {
        let rx = self.rx.as_ref()?;
        loop {
            match rx.try_recv() {
                Ok(AgentMsg::Line(line)) => {
                    self.output.push(line);
                    // Keep a rolling window to avoid unbounded memory growth.
                    if self.output.len() > 2000 {
                        self.output.drain(0..200);
                    }
                }
                Ok(AgentMsg::Done(result)) => {
                    self.rx = None;
                    return Some(result);
                }
                Err(_) => return None,
            }
        }
    }

    /// Elapsed time since the agent started.
    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Formatted elapsed string for the TUI panel (e.g. "12s", "3m").
    pub fn elapsed_str(&self) -> String {
        let s = self.started_at.elapsed().as_secs();
        if s < 60 {
            format!("{s}s")
        } else {
            format!("{}m{}s", s / 60, s % 60)
        }
    }
}

// ─── AgentManager ─────────────────────────────────────────────────────────────

/// Owns all agents for the current session.
pub struct AgentManager {
    agents: Vec<AgentHandle>,
    next_id: usize,
}

impl AgentManager {
    pub fn new() -> Self {
        Self {
            agents: Vec::new(),
            next_id: 1,
        }
    }

    /// Spawn a new agent thread.
    ///
    /// `task` is a one-line description shown in the TUI panel.
    /// `runner` is a closure that will be called inside the new thread; it
    /// receives a `SyncSender<AgentMsg>` to report progress.  The closure
    /// must return an `AgentResult`.
    pub fn spawn<F>(
        &mut self,
        name: impl Into<String>,
        agent_type: AgentType,
        task: impl Into<String>,
        runner: F,
    ) -> usize
    where
        F: FnOnce(AgentOutputSender) -> AgentResult + Send + 'static,
    {
        let id = self.next_id;
        self.next_id += 1;

        let (tx, rx) = mpsc::sync_channel::<AgentMsg>(1024);
        let sender = AgentOutputSender(tx.clone());
        let tx_done = tx;

        let thread = thread::Builder::new()
            .name(format!("anvil-agent-{id}"))
            .spawn(move || {
                let result = runner(sender);
                let _ = tx_done.send(AgentMsg::Done(result));
            })
            .ok();

        self.agents.push(AgentHandle {
            id,
            name: name.into(),
            agent_type,
            status: AgentStatus::Running,
            task: task.into(),
            output: Vec::new(),
            started_at: Instant::now(),
            rx: Some(rx),
            _thread: thread,
        });

        id
    }

    /// Poll all running agents for new output / completion events.
    ///
    /// Returns a list of `(id, AgentResult)` pairs for agents that just
    /// completed in this poll cycle.  The caller should inject those results
    /// into the main conversation.
    pub fn poll(&mut self) -> Vec<(usize, AgentResult)> {
        let mut completed = Vec::new();
        for handle in &mut self.agents {
            if handle.status != AgentStatus::Running && handle.status != AgentStatus::Waiting {
                continue;
            }
            if let Some(result) = handle.poll() {
                handle.status = if result.success {
                    AgentStatus::Completed
                } else {
                    AgentStatus::Failed(result.output.lines().last().unwrap_or("").to_string())
                };
                completed.push((handle.id, result));
            }
        }
        completed
    }

    /// Return a slice of all agents (running + completed + failed).
    pub fn agents(&self) -> &[AgentHandle] {
        &self.agents
    }

    /// Number of agents currently in `Running` or `Waiting` state.
    pub fn active_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| matches!(a.status, AgentStatus::Running | AgentStatus::Waiting))
            .count()
    }

    /// Total agents ever registered.
    pub fn total_count(&self) -> usize {
        self.agents.len()
    }

    /// Find an agent by ID, returning a mutable reference.
    pub fn get_mut(&mut self, id: usize) -> Option<&mut AgentHandle> {
        self.agents.iter_mut().find(|a| a.id == id)
    }

    /// Find an agent by ID, returning a shared reference.
    pub fn get(&self, id: usize) -> Option<&AgentHandle> {
        self.agents.iter().find(|a| a.id == id)
    }

    /// Mark a running agent as failed (used by the `/agents stop` command).
    pub fn stop(&mut self, id: usize) -> bool {
        if let Some(handle) = self.agents.iter_mut().find(|a| a.id == id) {
            if matches!(handle.status, AgentStatus::Running | AgentStatus::Waiting) {
                handle.status = AgentStatus::Failed("stopped by user".to_string());
                handle.rx = None;
                return true;
            }
        }
        false
    }

    /// Remove all agents that are in a terminal state (Completed or Failed).
    pub fn clear_completed(&mut self) {
        self.agents
            .retain(|a| matches!(a.status, AgentStatus::Running | AgentStatus::Waiting));
    }

    /// Format a human-readable summary for the `/agents` command.
    pub fn format_list(&self) -> String {
        if self.agents.is_empty() {
            return "No agents active or recently completed.".to_string();
        }
        let mut out = String::new();
        for a in &self.agents {
            let elapsed = a.elapsed_str();
            let icon = a.status.icon();
            let status_str = match &a.status {
                AgentStatus::Running => "running".to_string(),
                AgentStatus::Completed => "completed".to_string(),
                AgentStatus::Failed(msg) => format!("failed: {msg}"),
                AgentStatus::Waiting => "waiting".to_string(),
            };
            out.push_str(&format!(
                "  {icon} #{:02}  {:<12}  {:<40}  {}  ({})\n",
                a.id,
                a.agent_type.label(),
                truncate(&a.task, 40),
                elapsed,
                status_str,
            ));
        }
        out
    }

    /// Format the full output of a single agent.
    pub fn format_output(&self, id: usize) -> String {
        match self.get(id) {
            None => format!("No agent with id {id}."),
            Some(a) => {
                if a.output.is_empty() {
                    format!("Agent #{id} ({}) — no output captured yet.", a.agent_type.label())
                } else {
                    a.output.join("\n")
                }
            }
        }
    }

    /// Handle `/agents [subcommand]` input.  Returns a message to push into
    /// the TUI log.
    pub fn handle_command(&mut self, args: Option<&str>) -> String {
        let args = args.unwrap_or("").trim();
        if args.is_empty() || args == "list" {
            return self.format_list();
        }
        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();
        match sub {
            "view" => {
                let id = rest.parse::<usize>().unwrap_or(0);
                self.format_output(id)
            }
            "stop" => {
                let id = rest.parse::<usize>().unwrap_or(0);
                if self.stop(id) {
                    format!("Agent #{id} stopped.")
                } else {
                    format!("Agent #{id} not found or not running.")
                }
            }
            "clear" => {
                let before = self.agents.len();
                self.clear_completed();
                let after = self.agents.len();
                format!("Cleared {} completed agent(s).", before - after)
            }
            _ => {
                "Usage: /agents [list | view <id> | stop <id> | clear]".to_string()
            }
        }
    }
}

impl Default for AgentManager {
    fn default() -> Self {
        Self::new()
    }
}

// ─── AgentOutputSender ────────────────────────────────────────────────────────

/// A sender that agent runner closures use to emit incremental output.
#[derive(Clone)]
pub struct AgentOutputSender(SyncSender<AgentMsg>);

impl AgentOutputSender {
    /// Send one line of output.  Silently ignores send errors (receiver may
    /// have been stopped via `/agents stop`).
    pub fn send_line(&self, line: impl Into<String>) {
        let _ = self.0.send(AgentMsg::Line(line.into()));
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_and_poll_completes() {
        let mut mgr = AgentManager::new();
        let id = mgr.spawn("test", AgentType::General, "unit test task", |sender| {
            sender.send_line("hello from agent");
            AgentResult {
                output: "done".to_string(),
                success: true,
                duration: Duration::from_millis(1),
            }
        });
        assert_eq!(id, 1);

        // Give the thread time to run.
        thread::sleep(Duration::from_millis(50));

        let completed = mgr.poll();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].0, 1);
        assert!(completed[0].1.success);

        // Output should include the line we sent.
        let agent = mgr.get(id).unwrap();
        assert!(agent.output.contains(&"hello from agent".to_string()));
    }

    #[test]
    fn stop_agent() {
        let mut mgr = AgentManager::new();
        let id = mgr.spawn("slow", AgentType::General, "slow task", |_sender| {
            thread::sleep(Duration::from_secs(60));
            AgentResult {
                output: String::new(),
                success: true,
                duration: Duration::from_secs(60),
            }
        });
        assert!(mgr.stop(id));
        assert!(matches!(
            mgr.get(id).unwrap().status,
            AgentStatus::Failed(_)
        ));
    }

    #[test]
    fn agent_type_parse_roundtrip() {
        assert_eq!(AgentType::parse("backend").label(), "backend");
        assert_eq!(AgentType::parse("security-expert").label(), "security");
        assert_eq!(AgentType::parse("plan").label(), "plan");
        let custom = AgentType::parse("mycustomtype");
        assert_eq!(custom.label(), "mycustomtype");
    }
}
