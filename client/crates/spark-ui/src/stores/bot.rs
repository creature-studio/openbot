//! BotStore: manages the list of bots.

use gpui::{Context, EventEmitter};
use spark_model::{Bot, BotId};

#[derive(Debug, Clone)]
pub enum BotEvent {
    ListUpdated,
    Selected(Option<BotId>),
}

pub struct BotStore {
    pub bots: Vec<Bot>,
    pub selected: Option<BotId>,
}

impl EventEmitter<BotEvent> for BotStore {}

impl BotStore {
    pub fn new() -> Self {
        Self {
            bots: Vec::new(),
            selected: None,
        }
    }

    pub fn update_bots(&mut self, bots: Vec<Bot>, cx: &mut Context<Self>) {
        self.bots = bots;
        cx.notify();
    }

    pub fn select(&mut self, id: Option<BotId>, cx: &mut Context<Self>) {
        self.selected = id;
        cx.notify();
    }

    pub fn selected_bot(&self) -> Option<&Bot> {
        self.selected
            .as_ref()
            .and_then(|id| self.bots.iter().find(|b| &b.id == id))
    }
}
