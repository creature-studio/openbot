//! WorkbenchStore: manages workbench runtimes.

use gpui::{Context, EventEmitter};
use spark_model::Workbench;

#[derive(Debug, Clone)]
pub enum WorkbenchEvent {
    ListUpdated,
    Selected(Option<String>),
}

pub struct WorkbenchStore {
    pub workbenches: Vec<Workbench>,
    pub selected: Option<String>,
}

impl EventEmitter<WorkbenchEvent> for WorkbenchStore {}

impl WorkbenchStore {
    pub fn new() -> Self {
        Self {
            workbenches: Vec::new(),
            selected: None,
        }
    }

    pub fn update(&mut self, workbenches: Vec<Workbench>, cx: &mut Context<Self>) {
        self.workbenches = workbenches;
        cx.notify();
    }

    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected = id;
        cx.notify();
    }
}
