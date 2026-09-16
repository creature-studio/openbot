//! Composer: the bottom input bar.
//!
//! The composer is deliberately machine-aware only through `MachineStore`: it
//! pins a newly-created task to the selected machine and sends every later
//! action to host-agent. It never opens SSH or talks to a runtime directly.

use gpui::{
    div, prelude::*, App, Context, Entity, FocusHandle, IntoElement, KeyDownEvent, Render, Window,
};
use spark_model::TaskId;
use spark_transport::TransportCommand;

use crate::stores::{MachineStore, TaskStore};
use crate::ConnectionStore;

pub struct Composer {
    tasks: Entity<TaskStore>,
    machines: Entity<MachineStore>,
    #[allow(dead_code)]
    connection: Entity<ConnectionStore>,
    command_tx: tokio::sync::mpsc::UnboundedSender<TransportCommand>,
    input_text: String,
    is_focused: bool,
    focus_handle: FocusHandle,
}

#[derive(Debug, Clone)]
pub enum ComposerEvent {
    Sent { task_id: TaskId, content: String },
    Stopped { task_id: TaskId },
}

impl Composer {
    pub fn new(
        tasks: Entity<TaskStore>,
        connection: Entity<ConnectionStore>,
        machines: Entity<MachineStore>,
        command_tx: tokio::sync::mpsc::UnboundedSender<TransportCommand>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&tasks, |_, _, cx| cx.notify()).detach();
        cx.observe(&machines, |_, _, cx| cx.notify()).detach();

        Self {
            tasks,
            machines,
            connection,
            command_tx,
            input_text: String::new(),
            is_focused: false,
            focus_handle: cx.focus_handle(),
        }
    }

    fn has_running_task(&self, cx: &App) -> bool {
        self.tasks
            .read(cx)
            .selected_task()
            .map(|t| t.status == spark_model::TaskStatus::Running)
            .unwrap_or(false)
    }

    fn can_send(&self) -> bool {
        !self.input_text.trim().is_empty()
    }

    /// GPUI's key event exposes both the physical key and the produced
    /// character. This gives the composer real editing behaviour while IME and
    /// clipboard support remain owned by GPUI's normal input pipeline.
    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt {
            return;
        }
        match event.keystroke.key.as_str() {
            "backspace" | "delete" => {
                self.input_text.pop();
                cx.notify();
            }
            "enter" => self.send(cx),
            _ => {
                let text = event
                    .keystroke
                    .key_char
                    .as_deref()
                    .filter(|text| !text.is_empty())
                    .or_else(|| {
                        (event.keystroke.key.chars().count() == 1)
                            .then_some(event.keystroke.key.as_str())
                    });
                if let Some(text) = text {
                    if !text.chars().any(|character| character.is_control()) {
                        self.input_text.push_str(text);
                        self.is_focused = true;
                        cx.notify();
                    }
                }
            }
        }
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let content = self.input_text.trim().to_string();
        if content.is_empty() {
            return;
        }

        let selected_task = self.tasks.read(cx).selected_task().map(|task| {
            (task.id.clone(), task.status.clone())
        });
        if let Some((task_id, status)) = selected_task {
            if status == spark_model::TaskStatus::Running {
                let _ = self.command_tx.send(TransportCommand::SendTaskMessage {
                    task_id: task_id.0,
                    content,
                });
            } else {
                let machine_id = self
                    .machines
                    .read(cx)
                    .selected
                    .clone()
                    .unwrap_or_else(spark_model::MachineId::local);
                let tx = self.command_tx.clone();
                self.tasks.update(cx, |tasks, cx| {
                    tasks.create_task_on(machine_id, content, &tx, cx);
                });
            }
        } else {
            let machine_id = self
                .machines
                .read(cx)
                .selected
                .clone()
                .unwrap_or_else(spark_model::MachineId::local);
            let tx = self.command_tx.clone();
            self.tasks.update(cx, |tasks, cx| {
                tasks.create_task_on(machine_id, content, &tx, cx);
            });
        }
        self.input_text.clear();
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.tasks.read(cx).selected_task().map(|task| task.id.0.clone()) else {
            return;
        };
        let _ = self.command_tx.send(TransportCommand::StopTask { task_id });
    }

    fn select_next_machine(&mut self, cx: &mut Context<Self>) {
        self.machines.update(cx, |machines, cx| machines.select_next(cx));
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_running = self.has_running_task(cx);
        let can_send = self.can_send();
        let machine_label = self
            .machines
            .read(cx)
            .selected()
            .map(|machine| machine.display_name.clone().unwrap_or_else(|| machine.name.clone()))
            .unwrap_or_else(|| "Local".into());

        let machine_selector = self.machines.clone();
        let focus_handle = self.focus_handle.clone();
        let focus_for_click = focus_handle.clone();
        let mut input = div()
            .flex_1()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(gpui::rgb(0x111c32))
            .border_1()
            .border_color(if self.is_focused {
                gpui::rgb(0x3b82f6)
            } else {
                gpui::rgb(0x2d3748)
            })
            .text_sm()
            .id("composer-input")
            .track_focus(&focus_handle)
            .on_click(move |_, window, cx| {
                window.focus(&focus_for_click, cx);
            })
            .on_key_down(cx.listener(Self::on_key_down));
        input = input.child(if self.input_text.is_empty() {
            div()
                .text_color(gpui::rgb(0x6b7280))
                .child("Ask Spark...")
        } else {
            div().child(self.input_text.clone())
        });

        let mut root = div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .bg(gpui::rgb(0x0f172a))
            .border_t_1()
            .border_color(gpui::rgb(0x1e293b))
            .child(input)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(gpui::rgb(0x111c32))
                    .border_1()
                    .border_color(gpui::rgb(0x24324a))
                    .text_xs()
                    .cursor_pointer()
                    .id("composer-machine-selector")
                    .on_click(move |_, _, cx| {
                        machine_selector.update(cx, |machines, cx| machines.select_next(cx));
                    })
                    .child(format!("machine: {} ▾", machine_label)),
            );

        if is_running {
            let stop_view = cx.entity();
            root = root.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(gpui::rgb(0x7f1d1d))
                    .cursor_pointer()
                    .text_sm()
                    .id("composer-stop")
                    .hover(|d| d.bg(gpui::rgb(0x991b1b)))
                    .on_click(move |_, _, cx| {
                        stop_view.update(cx, |composer, cx| composer.stop(cx));
                    })
                    .child("Stop"),
            );
        }

        let send_view = cx.entity();
        root.child(
            div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(if can_send {
                    gpui::rgb(0x3b82f6)
                } else {
                    gpui::rgb(0x1e293b)
                })
                .cursor_pointer()
                .text_sm()
                .id("composer-send")
                .when(!can_send, |d| d.opacity(0.5))
                .hover(|d| d.bg(gpui::rgb(0x2563eb)))
                .on_click(move |_, _, cx| {
                    send_view.update(cx, |composer, cx| composer.send(cx));
                })
                .child("Send"),
        )
    }
}
