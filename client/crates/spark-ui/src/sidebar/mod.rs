//! Sidebar view: Bot selector, Task list, Workbench list, Machines list.
//!
//! Layout:
//! ```text
//! ┌──────────────┐
//! │ BOT          │
//! │ ● Coding Bot │
//! │              │
//! │ TASKS        │
//! │ ▶ Login Bug  │
//! │ ✓ API Fix    │
//! │ ! Deploy     │
//! │              │
//! │ WORKBENCH    │
//! │ Project A    │
//! │              │
//! │ MACHINES     │
//! │ ● localhost  │
//! │ ▶ prod-01    │
//! └──────────────┘
//! ```

use gpui::{
    div, prelude::*, Context, Entity, IntoElement, Render, SharedString,
    Stateful, WindowContext,
};
use crate::stores::{BotStore, TaskStore, WorkbenchStore, MachineStore};
use spark_model::{TaskStatus, MachineStatus};

pub struct Sidebar {
    bots: Entity<BotStore>,
    tasks: Entity<TaskStore>,
    workbenches: Entity<WorkbenchStore>,
    machines: Entity<MachineStore>,
}

