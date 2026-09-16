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
        // Start with one usable assistant so a fresh client is not a blank
        // shell. Bot creation is currently a client-side workspace concern;
        // task execution still goes through host-agent.
        let default = Bot {
            id: BotId("bot-default".to_string()),
            name: "Coding Bot".to_string(),
            model: "default".to_string(),
            system_prompt: None,
            session_ids: Vec::new(),
            runtime_ids: Vec::new(),
        };
        Self {
            selected: Some(default.id.clone()),
            bots: vec![default],
        }
    }

    pub fn create_default(&mut self, cx: &mut Context<Self>) -> BotId {
        let id = BotId::new();
        let number = self.bots.len() + 1;
        self.bots.push(Bot {
            id: id.clone(),
            name: format!("Bot {number}"),
            model: "default".to_string(),
            system_prompt: None,
            session_ids: Vec::new(),
            runtime_ids: Vec::new(),
        });
        self.selected = Some(id.clone());
        cx.notify();
        id
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
