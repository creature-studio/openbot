//! MachinePanel: rendering helpers for the three machine surfaces.
//!
//! ```text
//! MACHINES                    (sidebar)
//! ● Local        machine-local
//! ◐ devbox       10.0.0.42   38ms
//! ! old-box      10.0.0.7    需要确认
//! + Machine
//!
//! RUNTIME  (inspector tab)
//! host        devbox (10.0.0.42)
//! os          Ubuntu 24.04  kernel 6.8.0
//! cpu         8 cores   mem 31.2 GB
//! gpu         NVIDIA RTX 4090
//! sandd       0.1.0 (protocol 2)   latency 38ms
//! runtimes    rt-task-1  running  2 procs  1 pty
//!
//! + MACHINE (form)                HOST KEY (attention card)
//! name  [devbox      ]            SHA256:AbCdEf…
//! host  [10.0.0.42   ]            [取消]  [信任并连接]
//! user  [ubuntu      ] port [22]
//! [x] use ~/.ssh/config alias
//! [Test connection]  [Add machine]
//! ```
//!
//! These are `render_static` helpers (same convention as `BrowserPanel`): the
//! sidebar and the inspector own the entities, the drawing lives here.

use gpui::{div, px, prelude::*, Entity, FocusHandle, IntoElement, SharedString};
use spark_model::{Machine, MachineId, MachineKind, MachineStatus};

use crate::stores::machine::{
    latency_label, status_icon, status_label, MachineAttention, MachineForm, MachineFormField,
    MachineStore,
};

pub struct MachinePanel;

impl MachinePanel {
    // -----------------------------------------------------------------------
    // Sidebar: MACHINES section
    // -----------------------------------------------------------------------

    /// The machine list. `selected` draws the highlighted row.
    /// Owned inputs on purpose: the caller holds a GPUI read guard, and the
    /// returned element must not borrow from it.
    pub fn render_sidebar_list(
        machines: Vec<Machine>,
        selected: Option<MachineId>,
        store: Entity<MachineStore>,
    ) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_1();

        for machine in machines {
            let is_selected = selected.as_ref() == Some(&machine.id);
            list = list.child(Self::render_sidebar_row(machine, is_selected, store.clone()));
        }

