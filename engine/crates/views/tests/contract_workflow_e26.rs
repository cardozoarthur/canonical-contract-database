#[path = "../examples/support/contract_workflow.rs"]
mod workflow;
#[test]
fn actual_artifact_checker_rejects_a_tampered_passed_claim() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("activation-request.json");
    std::fs::write(&artifact, workflow::REQUEST).unwrap();
    workflow::check_artifact(&artifact).unwrap();
    std::fs::write(&artifact, b"{\"passed\":true}").unwrap();
    assert!(
        workflow::check_artifact(&artifact).is_err(),
        "a passed claim cannot replace exact checked artifact bytes"
    );
}

#[test]
fn workflow_reopens_exports_and_rejects_false_evidence_and_stale_changes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let evidence = dir.path().join("evidence");
    let report = workflow::run(&db, &evidence).unwrap();
    assert!(report.contains("tasks=3 states=3"));
    workflow::verify_rejected_changes(&db).unwrap();
    workflow::verify_existing(&db, &evidence).unwrap();
    std::fs::write(evidence.join(workflow::ARTIFACT_FILE), b"{\"passed\":true}").unwrap();
    assert!(workflow::verify_existing(&db, &evidence).is_err());
    let registry = canonical_registry::Registry::open(&db).unwrap();
    assert_eq!(
        registry.decode_record(1, 601).unwrap()[1],
        Some(canonical_registry::Value::Enum(1))
    );
    drop(registry);
    std::fs::write(evidence.join(workflow::ARTIFACT_FILE), workflow::REQUEST).unwrap();
    workflow::verify_existing(&db, &evidence).unwrap();
    std::fs::write(evidence.join("tasks.csv"), b"fabricated").unwrap();
    assert!(workflow::verify_existing(&db, &evidence).is_err());
}

#[test]
fn artifact_equal_length_tamper_checks_content_not_only_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("activation-request.json");
    let mut bytes = workflow::REQUEST.to_vec();
    bytes[0] = b'[';
    assert_eq!(bytes.len(), workflow::REQUEST.len());
    std::fs::write(&artifact, &bytes).unwrap();
    assert!(workflow::check_artifact(&artifact).is_err());
}
