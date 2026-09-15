//! AppState: top-level GPUI entity that owns all stores.
//!
//! This is the root of the Entity<T> ownership tree.
//! GPUI's App holds AppState as an entity, and views subscribe
//! to changes via Context<T> / cx.observe / cx.subscribe.
//!
//! AppState polls the transport for events and routes them to
//! the appropriate stores.

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, Task as GpuiTask,
};
use spark_model::*;
use spark_transport::{Transport, TransportCommand, TransportEvent};

use crate::stores::{*, MachineStore};

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum AppEvent {
    Refresh,
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