        let add_store = store.clone();
        list.child(
            div()
                .mt_2()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(gpui::rgb(0x24324a))
                .text_xs()
                .text_color(gpui::rgb(0x60a5fa))
                .cursor_pointer()
                .hover(|d| d.bg(gpui::rgb(0x172554)))
                .id("add-machine")
                .on_click(move |_, _, cx| {
                    add_store.update(cx, |store, cx| store.open_form(cx));
                })
                .child("+ Machine"),
        )
    }

    fn render_sidebar_row(machine: Machine, is_selected: bool, store: Entity<MachineStore>) -> impl IntoElement {
        let status = machine.status.clone();
        let machine_id = machine.id.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .when(is_selected, |d| d.bg(gpui::rgb(0x1e293b)))
            .hover(|d| d.bg(gpui::rgb(0x172554)))
            .id(format!("machine-row-{}", machine_id.as_str()))
            .on_click(move |_, _, cx| {
                let id = machine_id.clone();
                store.update(cx, |store, cx| store.select(Some(id), cx));
            })
            .child(
                div()
                    .w(px(14.0))
                    .text_sm()
                    .text_color(gpui::rgb(status_color(&status)))
                    .child(status_icon(&status)),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(gpui::rgb(0xe5e7eb))
                    .child(SharedString::from(
                        machine
                            .display_name
                            .clone()
                            .unwrap_or_else(|| machine.name.clone()),
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(status_color(&status)))
                    .child(SharedString::from(if status.is_connected() {
                        latency_label(&machine)
                    } else {
                        status_label(&status).to_string()
                    })),
            )
    }

    // -----------------------------------------------------------------------
    // Inspector: RUNTIME tab
    // -----------------------------------------------------------------------

    /// Runtime inspector: host, OS, CPU, memory, GPU, sandd version and latency,
    /// plus the runtimes currently alive on that machine.
    pub fn render_runtime_inspector(
        machine: Option<Machine>,
        runtimes: Vec<spark_transport::RuntimeInfo>,
    ) -> impl IntoElement {
        let Some(machine) = machine else {
            return div()
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .text_color(gpui::rgb(0x6b7280))
                .child("No machine selected");
        };

        let metadata = machine.metadata.clone();
        let capabilities = machine.capabilities.clone();

        let rows: Vec<(String, String)> = vec![
            (
                "host".to_string(),
                match &machine.kind {
                    MachineKind::Local => "local (this machine)".to_string(),
                    MachineKind::Ssh { host, port, user, .. } => match user {
                        Some(user) => format!("{user}@{host}:{port}"),
                        None => format!("{host}:{port}"),
                    },
                },
            ),
            ("status".to_string(), status_label(&machine.status).to_string()),
            ("os".to_string(), metadata.os.clone().unwrap_or_else(|| "—".into())),
            (
                "kernel / arch".to_string(),
                format!(
                    "{} / {}",
                    metadata.kernel.clone().unwrap_or_else(|| "—".into()),
                    metadata.arch.clone().unwrap_or_else(|| "—".into())
                ),
            ),
            (
                "cpu".to_string(),
                metadata
                    .cpu_cores
                    .map(|cores| format!("{cores} cores"))
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "memory".to_string(),
                metadata
                    .memory_total
                    .map(format_bytes)
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "gpu".to_string(),
                metadata.gpu.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "sandd".to_string(),
                format!(
                    "{}  latency {}",
                    metadata.sandd_version.clone().unwrap_or_else(|| "—".into()),
                    latency_label(&machine)
                ),
            ),
            (
                "capabilities".to_string(),
                format!(
                    "{}{}{}{}",
                    flag("exec", capabilities.exec),
                    flag("pty", capabilities.pty),
                    flag("fs", capabilities.filesystem),
                    flag("browser", capabilities.browser),
                ) + &format!(
                    "{}{}{}",
                    flag("computer", capabilities.computer_use),
                    flag("desktop", capabilities.desktop),
                    flag("gpu", capabilities.gpu),
                ),
            ),
            (
                "uptime".to_string(),
                metadata
                    .uptime_seconds
                    .map(format_uptime)
                    .unwrap_or_else(|| "—".into()),
            ),
        ];

        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .h_full()
            .p_3()
            .bg(gpui::rgb(0x0b1220));
        for (label, value) in rows {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .bg(gpui::rgb(0x111c32))
                    .child(
                        div()
                            .w(px(88.0))
                            .text_xs()
                            .text_color(gpui::rgb(0x6b7280))
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(gpui::rgb(0xe5e7eb))
                            .child(SharedString::from(value)),
                    ),
            );
        }

        body = body.child(
            div()
                .mt_2()
                .text_xs()
                .text_color(gpui::rgb(0x6b7280))
                .child(SharedString::from(format!("runtimes ({})", runtimes.len()))),
        );

        for runtime in runtimes {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(gpui::rgb(0xe5e7eb))
                            .child(SharedString::from(runtime.id.clone())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(gpui::rgb(0x9ca3af))
                            .child(SharedString::from(format!(
                                "{} · {} procs · {} pty",
                                runtime.state, runtime.process_count, runtime.pty_count
                            ))),
                    ),
            );
        }

        body.child(
            div()
                .mt_2()
                .text_xs()
                .text_color(gpui::rgb(0x6b7280))
                .child(
                    "远程 machine 上的 runtime 断开 SSH 后仍然存活；重新连接后在这里恢复。",
                ),
        )
    }

    // -----------------------------------------------------------------------
    // "+ Machine" form
    // -----------------------------------------------------------------------

    pub fn render_add_form(
        form: MachineForm,
        store: Entity<MachineStore>,
        command_tx: tokio::sync::mpsc::UnboundedSender<spark_transport::TransportCommand>,
    ) -> impl IntoElement {
        let mut body = div()
            .w(px(420.0))
            .rounded_lg()
            .bg(gpui::rgb(0x111c32))
            .border_1()
            .border_color(gpui::rgb(0x24324a))
            .p_6()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .text_color(gpui::rgb(0xffffff))
                    .mb_2()
                    .child("+ Machine"),
            );

        body = body.child(field(
            "name",
            &form.name,
            "devbox",
            store.clone(),
            MachineFormField::Name,
            form.focus_handles.get(&MachineFormField::Name).cloned(),
            form.active_field == Some(MachineFormField::Name),
        ));
        body = body.child(field(
            "host / ip",
            &form.host,
            "10.0.0.42",
            store.clone(),
            MachineFormField::Host,
            form.focus_handles.get(&MachineFormField::Host).cloned(),
            form.active_field == Some(MachineFormField::Host),
        ));
        body = body.child(
            div()
                .flex()
                .gap_2()
                .child(div().flex_1().child(field(
                    "user",
                    &form.user,
                    "ubuntu",
                    store.clone(),
                    MachineFormField::User,
                    form.focus_handles.get(&MachineFormField::User).cloned(),
                    form.active_field == Some(MachineFormField::User),
                )))
                .child(div().w(px(90.0)).child(field(
                    "port",
                    &form.port,
                    "22",
                    store.clone(),
                    MachineFormField::Port,
                    form.focus_handles.get(&MachineFormField::Port).cloned(),
                    form.active_field == Some(MachineFormField::Port),
                ))),
        );
        let toggle_store = store.clone();
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(gpui::rgb(0x9ca3af))
                .cursor_pointer()
                .id("machine-form-ssh-config")
                .on_click(move |_, _, cx| {
                    toggle_store.update(cx, |store, cx| store.toggle_ssh_config(cx));
                })
                .child(if form.use_ssh_config { "☑" } else { "☐" })
                .child("使用 ~/.ssh/config 别名（ProxyJump / IdentityFile 由 OpenSSH 处理）"),
        );
        if form.use_ssh_config {
            body = body.child(field(
                "ssh alias",
                &form.ssh_config_host,
                "devbox",
                store.clone(),
                MachineFormField::SshConfigHost,
                form.focus_handles
                    .get(&MachineFormField::SshConfigHost)
                    .cloned(),
                form.active_field == Some(MachineFormField::SshConfigHost),
            ));
        }

        body = body.child(
            div()
                .text_xs()
                .text_color(gpui::rgb(0x6b7280))
                .child("Spark 只保存主机别名/名称/端口/用户名；私钥、密码永远不会写入数据库。"),
        );

        if let Some(error) = form.error() {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0xf59e0b))
                    .child(SharedString::from(error)),
            );
        }

        if form.testing {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0x60a5fa))
                    .child("正在测试连接（ssh host true）…"),
            );
        } else if let Some(test) = &form.test {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(if test.ok {
                        gpui::rgb(0x10b981)
                    } else {
                        gpui::rgb(0xf87171)
                    })
                    .child(SharedString::from(test.message.clone())),
            );
        }

        let cancel_store = store.clone();
        let test_store = store.clone();
        let add_store = store.clone();
        let test_tx = command_tx.clone();
        let add_tx = command_tx.clone();
        body = body.child(
            div()
                .flex()
                .justify_end()
                .gap_3()
                .mt_3()
                .child(button("取消", 0x374151).id("machine-form-cancel").on_click(move |_, _, cx| {
                    cancel_store.update(cx, |store, cx| store.close_form(cx));
                }))
                .child(button("Test connection", 0x2563eb).id("machine-form-test").on_click(move |_, _, cx| {
                    test_store.update(cx, |store, cx| store.test_connection(&test_tx, cx));
                }))
                .child(button("Add machine", 0x059669).id("machine-form-add").on_click(move |_, _, cx| {
                    add_store.update(cx, |store, cx| store.add_machine(&add_tx, cx));
                })),
        );

        body
    }

    // -----------------------------------------------------------------------
    // Host key confirmation
    // -----------------------------------------------------------------------

    /// Unknown host key → fingerprint + [取消] / [信任并连接].
    /// Changed host key → same fingerprint, but the wording makes clear this is
    /// a *different* key than known_hosts recorded.
    pub fn render_host_key_attention(
        attention: MachineAttention,
        store: Entity<MachineStore>,
        command_tx: tokio::sync::mpsc::UnboundedSender<spark_transport::TransportCommand>,
    ) -> impl IntoElement {
        let (border, title) = if attention.changed {
            (gpui::rgb(0xef4444), "⚠ 主机密钥已变更")
        } else {
            (gpui::rgb(0xf59e0b), "⚠ 未知主机密钥")
        };

        let mut body = div()
            .w(px(460.0))
            .rounded_lg()
            .bg(gpui::rgb(0x1e293b))
            .border_1()
            .border_color(border)
            .p_6()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .text_color(border)
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(gpui::rgb(0xe5e7eb))
                    .child(SharedString::from(format!(
                        "{} · {}",
                        attention.machine_id.as_str(),
                        attention.message
                    ))),
            );

        if let Some(fingerprint) = attention.fingerprint.clone() {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0x9ca3af))
                    .child("指纹（请与目标机器核对）"),
            );
            body = body.child(
                div()
                    .text_sm()
                    .p_3()
                    .rounded_md()
                    .bg(gpui::rgb(0x0f172a))
                    .child(SharedString::from(fingerprint)),
            );
        }

        if attention.changed {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(gpui::rgb(0xf87171))
                    .child("这台机器报告的密钥与 known_hosts 中记录的不同，可能存在中间人风险。"),
            );
        }

        let cancel_store = store.clone();
        let trust_store = store.clone();
        let trust_tx = command_tx.clone();
        body.child(
            div()
                .flex()
                .justify_end()
                .gap_3()
                .child(button("取消", 0x374151).id("host-key-cancel").on_click(move |_, _, cx| {
                    cancel_store.update(cx, |store, cx| store.cancel_attention(cx));
                }))
                .child(button(
                    if attention.changed {
                        "我确认，替换 known_hosts 条目"
                    } else {
                        "信任并连接"
                    },
                    0x059669,
                ).id("host-key-trust").on_click(move |_, _, cx| {
                    trust_store.update(cx, |store, cx| store.trust_and_connect(&trust_tx, cx));
                })),
        )
    }
}

