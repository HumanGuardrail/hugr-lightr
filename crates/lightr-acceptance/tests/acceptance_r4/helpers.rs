use std::fs;

/// Create a minimal workspace with one file for run/snapshot exercises.
pub(super) fn tiny_workspace(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/hello.txt"), b"hello lightr\n").unwrap();
    fs::write(root.join("README.md"), b"# test workspace\n").unwrap();
}

/// Extract the `lightr-json: {…}` line from stderr bytes and parse the object.
/// Returns None if the sentinel line is absent.
pub(super) fn parse_run_json_from_stderr(stderr: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(stderr);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("lightr-json: ") {
            return serde_json::from_str(rest).ok();
        }
    }
    None
}

/// Parse a JSON array from stdout bytes.
pub(super) fn parse_json_array(stdout: &[u8]) -> serde_json::Value {
    serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "expected JSON array on stdout; parse error: {e}\nraw: {}",
            String::from_utf8_lossy(stdout)
        )
    })
}

/// Parse a JSON object from stdout bytes.
pub(super) fn parse_json_object(stdout: &[u8]) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "expected JSON object on stdout; parse error: {e}\nraw: {}",
            String::from_utf8_lossy(stdout)
        )
    });
    assert!(v.is_object(), "expected JSON object, got: {v}");
    v
}
