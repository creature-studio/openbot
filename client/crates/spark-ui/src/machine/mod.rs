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

use gpui::{div, px, prelude::*, IntoElement, SharedString};
use spark_model::{Machine, MachineId, MachineKind, MachineStatus};

use crate::stores::machine::{
    latency_label, status_icon, status_label, MachineAttention, MachineForm,
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
    ) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_1();

        for machine in machines {
            let is_selected = selected.as_ref() == Some(&machine.id);
            list = list.child(Self::render_sidebar_row(machine, is_selected));
        }

        list.child(
            div()
                .mt_1()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(gpui::rgb(0x9ca3af))
                .cursor_pointer()
                .hover(|d| d.text_color(gpui::rgb(0xffffff)))
                .child("+ Machine"),
        )
    }

    fn render_sidebar_row(machine: Machine, is_selected: bool) -> impl IntoElement {
        let status = machine.status.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .when(is_selected, |d| d.bg(gpui::rgb(0x1f2937)))
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
                    .text_color(gpui::rgb(0x6b7280))
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

        let mut body = div().flex().flex_col().gap_2().p_3();
        for (label, value) in rows {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
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

    pub fn render_add_form(form: MachineForm) -> impl IntoElement {
        let mut body = div()
            .w(px(420.0))
            .rounded_lg()
            .bg(gpui::rgb(0x1e293b))
            .border_1()
            .border_color(gpui::rgb(0x374151))
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

        body = body.child(field("name", &form.name, "devbox"));
        body = body.child(field("host / ip", &form.host, "10.0.0.42"));
        body = body.child(
            div()
                .flex()
                .gap_2()
                .child(div().flex_1().child(field("user", &form.user, "ubuntu")))
                .child(div().w(px(90.0)).child(field("port", &form.port, "22"))),
        );
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(gpui::rgb(0x9ca3af))
                .child(if form.use_ssh_config { "☑" } else { "☐" })
                .child("使用 ~/.ssh/config 别名（ProxyJump / IdentityFile 由 OpenSSH 处理）"),
        );
        if form.use_ssh_config {
            body = body.child(field("ssh alias", &form.ssh_config_host, "devbox"));
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

        body = body.child(
            div()
                .flex()
                .justify_end()
                .gap_3()
                .mt_3()
                .child(button("取消", 0x374151))
                .child(button("Test connection", 0x2563eb))
                .child(button("Add machine", 0x059669)),
        );

        body
    }

    // -----------------------------------------------------------------------
    // Host key confirmation
    // -----------------------------------------------------------------------

    /// Unknown host key → fingerprint + [取消] / [信任并连接].
    /// Changed host key → same fingerprint, but the wording makes clear this is
    /// a *different* key than known_hosts recorded.
    pub fn render_host_key_attention(attention: MachineAttention) -> impl IntoElement {
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

        body.child(
            div()
                .flex()
                .justify_end()
                .gap_3()
                .child(button("取消", 0x374151))
                .child(button(
                    if attention.changed {
                        "我确认，替换 known_hosts 条目"
                    } else {
                        "信任并连接"
                    },
                    0x059669,
                )),
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

fn field(label: &str, value: &str, placeholder: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(gpui::rgb(0x6b7280))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(gpui::rgb(0x0f172a))
                .text_sm()
                .text_color(gpui::rgb(0xe5e7eb))
                .child(SharedString::from(if value.is_empty() {
                    placeholder.to_string()
                } else {
                    value.to_string()
                })),
        )
}

fn button(label: &str, background: u32) -> impl IntoElement {
    div()
        .px_4()
        .py_2()
        .rounded_md()
        .bg(gpui::rgb(background))
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
