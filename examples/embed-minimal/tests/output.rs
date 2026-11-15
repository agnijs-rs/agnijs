//! Runs the example and checks what it prints, so the example in
//! docs/embedding-guide.md cannot rot.

use std::process::Command;

#[test]
fn the_example_prints_what_the_guide_shows() {
    let out = Command::new(env!("CARGO_BIN_EXE_embed-minimal"))
        .output()
        .expect("run embed-minimal");
    assert!(
        out.status.success(),
        "exit {:?}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n");
    let expected = "\
script: 1 + 2 = 3
host function: add(2, 40) = 42
console.log: double(21) is 42
module: answer = 42
jobs: ran 1, order script, job
exception: RangeError: negative at check (check.js:2)
";
    assert_eq!(stdout, expected);
}
