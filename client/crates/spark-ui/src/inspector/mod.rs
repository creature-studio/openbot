//! Inspector: the right-side panel with tabs.
//!
//! ```text
//! ┌────────────────────────┐
//! │ Browser │ Files │ Term  │
//! ├────────────────────────┤
//! │                        │
//! │   (active tab content) │
//! │                        │
//! └────────────────────────┘
//! ```
//!
//! Tabs:
//! - Browser: screenshot stream + DOM snapshot + interaction overlay
//! - Files: changed files list + diff viewer
//! - Terminal: terminal output
//! - Runtime: which machine the selected runtime lives on, and that machine's
//!   host / OS / CPU / memory / GPU / sandd version / latency

use gpui::{
    div, prelude::*, Context, Entity, IntoElement, Render, ViewContext,
};
use crate::machine::MachinePanel;
use crate::stores::{MachineStore, TaskStore};
use crate::browser::BrowserPanel;
use crate::terminal::TerminalPanel;
use crate::diff::DiffPanel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    Browser,
    Files,
    Terminal,
    Runtime,
}

pub struct Inspector {
    tasks: Entity<TaskStore>,
    machines: Entity<MachineStore>,
    active_tab: InspectorTab,
}

impl Inspector {
    pub fn new(tasks: Entity<TaskStore>, machines: Entity<MachineStore>, cx: &mut ViewContext<Self>) -> Self {
        cx.observe(&tasks, |_, _, cx| cx.notify()).detach();
        cx.observe(&machines, |_, _, cx| cx.notify()).detach();

        // A task always has a machine, and the runtime tab is where the user
        // finds out which one (local or remote) it is.
        Self {
            tasks,
            machines,
            active_tab: InspectorTab::Browser,
        }
    }

    fn set_tab(&mut self, tab: InspectorTab, cx: &mut ViewContext<Self>) {
        self.active_tab = tab;
        cx.notify();
    }
}

impl Render for Inspector {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(360.0))
            .bg(cx.theme().colors().panel_background)
            .border_l_1()
            .border_color(cx.theme().colors().border)
            .child(self.render_tabs(cx))
            .child(self.render_content(cx))
    }
}

impl Inspector {
    fn render_tabs(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        div()
            .flex()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(self.tab_button("Browser", InspectorTab::Browser, cx))
            .child(self.tab_button("Files", InspectorTab::Files, cx))
            .child(self.tab_button("Terminal", InspectorTab::Terminal, cx))
            .child(self.tab_button("Runtime", InspectorTab::Runtime, cx))
    }

    fn tab_button(
        &self,
        label: &str,
        tab: InspectorTab,
        cx: &ViewContext<Self>,
    ) -> impl IntoElement {
        let is_active = self.active_tab == tab;
        div()
            .px_3()
            .py_2()
            .text_sm()
            .cursor_pointer()
            .when(is_active, |d| {
                d.border_b_2()
                    .border_color(gpui::rgb(0x3b82f6))
                    .text_color(gpui::rgb(0xffffff))
            })
            .when(!is_active, |d| d.text_color(gpui::rgb(0x9ca3af)))
            .hover(|d| d.text_color(gpui::rgb(0xffffff)))
            .child(label)
    }

    fn render_content(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let tasks = self.tasks.read(cx);
        let task = tasks.selected_task();

        match self.active_tab {
            InspectorTab::Browser => {
                div()
                    .flex_1()
                    .p_3()
                    .child(BrowserPanel::render_static(task))
            }
            InspectorTab::Files => {
                div()
                    .flex_1()
                    .p_3()
                    .child(DiffPanel::render_static(task))
            }
            InspectorTab::Terminal => {
                div()
                    .flex_1()
                    .p_3()
                    .child(TerminalPanel::render_static(task))
            }
            InspectorTab::Runtime => {
                // The machine of the *selected task's* runtime: for a task on
                // devbox this shows devbox, not whatever the sidebar highlights.
                let machines = self.machines.read(cx);
                let selected = task
                    .map(|task| task.machine_id.clone())
                    .or_else(|| machines.selected.clone());
                // Clone out of the read guard: the element outlives it.
                let machine = selected
                    .as_ref()
                    .and_then(|id| machines.get(id))
                    .cloned();
                let runtimes = selected
                    .as_ref()
                    .map(|id| machines.runtimes_on(id).to_vec())
                    .unwrap_or_default();
                div()
                    .flex_1()
                    .child(MachinePanel::render_runtime_inspector(machine, runtimes))
            }
        }
    }
}
