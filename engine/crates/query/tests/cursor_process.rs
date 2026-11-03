use std::process::Command;

#[test]
fn old_cursor_is_rejected_after_process_restart_and_snapshot_change() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("cursor.bin");
    let database = dir.path().join("db");
    for mode in ["issue", "verify"] {
        let result = Command::new(env!("CARGO_BIN_EXE_cursor-probe"))
            .arg(&database)
            .arg(&token)
            .arg(mode)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "mode={mode}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        if mode == "verify" {
            assert!(
                String::from_utf8_lossy(&result.stdout)
                    .contains("old_process_cursor_rejected=true")
            );
        }
    }
}
