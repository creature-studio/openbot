//! TaskStore: manages all task entities.
//!
//! Each task has its own rich state including timeline, attention,
//! browser, terminals, and file changes. The TaskStore owns a
//! HashMap of TaskEntity and provides selection, creation, and
//! event routing.

use gpui::{Context, EventEmitter};
use spark_model::*;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// TaskEntity: the rich per-task state
// ---------------------------------------------------------------------------

/// A single task's complete UI state.
/// This is NOT a GPUI Entity itself — it's owned by TaskStore.
/// TaskStore is the Entity, and it contains TaskEntity values.
#[derive(Debug, Clone)]
pub struct TaskEntity {
    pub id: TaskId,
    pub goal: String,
    /// Machine this task's runtime lives on. Chosen when the task is created and
    /// never changed afterwards (architecture §二十: no handoff in v1).
    pub machine_id: MachineId,
    /// Runtime is created with the task and remains pinned to this id.
    /// Keeping it here lets runtime-addressed browser/computer frames route
    /// back to the owning task without introducing a remote task model.
    pub runtime_id: Option<String>,
    pub status: TaskStatus,

    pub timeline: Vec<TimelineItem>,

    pub attention: Option<Attention>,

    pub artifacts: Vec<String>,

    pub browser_url: Option<String>,
    pub browser_snapshot: Option<Vec<u8>>, // JPEG/WebP
    pub browser_actions: Vec<String>,

    pub terminal_ids: Vec<String>,

    pub file_changes: Vec<FileChange>,

    /// True while the machine of this task has no bridge. The task is paused,
    /// not failed — see [`TaskEntity::mark_connection_lost`].
    pub connection_lost: bool,

    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl TaskEntity {
    /// A task on the local machine (the default when the user does not choose).
    pub fn new(id: TaskId, goal: String) -> Self {
        Self::on_machine(MachineId::local(), id, goal)
    }

    /// A task whose runtime is created on `machine_id`.
    pub fn on_machine(machine_id: MachineId, id: TaskId, goal: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            id,
            goal,
            machine_id,
            runtime_id: None,
            status: TaskStatus::Pending,
            timeline: Vec::new(),
            attention: None,
            artifacts: Vec::new(),
            browser_url: None,
            browser_snapshot: None,
            browser_actions: Vec::new(),
            terminal_ids: Vec::new(),
            file_changes: Vec::new(),
            connection_lost: false,
            created_at: now,
            updated_at: now,
        }
    }

    /// The machine running this task lost its bridge.
    ///
    /// The task is **not** failed: the runtime, its PTYs and its processes are
    /// still alive on the remote machine (architecture §十五). The timeline
    /// gets a status line, and the agent loop resumes after a reconnect.
    pub fn mark_connection_lost(&mut self, status: &MachineStatus) -> bool {
        if self.connection_lost {
            return false;
        }
        self.connection_lost = true;
        self.updated_at = chrono::Utc::now();
        self.timeline.push(TimelineItem::Status(StatusItem {
            id: format!("conn-lost-{}", self.id.0),
            label: "连接中断".to_string(),
            detail: Some(format!(
                "机器 {} 状态：{}。远端 runtime / PTY 仍然存活，重连后自动恢复。",
                self.machine_id.as_str(),
                status.as_str()
            )),
            at: chrono::Utc::now(),
        }));
        true
    }

    /// Reconnect succeeded: tell the user the work was never lost.
    pub fn mark_reconnected(&mut self) -> bool {
        if !self.connection_lost {
            return false;
        }
        self.connection_lost = false;
        self.updated_at = chrono::Utc::now();
        self.timeline.push(TimelineItem::Status(StatusItem {
            id: format!("conn-back-{}", self.id.0),
            label: "已重新连接".to_string(),
            detail: Some(format!(
                "机器 {} 已恢复；runtime 及其中的进程仍在运行。",
                self.machine_id.as_str()
            )),
            at: chrono::Utc::now(),
        }));
        true
    }

