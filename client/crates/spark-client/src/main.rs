//! Spark GPUI Client
//!
//! A native Rust AI agent workbench built on GPUI (Zed's GPU-accelerated UI).
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │ Spark                                      Connected ●    ⌘K   │
//! ├──────────────┬──────────────────────────────┬───────────────────┤
//! │              │                              │                   │
//! │ BOT          │     修复登录页面              │   Browser         │
//! │ ● Coding Bot │                              │                   │
//! │              │ 用户：                       │  localhost:3000   │
//! │ TASKS        │ 帮我修复登录异常              │                   │
//! │              │                              │  [browser view]   │
//! │ ▶ Login Bug  │ ● 正在工作                   │                   │
//! │ ✓ API Fix    │                              ├───────────────────┤
//! │ ! Deploy     │ ▼ 搜索文件        16ms       │  Files            │
//! │              │   src/login...               │                   │
//! │ WORKBENCH    │                              │  Changes           │
//! │              │ ▼ 读取文件        3ms        │                   │
//! │ Project A    │                              │  Terminal          │
//! │              │ ▼ 修改文件        12ms       │                   │
//! │              │   +12 -3                     │                   │
//! │              │                              │                   │
//! │              │ ▶ cargo test                 │                   │
//! │              │                              │                   │
//! ├──────────────┴──────────────────────────────┴───────────────────┤
//! │  Ask Spark...                                      Stop   Send  │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

mod app;

use gpui::prelude::*;
use spark_ui::app::AppState;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("spark_client=debug".parse().unwrap()),
        )
        .init();

    tracing::info!("Spark client starting");

    gpui::Application::new().run(|cx| {
        // Initialize AppState as a root entity.
        let app_state = AppState::new(cx);

        // Create the main window.
        cx.open_window(
            gpui::WindowOptions {
                title: Some("Spark".into()),
                bounds: gpui::WindowBounds::Fixed(gpui::Bounds {
                    origin: gpui::Point::new(100.0, 100.0),
                    size: gpui::Size {
                        width: 1440.0,
                        height: 900.0,
                    },
                }),
                ..Default::default()
            },
            |cx| {
                cx.new_view(|_cx| MainWindow {
                    app: app_state.clone(),
                })
            },
        )
        .unwrap();
    });
}

// ---------------------------------------------------------------------------
// MainWindow: the root view
// ---------------------------------------------------------------------------

/// Root view that assembles all panels.
///
/// Layout:
/// ```text
/// ┌────────┬────────────────────┬──────────┐
/// │        │                    │          │
/// │Sidebar │   Timeline         │Inspector │
/// │        │                    │          │
/// │        │                    │          │
/// ├────────┴────────────────────┴──────────┤
/// │              Composer                   │
/// └────────────────────────────────────────┘
/// ```
struct MainWindow {
    app: gpui::Entity<AppState>,
}

impl gpui::Render for MainWindow {
    fn render(&mut self, cx: &mut gpui::ViewContext<Self>) -> impl IntoElement {
        use gpui::div;

        let app = self.app.read(cx);

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui::rgb(0x0f172a)) // dark bg
            .text_color(gpui::rgb(0xe2e8f0))
            .font_family("Inter")
            .font_size(14.0)
            // Top bar
            .child(render_top_bar(cx))
            // Main content: sidebar + timeline + inspector
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .overflow_hidden()
                    .child(spark_ui::sidebar::Sidebar::new(
                        app.bots.clone(),
                        app.tasks.clone(),
                        app.workbenches.clone(),
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(spark_ui::timeline::TaskTimeline::new(
                                app.tasks.clone(),
                                cx,
                            ))
                            .flex_1(),
                    )
                    .child(spark_ui::inspector::Inspector::new(
                        app.tasks.clone(),
                        cx,
                    )),
            )
            // Bottom composer
            .child(spark_ui::composer::Composer::new(
                app.tasks.clone(),
                app.connection.clone(),
                cx,
            ))
            // Attention overlay (rendered on top of everything)
            .child(spark_ui::attention::AttentionOverlay::new(
                app.attention.clone(),
                cx,
            ))
    }
}

fn render_top_bar(_cx: &mut gpui::WindowContext) -> impl IntoElement {
    use gpui::div;

    div()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_2()
        .bg(gpui::rgb(0x1e293b))
        .border_b_1()
        .border_color(gpui::rgb(0x2d3748))
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::BOLD)
                .child("Spark"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui::rgb(0x10b981))
                        .child("Connected ●"),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(gpui::rgb(0x2d3748))
                        .text_xs()
                        .text_color(gpui::rgb(0x9ca3af))
                        .cursor_pointer()
                        .child("⌘K"),
                ),
        )
}