// ---------------------------------------------------------------------------
// Small drawing helpers
// ---------------------------------------------------------------------------

/// `u32` rather than a colour type, so callers pass it straight to
/// `gpui::rgb(..)` and this file stays independent of GPUI's palette types.
fn status_color(status: &MachineStatus) -> u32 {
    match status {
        MachineStatus::Connected => 0x10b981,
        MachineStatus::Degraded => 0xf59e0b,
        MachineStatus::Connecting | MachineStatus::Bootstrapping => 0x60a5fa,
        MachineStatus::RequiresUserAction => 0xef4444,
        MachineStatus::Unreachable => 0x9ca3af,
        MachineStatus::Disconnected | MachineStatus::Error => 0x6b7280,
    }
}

fn field(
    label: &str,
    value: &str,
    placeholder: &str,
    store: Entity<MachineStore>,
    field: MachineFormField,
    focus_handle: Option<FocusHandle>,
    active: bool,
) -> impl IntoElement {
    let field_store = store.clone();
    let click_store = store;
    let field_id = format!("machine-form-field-{}", label.replace(' ', "-"));
    let click_focus = focus_handle.clone();
    let input = div()
        .id(field_id)
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            click_store.update(cx, |store, cx| {
                let cursor = store.form.field_len(field);
                store.form.active_field = Some(field);
                store.form.cursor = cursor;
                cx.notify();
            });
            if let Some(handle) = click_focus.as_ref() {
                window.focus(handle);
            }
        })
        .on_key_down(move |event, _, cx| {
            let key = event.keystroke.key.clone();
            let key_char = event.keystroke.key_char.as_deref().map(str::to_string);
            field_store.update(cx, |store, cx| {
                store.edit_form_field(field, &key, key_char.as_deref(), cx);
            });
        })
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(if active {
            gpui::rgb(0x3b82f6)
        } else {
            gpui::rgb(0x24324a)
        })
        .bg(gpui::rgb(0x0f172a))
        .hover(|d| d.border_color(gpui::rgb(0x3b82f6)))
        .text_sm()
        .text_color(if value.is_empty() {
            gpui::rgb(0x64748b)
        } else {
            gpui::rgb(0xe5e7eb)
        })
        .child(SharedString::from(if value.is_empty() {
            placeholder.to_string()
        } else {
            value.to_string()
        }));

    let input = if let Some(handle) = focus_handle {
        input.track_focus(&handle).into_any_element()
    } else {
        input.into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(gpui::rgb(0x94a3b8))
                .child(SharedString::from(label.to_string())),
        )
        .child(input)
}

fn button(label: &str, background: u32) -> gpui::Div {
    div()
        .px_4()
        .py_2()
        .rounded_md()
        .bg(gpui::rgb(background))
        .hover(|d| d.bg(gpui::rgb(background.saturating_add(0x101010))))
        .cursor_pointer()
        .text_sm()
        .child(SharedString::from(label.to_string()))
}

fn flag(name: &str, enabled: bool) -> String {
    format!("{}{}  ", if enabled { "✓" } else { "✗" }, name)
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let gib = bytes as f64 / GIB;
    if gib >= 1.0 {
        format!("{gib:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    if days > 0 {
        format!("{days}d {hours}h")
    } else {
        format!("{hours}h")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_and_uptime_formatting() {
        assert_eq!(format_bytes(32 * 1024 * 1024 * 1024), "32.0 GB");
        assert_eq!(format_bytes(512 * 1024 * 1024), "512 MB");
        assert_eq!(format_uptime(90_000), "1d 1h");
        assert_eq!(format_uptime(3_600), "1h");
    }

    #[test]
    fn capability_flags_are_readable() {
        let text = format!("{}{}", flag("exec", true), flag("gpu", false));
        assert!(text.contains("✓exec"));
        assert!(text.contains("✗gpu"));
    }
}
