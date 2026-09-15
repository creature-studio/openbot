use super::{Tool, ToolDefinition, ToolResult};

// Placeholder for computer use tools - Phase 3D
pub struct ComputerScreenshotTool;
pub struct ComputerClickTool;
pub struct ComputerTypeTool;

impl Tool for ComputerScreenshotTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.screenshot".to_string(),
            description: "Take desktop screenshot via X11 XShm.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{},"required":[]}"#.to_string(),
        }
    }
    fn execute(&self, _args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        Ok(ToolResult { content: "computer.screenshot not yet implemented - Phase 3D".to_string(), is_error: false })
    }
}

impl Tool for ComputerClickTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.click".to_string(),
            description: "Click at coordinates via XTest.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        Ok(ToolResult { content: format!("computer.click not yet implemented, args: {}", args), is_error: false })
    }
}

impl Tool for ComputerTypeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "computer.type".to_string(),
            description: "Type text via XTest.".to_string(),
            parameters_schema: r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#.to_string(),
        }
    }
    fn execute(&self, args: &str, _runtime_id: &str) -> Result<ToolResult, String> {
        Ok(ToolResult { content: format!("computer.type not yet implemented, args: {}", args), is_error: false })
    }
}
