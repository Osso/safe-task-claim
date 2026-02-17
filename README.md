# safe-task-claim

MCP server that provides atomic task claiming for Claude Code teams using `flock`.

## Problem

Claude Code's `TaskUpdate` tool has no atomic claim semantics. When multiple instances share a task list, one can overwrite another's claim. The manual workaround (TaskUpdate then TaskGet to verify) is racy.

## Solution

A single `safe_claim` tool that atomically:

1. Acquires an exclusive `flock` on `~/.claude/tasks/{team}/.lock`
2. Reads the task JSON file
3. Rejects if already claimed, in_progress, or completed
4. Sets `owner` and `status: "in_progress"`
5. Writes the file and releases the lock

## Parameters

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `task_id` | string | yes | Task ID to claim |
| `owner` | string | yes | Agent name claiming the task |
| `team` | string | no | Team name (defaults to first directory in `~/.claude/tasks/`) |

## Setup

Add to `~/.claude.json`:

```json
{
  "mcpServers": {
    "safe-task-claim": {
      "type": "stdio",
      "command": "cargo",
      "args": ["run", "--quiet", "--manifest-path", "/path/to/safe-task-claim/Cargo.toml"],
      "instructions": "Safe task claiming with file locking. Use safe_claim before starting work on any task."
    }
  }
}
```

## Usage

```
safe_claim(task_id: "1", owner: "agent-alpha", team: "my-team")
// => "Claimed task 1: Write unit tests"

safe_claim(task_id: "1", owner: "agent-beta", team: "my-team")
// => "Error: already claimed by agent-alpha"
```
