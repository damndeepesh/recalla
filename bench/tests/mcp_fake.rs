//! Runs the generic MCP adapter against the bundled fake server, end to end.

use std::process::Command;

#[test]
fn mcp_adapter_scores_a_real_server_process() {
    let bin = env!("CARGO_BIN_EXE_membench");
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixture.json");
    let out = Command::new(bin)
        .args(["run", "--suite", fixture, "--adapters", "none,mcp"])
        .args([
            "--mcp-command",
            &format!("{bin} fake-mcp"),
            "--mcp-name",
            "fake",
        ])
        .output()
        .expect("run membench");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let table = String::from_utf8_lossy(&out.stdout);
    let row = |name: &str, budget: &str| {
        table
            .lines()
            .find(|l| l.starts_with(&format!("| {name} | {budget} ")))
            .unwrap_or_else(|| panic!("no {name} row in:\n{table}"))
            .to_owned()
    };
    // The fake server finds the fact; the no-memory baseline cannot.
    assert!(
        row("fake", "1500").contains("| 1.00 |"),
        "{}",
        row("fake", "1500")
    );
    assert!(
        row("none", "1500").contains("| 0.00 |"),
        "{}",
        row("none", "1500")
    );
    // The budget cuts the reply: 500 tokens still fit one short fact.
    assert!(row("fake", "500").contains("| 1.00 |"));
}
