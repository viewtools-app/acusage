//! Runs the snapshot generator on a synthetic LiteLLM file and compares with the expected output.
//! Skipped when python3 is not installed.
use std::process::Command;

#[test]
fn generator_matches_expected_fixture() {
    let root = env!("CARGO_MANIFEST_DIR");
    let out = std::env::temp_dir().join(format!("acusage-litellm-{}.json", std::process::id()));
    let run = Command::new("python3")
        .args(["-I", &format!("{root}/scripts/update-litellm-prices.py")])
        .arg(format!("{root}/tests/fixtures/litellm/input.json"))
        .arg("--out")
        .arg(&out)
        .args(["--generated", "2026-01-01"])
        .output();
    let Ok(run) = run else {
        eprintln!("python3 not found, skipping");
        return;
    };
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let got = std::fs::read_to_string(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    let want =
        std::fs::read_to_string(format!("{root}/tests/fixtures/litellm/expected.json")).unwrap();
    assert_eq!(got, want);
}
