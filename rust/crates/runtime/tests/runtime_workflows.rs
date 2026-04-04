//! Integration tests for runtime workflows that span multiple subsystems.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::task_registry::TaskRegistry;
use runtime::{
    validate_packet, ConfigLoader, HookRunner, LaneEvent, LaneEventBlocker, LaneFailureClass,
    RuntimeFeatureConfig, RuntimeHookConfig, TaskPacket, WorkerEventKind, WorkerRegistry,
    WorkerStatus,
};
use serde_json::json;

fn temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("runtime-{prefix}-{nanos}"))
}

#[test]
fn worker_boot_state_tracks_trust_resolution_and_ready_snapshot() {
    let registry = WorkerRegistry::new();
    let cwd = "/tmp/runtime-worker-boot-integration";
    let worker = registry.create(cwd, &[cwd.to_string()], true);

    let spawning = registry
        .observe(
            &worker.worker_id,
            "Do you trust the files in this folder?\nAllow and continue",
        )
        .expect("trust prompt should be observed");
    assert_eq!(spawning.status, WorkerStatus::Spawning);
    assert!(spawning.trust_gate_cleared);

    registry
        .observe(&worker.worker_id, "Ready for your input\n>")
        .expect("ready cue should be observed");

    let snapshot = registry
        .await_ready(&worker.worker_id)
        .expect("snapshot should be available");
    assert!(snapshot.ready);
    assert!(!snapshot.blocked);
    assert_eq!(snapshot.status, WorkerStatus::ReadyForPrompt);
    assert_eq!(snapshot.last_error, None);

    let stored = registry
        .get(&worker.worker_id)
        .expect("worker should exist");
    let event_kinds: Vec<_> = stored.events.iter().map(|event| event.kind).collect();
    assert_eq!(
        event_kinds,
        vec![
            WorkerEventKind::Spawning,
            WorkerEventKind::TrustRequired,
            WorkerEventKind::TrustResolved,
            WorkerEventKind::ReadyForPrompt,
        ]
    );
}

#[test]
fn lane_event_emission_serializes_prompt_misdelivery_failures() {
    let registry = WorkerRegistry::new();
    let worker = registry.create("/tmp/runtime-lane-event-integration", &[], true);
    registry
        .observe(&worker.worker_id, "Ready for input\n>")
        .expect("worker should become ready");
    registry
        .send_prompt(&worker.worker_id, Some("Run lane event parity checks"))
        .expect("prompt dispatch should succeed");

    let failed = registry
        .observe(
            &worker.worker_id,
            "% Run lane event parity checks\nzsh: command not found: Run",
        )
        .expect("prompt misdelivery should be classified");
    assert_eq!(failed.status, WorkerStatus::ReadyForPrompt);
    let failure = failed
        .last_error
        .as_ref()
        .expect("misdelivery should record a failure");

    let blocker = LaneEventBlocker {
        failure_class: LaneFailureClass::PromptDelivery,
        detail: failure.message.clone(),
    };
    let emitted = serde_json::to_value(
        LaneEvent::failed("2026-04-04T00:00:00Z", &blocker).with_data(json!({
            "worker_id": failed.worker_id,
            "attempts": failed.prompt_delivery_attempts,
            "replay_prompt_ready": failed.replay_prompt.is_some(),
        })),
    )
    .expect("lane event should serialize");

    assert_eq!(emitted["event"], json!("lane.failed"));
    assert_eq!(emitted["status"], json!("failed"));
    assert_eq!(emitted["failureClass"], json!("prompt_delivery"));
    assert_eq!(emitted["data"]["attempts"], json!(1));
    assert_eq!(emitted["data"]["replay_prompt_ready"], json!(true));
    assert!(emitted["detail"]
        .as_str()
        .expect("detail should be serialized")
        .contains("worker prompt landed in shell"));
}

