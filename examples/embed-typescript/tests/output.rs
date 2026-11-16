//! Runs the example and checks what it prints, so the TypeScript example in
//! docs/embedding-guide.md cannot rot.

use std::process::Command;

#[test]
fn the_example_prints_what_the_guide_shows() {
    let out = Command::new(env!("CARGO_BIN_EXE_embed-typescript"))
        .output()
        .expect("run embed-typescript");
    assert!(
        out.status.success(),
        "exit {:?}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n");
    let expected = "\
console.log: ada: 30 EUR
exception: RangeError: ada has 30 EUR
  at app/account.ts:8:7
  at app/main.ts:6:5
import failed: TypeError: app/broken.ts:2:3: `accessor` fields are not supported
  help: use a getter and a setter
";
    assert_eq!(stdout, expected);
}
