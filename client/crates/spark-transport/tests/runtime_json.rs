use sand_protocol::{MachineId, Runtime, RuntimeId, RuntimeKind, RuntimeState};

#[test]
fn persisted_runtime_round_trips_special_characters() {
    let runtime = Runtime {
        id: RuntimeId("runtime-1".into()),
        kind: RuntimeKind::Task,
        state: RuntimeState::Failed { reason: "a \"quoted\" error\nwith a backslash \\".into() },
        workspace: "/tmp/a\"b\\c".into(),
        cgroup_path: None,
        created_at_ms: 123,
        started_at_ms: None,
        capabilities: vec!["exec".into(), "a\"b\\c".into()],
        process_count: 0,
        pty_count: 0,
        machine_id: MachineId("machine-1".into()),
    };
    let line = runtime.to_json_line();
    assert!(!line.contains('\n'));
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["state"], runtime.state.as_str());
    assert_eq!(value["workspace"], runtime.workspace.to_str().unwrap());
    assert_eq!(value["caps"], serde_json::json!(runtime.capabilities));
    assert_eq!(value["started"], 0);
    assert_eq!(value["machine_id"], "machine-1");
}