#[test]
fn hook_merge_runs_deduped_commands_across_hook_stages() {
    let base = RuntimeHookConfig::new(
        vec!["printf 'base-pre'".to_string()],
        vec!["printf 'base-post'".to_string()],
        vec!["printf 'base-failure'".to_string()],
    );
    let overlay = RuntimeHookConfig::new(
        vec![
            "printf 'base-pre'".to_string(),
            "printf 'overlay-pre'".to_string(),
        ],
        vec!["printf 'overlay-post'".to_string()],
        vec![
            "printf 'base-failure'".to_string(),
            "printf 'overlay-failure'".to_string(),
        ],
    );
    let runner = HookRunner::from_feature_config(
        &RuntimeFeatureConfig::default().with_hooks(base.merged(&overlay)),
    );

    let pre = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);
    let post = runner.run_post_tool_use("Read", r#"{"path":"README.md"}"#, "ok", false);
    let failure = runner.run_post_tool_use_failure("Read", r#"{"path":"README.md"}"#, "boom");

    assert_eq!(
        pre.messages(),
        &["base-pre".to_string(), "overlay-pre".to_string()]
    );
    assert_eq!(
        post.messages(),
        &["base-post".to_string(), "overlay-post".to_string()]
    );
    assert_eq!(
        failure.messages(),
        &["base-failure".to_string(), "overlay-failure".to_string(),]
    );
}

#[test]
fn task_packet_roundtrip_survives_validation_and_registry_storage() {
    let packet = TaskPacket {
        objective: "Ship worker boot integration coverage".to_string(),
        scope: "runtime/tests".to_string(),
        repo: "claw-code-parity".to_string(),
        branch_policy: "origin/main only".to_string(),
        acceptance_tests: vec![
            "cargo test -p runtime --test runtime_workflows".to_string(),
            "cargo test --workspace".to_string(),
        ],
        commit_policy: "single verified commit".to_string(),
        reporting_contract: "print verification evidence and sha".to_string(),
        escalation_policy: "escalate only on destructive ambiguity".to_string(),
    };

    let serialized = serde_json::to_string(&packet).expect("packet should serialize");
    let decoded: TaskPacket = serde_json::from_str(&serialized).expect("packet should decode");
    let validated = validate_packet(decoded).expect("packet should validate");

    let registry = TaskRegistry::new();
    let task = registry
        .create_from_packet(validated.into_inner())
        .expect("packet-backed task should be created");
    registry
        .update(&task.task_id, "Keep runtime hook semantics stable")
        .expect("task update should succeed");
    registry
        .append_output(&task.task_id, "tests passed")
        .expect("task output should append");

    let stored = registry.get(&task.task_id).expect("task should be stored");
    assert_eq!(stored.prompt, packet.objective);
    assert_eq!(stored.description.as_deref(), Some("runtime/tests"));
    assert_eq!(stored.task_packet, Some(packet));
    assert_eq!(stored.messages.len(), 1);
    assert_eq!(
        stored.messages[0].content,
        "Keep runtime hook semantics stable"
    );
    assert_eq!(
        registry.output(&task.task_id).expect("output should exist"),
        "tests passed"
    );
}

#[test]
fn config_validation_rejects_invalid_hook_shapes_before_runtime_use() {
    let root = temp_dir("config-validation");
    let cwd = root.join("project");
    let home = root.join("home").join(".claw");
    fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
    fs::create_dir_all(&home).expect("home config dir");

    let bad_settings = cwd.join(".claw").join("settings.local.json");
    fs::write(
        home.join("settings.json"),
        r#"{"hooks":{"PreToolUse":["printf 'base'"]}}"#,
    )
    .expect("write base settings");
    fs::write(
        &bad_settings,
        r#"{"hooks":{"PreToolUse":["printf 'overlay'",42]}}"#,
    )
    .expect("write invalid local settings");

    let error = ConfigLoader::new(&cwd, &home)
        .load()
        .expect_err("invalid hook shapes should fail validation");
    let rendered = error.to_string();

    assert!(rendered.contains(&format!(
        "{}: hooks: field PreToolUse must contain only strings",
        bad_settings.display()
    )));
    assert!(!rendered.contains("merged settings.hooks"));

    fs::remove_dir_all(root).expect("cleanup temp dir");
}
