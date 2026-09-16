//! spark-ui: GPUI-based UI for the Spark agent workbench.
//!
//! Architecture follows GPUI's Entity<T> ownership model:
//!
//! ```text
//! AppState (held by App)
//! ├── ConnectionStore
//! ├── BotStore
//! ├── TaskStore
//! │    ├── TaskEntity A
//! │    ├── TaskEntity B
//! │    └── TaskEntity C
//! ├── WorkbenchStore
//! ├── AttentionStore
//! ├── SettingsStore
//! └── MachineStore      (Machines sidebar, "+ Machine" form, Runtime tab)
//! ```
//!
//! Windows:
//!
//! ```text
//! MainWindow  (RootView)
//! ├── Sidebar        BOT / TASKS / WORKBENCH / MACHINES
//! ├── TaskTimeline
//! ├── Inspector      Browser / Files / Terminal / Runtime
//! └── Composer       goal + machine selector
//! ```
//!
//! The machine layer is invisible to the task views: a task on `devbox` is just
//! a task whose [`spark_model::TaskEntity`] carries a non-local `machine_id`.

pub mod stores;
pub mod sidebar;
pub mod timeline;
pub mod composer;
pub mod inspector;
pub mod terminal;
pub mod browser;
pub mod diff;
pub mod attention;
pub mod machine;
pub mod link;
pub mod app;

pub use app::{AppState, RootView};
pub use link::{push_event, send as send_command, take_events};
pub use machine::MachinePanel;
pub use stores::{
    ConnectionStore, BotStore, TaskStore, WorkbenchStore, AttentionStore, SettingsStore,
    MachineStore,
};
pub use stores::machine::{MachineAttention, MachineForm};

use gpui::AppContext as _;

/// Start the GPUI application.
///
/// The window owns [`AppState`]; every frame drains the host-agent link queue
/// ([`link::take_events`]) and routes the events into the stores, so the
/// sidebar's machine status, the Runtime tab and the host-key card all update
/// from the same stream the task timeline uses.
pub fn run_app() {
    gpui_platform::application().run(|cx: &mut gpui::App| {
        let state = AppState::new("host-agent", cx);

        // Commands the UI produced travel over the host-agent link. A plain OS
        // thread does the blocking receive so the UI thread never waits on it.
        if let Some(mut commands) = state.update(cx, |state, _| state.take_commands()) {
            std::thread::spawn(move || {
                while let Some(command) = commands.blocking_recv() {
                    link::send(command);
                }
            });
        }

        let state_for_window = state.clone();
        let window_bounds = Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
            gpui::point(gpui::px(100.0), gpui::px(100.0)),
            gpui::size(gpui::px(1280.0), gpui::px(800.0)),
        )));
        let options = gpui::WindowOptions {
            window_bounds,
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Spark".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            app_id: Some("spark".into()),
            focus: true,
            show: true,
            window_decorations: Some(gpui::WindowDecorations::Server),
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            ..Default::default()
        };
        match cx.open_window(options, move |_window, cx| {
            cx.new(|cx| RootView::new(state_for_window.clone(), cx))
        }) {
            Ok(_) => {
                cx.activate(true);
            }
            Err(e) => {
                tracing::error!("could not open the Spark window: {e:#}");
            }
        }
    });
}
