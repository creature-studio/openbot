use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct AgentContext {
    pub cwd: String,
    pub env: HashMap<String, String>,
    pub runtime_id: String,
    pub capabilities: Vec<String>,
    pub permissions: HashMap<String, Permission>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Permission {
    Allow,
    Ask,
    Deny,
}

impl AgentContext {
    pub fn new(runtime_id: String) -> Self {
        let mut perms = HashMap::new();
        // Default permissions
        perms.insert("shell.exec".to_string(), Permission::Allow);
        perms.insert("file.read".to_string(), Permission::Allow);
        perms.insert("file.write".to_string(), Permission::Allow);
        perms.insert("file.list".to_string(), Permission::Allow);
        perms.insert("file.search".to_string(), Permission::Allow);
        perms.insert("terminal.open".to_string(), Permission::Allow);
        perms.insert("terminal.write".to_string(), Permission::Allow);
        perms.insert("browser.open".to_string(), Permission::Allow);
        perms.insert("browser.snapshot".to_string(), Permission::Allow);
        perms.insert("secret.read".to_string(), Permission::Ask);
        perms.insert("external.send".to_string(), Permission::Ask);
        perms.insert("destructive".to_string(), Permission::Ask);

        Self {
            cwd: "/workspace".to_string(),
            env: HashMap::new(),
            runtime_id,
            capabilities: vec![],
            permissions: perms,
        }
    }

    pub fn check_permission(&self, tool: &str) -> Permission {
        self.permissions.get(tool).cloned().unwrap_or(Permission::Ask)
    }
}