    /// Find a tool item by call_id for updating output/status.
    pub fn find_tool_mut(&mut self, call_id: &str) -> Option<&mut ToolItem> {
        self.timeline.iter_mut().find_map(|item| {
            if let TimelineItem::Tool(t) = item {
                if t.call_id == call_id {
                    return Some(t);
                }
            }
            None
        })
    }

    /// Summary line for sidebar display.
    pub fn sidebar_summary(&self) -> String {
        let prefix = match &self.status {
            TaskStatus::Running => "●",
            TaskStatus::Completed => "✓",
            TaskStatus::Failed => "✗",
            TaskStatus::WaitingApproval | TaskStatus::ReadyForCheck => "!",
            TaskStatus::Pending => "○",
            TaskStatus::Cancelled => "⊘",
        };
        format!("{} {}", prefix, self.goal)
    }
}

// ---------------------------------------------------------------------------
// TaskStore
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TaskEvent {
    TaskAdded(TaskId),
    TaskUpdated(TaskId),
    Selected(Option<TaskId>),
    TimelineAppended(TaskId),
}

pub struct TaskStore {
    pub tasks: HashMap<TaskId, TaskEntity>,
    pub order: Vec<TaskId>, // display order
    pub selected: Option<TaskId>,
}

impl EventEmitter<TaskEvent> for TaskStore {}

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            order: Vec::new(),
            selected: None,
        }
    }

    /// Create a task on a machine: the machine selector in the composer sends
    /// this, and the task is pinned to that machine from then on.
    pub fn create_task_on(
        &mut self,
        machine_id: MachineId,
        goal: String,
        command_tx: &tokio::sync::mpsc::UnboundedSender<spark_transport::TransportCommand>,
        cx: &mut Context<Self>,
    ) -> TaskId {
        let task_id = TaskId(format!(
            "task-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let task = TaskEntity::on_machine(machine_id.clone(), task_id.clone(), goal.clone());
        self.add_task(task, cx);
        let _ = command_tx.send(spark_transport::TransportCommand::CreateTask { machine_id, goal });
        task_id
    }

    /// Pin the task to the runtime returned by host-agent.
    pub fn with_runtime(mut self, runtime_id: String) -> Self {
        self.runtime_id = Some(runtime_id);
        self
    }

    /// Machine of the selected task — what the Runtime inspector shows.
    pub fn selected_machine(&self) -> Option<&MachineId> {
        self.selected_task().map(|task| &task.machine_id)
    }

    pub fn add_task(&mut self, task: TaskEntity, cx: &mut Context<Self>) {
        let id = task.id.clone();
        self.order.push(id.clone());
        self.tasks.insert(id.clone(), task);
        self.selected = Some(id.clone());
        cx.notify();
    }

    pub fn select(&mut self, id: Option<TaskId>, cx: &mut Context<Self>) {
        self.selected = id;
        cx.notify();
    }

    pub fn selected_task(&self) -> Option<&TaskEntity> {
        self.selected
            .as_ref()
            .and_then(|id| self.tasks.get(id))
    }

    pub fn selected_task_mut(&mut self) -> Option<&mut TaskEntity> {
        self.selected
            .as_mut()
            .and_then(|id| self.tasks.get_mut(id))
    }

    pub fn get_mut(&mut self, id: &TaskId) -> Option<&mut TaskEntity> {
        self.tasks.get_mut(id)
    }

    /// Process a transport event and update the relevant task.
    pub fn handle_event(&mut self, event: &spark_transport::TransportEvent, cx: &mut Context<Self>) {
        use spark_transport::TransportEvent;

        match event {
            TransportEvent::TaskCreated {
                task_id,
                goal,
                machine_id,
                runtime_id,
            } => {
                let machine = machine_id.clone().unwrap_or_else(MachineId::local);
                let task = TaskEntity::on_machine(machine, TaskId(task_id.clone()), goal.clone());
                let task = runtime_id
                    .clone()
                    .map(|runtime_id| task.with_runtime(runtime_id))
                    .unwrap_or(task);
                self.add_task(task, cx);
            }

            TransportEvent::TaskStatusChanged { task_id, status } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    task.status = parse_task_status(status);
                    task.updated_at = chrono::Utc::now();
                    cx.notify();
                }
            }

            TransportEvent::UserMessage {
                task_id,
                message_id,
                content,
            } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    task.timeline.push(TimelineItem::UserMessage(UserMessage {
                        id: message_id.clone(),
                        content: content.clone(),
                        at: chrono::Utc::now(),
                    }));
                    task.updated_at = chrono::Utc::now();
                    cx.notify();
                }
            }

            TransportEvent::AssistantMessage {
                task_id,
                message_id,
                content,
                streaming,
            } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    task.timeline
                        .push(TimelineItem::AssistantMessage(AssistantMessage {
                            id: message_id.clone(),
                            content: content.clone(),
                            streaming: *streaming,
                            at: chrono::Utc::now(),
                        }));
                    task.updated_at = chrono::Utc::now();
                    cx.notify();
                }
            }

            TransportEvent::ToolStarted {
                task_id,
                call_id,
                tool_name,
                args_json,
            } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    let presentation = ToolPresentation::from_tool_name(tool_name);
                    let label = make_tool_label(tool_name, args_json);
                    task.timeline.push(TimelineItem::Tool(ToolItem {
                        id: format!("tool-{}", call_id),
                        call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        presentation,
                        label,
                        args_json: Some(args_json.clone()),
                        output: None,
                        status: ToolItemStatus::Running,
                        duration_ms: None,
                        error: None,
                        diff_summary: None,
                        at: chrono::Utc::now(),
                    }));
                    task.updated_at = chrono::Utc::now();
                    cx.notify();
                }
            }

            TransportEvent::ToolOutput {
                task_id,
                call_id,
                delta,
            } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    if let Some(tool) = task.find_tool_mut(call_id) {
                        match &mut tool.output {
                            Some(ref mut out) => out.push_str(delta),
                            None => tool.output = Some(delta.clone()),
                        }
                        // Don't notify for every stdout chunk — too noisy.
                        // The running tool card auto-refreshes on a timer.
                    }
                }
            }

            TransportEvent::ToolFinished {
                task_id,
                call_id,
                tool_name: _,
                status,
                result,
                duration_ms,
            } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    if let Some(tool) = task.find_tool_mut(call_id) {
                        tool.status = match status.as_str() {
                            "success" => ToolItemStatus::Success,
                            "error" => ToolItemStatus::Error,
                            "permission_required" => ToolItemStatus::PermissionRequired,
                            "cancelled" => ToolItemStatus::Cancelled,
                            _ => ToolItemStatus::Success,
                        };
                        tool.output = Some(result.clone());
                        tool.duration_ms = Some(*duration_ms);
                        task.updated_at = chrono::Utc::now();
                        cx.notify();
                    }
                }
            }

            TransportEvent::PermissionRequired {
                task_id,
                permission_id,
                tool_name,
                reason,
            } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    task.attention = Some(Attention::PermissionRequired {
                        tool: tool_name.clone(),
                        reason: reason.clone(),
                        detail: None,
                    });
                    task.timeline
                        .push(TimelineItem::Permission(PermissionItem {
                            id: permission_id.clone(),
                            tool_name: tool_name.clone(),
                            reason: reason.clone(),
                            detail: None,
                            resolved: None,
                            at: chrono::Utc::now(),
                        }));
                    task.status = TaskStatus::WaitingApproval;
                    task.updated_at = chrono::Utc::now();
                    cx.notify();
                }
            }

            TransportEvent::ReadyForCheck {
                task_id,
                summary,
                files_changed,
                tests_passed,
                browser_verified,
            } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    task.attention = Some(Attention::ReadyForCheck {
                        summary: summary.clone(),
                        files_changed: *files_changed,
                        tests_passed: *tests_passed,
                        browser_verified: *browser_verified,
                    });
                    task.timeline
                        .push(TimelineItem::ReadyForCheck(ReadyForCheckItem {
                            id: format!("rfc-{}", task_id),
                            summary: summary.clone(),
                            files_changed: *files_changed,
                            tests_passed: *tests_passed,
                            browser_verified: *browser_verified,
                            artifacts: task.artifacts.clone(),
                            resolved: None,
                            at: chrono::Utc::now(),
                        }));
                    task.status = TaskStatus::ReadyForCheck;
                    task.updated_at = chrono::Utc::now();
                    cx.notify();
                }
            }

            TransportEvent::BrowserScreenshot { task_id, frame } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    task.browser_snapshot = Some(frame.clone());
                    // Don't notify for every frame; use a timer-based refresh.
                }
            }

            TransportEvent::BrowserFrame { runtime_id, data, .. }
            | TransportEvent::ComputerFrame { runtime_id, data, .. } => {
                if let Some(task) = self
                    .tasks
                    .values_mut()
                    .find(|task| task.runtime_id.as_deref() == Some(runtime_id.as_str()))
                {
                    task.browser_snapshot = Some(data.clone());
                    task.updated_at = chrono::Utc::now();
                    cx.notify();
                }
            }

            TransportEvent::BrowserAction { task_id, action } => {
                if let Some(task) = self.tasks.get_mut(&TaskId(task_id.clone())) {
                    task.browser_actions.push(action.clone());
                }
            }

            TransportEvent::Error {
                task_id,
                message,
                recoverable,
            } => {
                let target_id = task_id
                    .as_ref()
                    .map(|id| TaskId(id.clone()))
                    .or_else(|| self.selected.clone());
                if let Some(tid) = target_id {
                    if let Some(task) = self.tasks.get_mut(&tid) {
                        task.timeline.push(TimelineItem::Error(ErrorItem {
                            id: format!("err-{}", uuid::Uuid::new_v4()),
                            message: message.clone(),
                            recoverable: *recoverable,
                            at: chrono::Utc::now(),
                        }));
                        task.updated_at = chrono::Utc::now();
                        cx.notify();
                    }
                }
            }

            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_task_status(s: &str) -> TaskStatus {
    match s {
        "pending" => TaskStatus::Pending,
        "running" => TaskStatus::Running,
        "waiting_approval" => TaskStatus::WaitingApproval,
        "ready_for_check" => TaskStatus::ReadyForCheck,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        _ => TaskStatus::Pending,
    }
}

fn make_tool_label(tool_name: &str, args_json: &str) -> Option<String> {
    // Extract a human-readable label from tool name + args.
    let args: serde_json::Value = serde_json::from_str(args_json).ok()?;
    match tool_name {
        "shell.exec" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|cmd| truncate(cmd, 60)),
        "file.read" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("读取 {}", p)),
        "file.list" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("列出 {}", p)),
        "file.search" => args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|q| format!("搜索 \"{}\"", q)),
        "file.patch" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| format!("修改 {}", p)),
        "browser.open" => args
            .get("url")
            .and_then(|v| v.as_str())
            .map(|u| format!("打开 {}", u)),
        "browser.snapshot" => Some("获取页面快照".to_string()),
        "browser.click" => args
            .get("selector")
            .and_then(|v| v.as_str())
            .map(|s| format!("点击 {}", s)),
        "browser.fill" => args
            .get("selector")
            .and_then(|v| v.as_str())
            .map(|s| format!("输入 {}", s)),
        "task.complete" => Some("任务完成".to_string()),
        _ => None,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
