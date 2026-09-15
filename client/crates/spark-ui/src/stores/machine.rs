//! MachineStore: manages the list of available machines (local + SSH).

use gpui::{Context, Entity, EventEmitter};
use spark_model::*;

#[derive(Debug, Clone)]
pub enum MachineStoreEvent {
    MachineAdded(MachineId),
    MachineRemoved(MachineId),
    MachineStatusChanged(MachineId),
    MachineSelected(Option<MachineId>),
}

pub struct MachineStore {
    pub machines: Vec<Machine>,
    pub selected: Option<MachineId>,
    pub is_connecting: bool,
}

impl EventEmitter<MachineStoreEvent> for MachineStore {}

impl MachineStore {
    pub fn new() -> Self {
        // Start with the local machine
        let machines = vec![Machine::local(Some("Local".to_string()))];
        Self {
            machines,
            selected: Some(MachineId::local()),
            is_connecting: false,
        }
    }

    pub fn add_machine(&mut self, machine: Machine, cx: &mut Context<Self>) {
        let id = machine.id.clone();
        self.machines.push(machine);
        self.selected = Some(id.clone());
        cx.notify();
    }

    pub fn remove_machine(&mut self, machine_id: &MachineId, cx: &mut Context<Self>) {
        self.machines.retain(|m| &m.id != machine_id);
        if self.selected.as_ref() == Some(machine_id) {
            self.selected = self.machines.first().map(|m| m.id.clone());
        }
        cx.notify();
    }

    pub fn select(&mut self, id: Option<MachineId>, cx: &mut Context<Self>) {
        self.selected = id;
        cx.notify();
    }

    pub fn selected(&self) -> Option<&Machine> {
        self.selected
            .as_ref()
            .and_then(|id| self.machines.iter().find(|m| &m.id == id))
    }

    pub fn get(&self, id: &MachineId) -> Option<&Machine> {
        self.machines.iter().find(|m| &m.id == id)
    }

    pub fn update_machine(&mut self, machine: Machine, cx: &mut Context<Self>) {
        let id = machine.id.clone();
        if let Some(pos) = self.machines.iter().position(|m| m.id == id) {
            self.machines[pos] = machine;
        } else {
            self.machines.push(machine);
        }
        cx.notify();
    }

    pub fn list_machines(&self) -> Vec<Machine> {
        self.machines.clone()
    }
}
