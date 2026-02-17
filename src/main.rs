use std::fs;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, bail};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
struct TaskFile {
    id: String,
    subject: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "activeForm")]
    active_form: String,
    status: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    blocks: Vec<String>,
    #[serde(default, rename = "blockedBy")]
    blocked_by: Vec<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SafeClaimParams {
    #[schemars(description = "Task ID to claim")]
    task_id: String,
    #[schemars(description = "Agent name claiming the task")]
    owner: String,
    #[schemars(description = "Team name (defaults to first directory in ~/.claude/tasks/)")]
    team: Option<String>,
}

#[derive(Clone)]
struct SafeTaskClaim {
    tool_router: ToolRouter<Self>,
}

impl SafeTaskClaim {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    fn tasks_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".claude/tasks")
    }

    fn resolve_team(team: Option<&str>) -> anyhow::Result<String> {
        if let Some(t) = team {
            return Ok(t.to_string());
        }
        let tasks_dir = Self::tasks_dir();
        let entries = fs::read_dir(&tasks_dir)
            .with_context(|| format!("cannot read {}", tasks_dir.display()))?;
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    return Ok(name.to_string());
                }
            }
        }
        bail!("no team directories found in {}", tasks_dir.display());
    }

    fn do_claim(&self, params: SafeClaimParams) -> anyhow::Result<String> {
        let team = Self::resolve_team(params.team.as_deref())?;
        let team_dir = Self::tasks_dir().join(&team);
        if !team_dir.is_dir() {
            bail!("team directory not found: {}", team_dir.display());
        }

        let lock_path = team_dir.join(".lock");
        let task_path = team_dir.join(format!("{}.json", params.task_id));

        if !task_path.exists() {
            bail!("task file not found: {}", task_path.display());
        }

        let lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("cannot open lock: {}", lock_path.display()))?;

        lock_exclusive(&lock_file)?;
        let result = self.claim_under_lock(&task_path, &params.task_id, &params.owner);
        unlock(&lock_file)?;

        result
    }

    fn claim_under_lock(
        &self,
        task_path: &PathBuf,
        task_id: &str,
        owner: &str,
    ) -> anyhow::Result<String> {
        let content = fs::read_to_string(task_path)
            .with_context(|| format!("cannot read task {task_id}"))?;
        let mut task: TaskFile =
            serde_json::from_str(&content).with_context(|| format!("invalid JSON in task {task_id}"))?;

        if let Some(existing) = &task.owner {
            if !existing.is_empty() {
                bail!("already claimed by {existing}");
            }
        }

        match task.status.as_str() {
            "in_progress" => bail!("task is already in_progress"),
            "completed" => bail!("task is already completed"),
            "deleted" => bail!("task is deleted"),
            _ => {}
        }

        task.owner = Some(owner.to_string());
        task.status = "in_progress".to_string();

        let json = serde_json::to_string_pretty(&task)?;
        fs::write(task_path, json)
            .with_context(|| format!("cannot write task {task_id}"))?;

        Ok(format!("Claimed task {task_id}: {}", task.subject))
    }
}

fn lock_exclusive(file: &fs::File) -> anyhow::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if ret != 0 {
        bail!("flock failed: {}", io::Error::last_os_error());
    }
    Ok(())
}

fn unlock(file: &fs::File) -> anyhow::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let ret = unsafe { libc::flock(fd, libc::LOCK_UN) };
    if ret != 0 {
        bail!("flock failed: {}", io::Error::last_os_error());
    }
    Ok(())
}

#[tool_router]
impl SafeTaskClaim {
    #[tool(description = "Atomically claim a task with file locking. Rejects if already claimed, in_progress, or completed.")]
    async fn safe_claim(&self, Parameters(params): Parameters<SafeClaimParams>) -> String {
        match self.do_claim(params) {
            Ok(msg) => msg,
            Err(e) => format!("Error: {e}"),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SafeTaskClaim {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Safe task claiming with file locking. Use safe_claim before starting work on any task to prevent race conditions."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let service = SafeTaskClaim::new();
    let server = service.serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_team(dir: &std::path::Path, task_id: &str, status: &str, owner: Option<&str>) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(".lock"), "").unwrap();
        let task = TaskFile {
            id: task_id.to_string(),
            subject: "Test task".to_string(),
            description: "A test".to_string(),
            active_form: "Testing".to_string(),
            status: status.to_string(),
            owner: owner.map(|s| s.to_string()),
            blocks: vec![],
            blocked_by: vec![],
            metadata: None,
        };
        let json = serde_json::to_string_pretty(&task).unwrap();
        fs::write(dir.join(format!("{task_id}.json")), json).unwrap();
    }

    #[test]
    fn claim_pending_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let team_dir = tmp.path().join("test-team");
        setup_team(&team_dir, "1", "pending", None);

        let service = SafeTaskClaim::new();
        let result = service.claim_under_lock(
            &team_dir.join("1.json"),
            "1",
            "agent-a",
        );
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Claimed task 1"));

        let content = fs::read_to_string(team_dir.join("1.json")).unwrap();
        let task: TaskFile = serde_json::from_str(&content).unwrap();
        assert_eq!(task.owner.as_deref(), Some("agent-a"));
        assert_eq!(task.status, "in_progress");
    }

    #[test]
    fn claim_already_owned_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let team_dir = tmp.path().join("test-team");
        setup_team(&team_dir, "2", "pending", Some("agent-b"));

        let service = SafeTaskClaim::new();
        let result = service.claim_under_lock(
            &team_dir.join("2.json"),
            "2",
            "agent-a",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already claimed by agent-b"));
    }

    #[test]
    fn claim_in_progress_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let team_dir = tmp.path().join("test-team");
        setup_team(&team_dir, "3", "in_progress", None);

        let service = SafeTaskClaim::new();
        let result = service.claim_under_lock(
            &team_dir.join("3.json"),
            "3",
            "agent-a",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already in_progress"));
    }

    #[test]
    fn claim_completed_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let team_dir = tmp.path().join("test-team");
        setup_team(&team_dir, "4", "completed", None);

        let service = SafeTaskClaim::new();
        let result = service.claim_under_lock(
            &team_dir.join("4.json"),
            "4",
            "agent-a",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already completed"));
    }
}
