use std::{path::Path, process::Command};
fn u32at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
// Strict extraction for our compiler-produced, single-function Mach-O object.
// Relocations and extra executable sections are rejected, never silently ignored.
fn extract(b: &[u8]) -> Vec<u8> {
    assert_eq!(u32at(b, 0), 0xfeedfacf);
    assert_eq!(u32at(b, 4), 0x01000007);
    assert_eq!(u32at(b, 12), 1);
    let mut at = 32;
    let mut text = None;
    for _ in 0..u32at(b, 16) {
        let cmd = u32at(b, at);
        let len = u32at(b, at + 4) as usize;
        assert!(len >= 8 && at + len <= b.len());
        if cmd == 0x19 {
            for n in 0..u32at(b, at + 64) as usize {
                let section = at + 72 + n * 80;
                let name = &b[section..section + 16];
                let size = u64at(b, section + 40) as usize;
                if name.starts_with(b"__text\0") {
                    assert!(text.is_none());
                    assert_eq!(u64at(b, section + 32), 0);
                    assert_eq!(
                        u32at(b, section + 60),
                        0,
                        "text relocations require linking"
                    );
                    let offset = u32at(b, section + 48) as usize;
                    text = Some(b[offset..offset + size].to_vec());
                } else {
                    assert_eq!(
                        u32at(b, section + 64) & 0x80000400,
                        0,
                        "extra executable section"
                    );
                }
            }
        }
        at += len;
    }
    text.expect("text section")
}
pub fn compile(source: &Path) -> Vec<u8> {
    let object =
        std::env::temp_dir().join(format!("macps5-compiled-integer-{}.o", std::process::id()));
    let output = Command::new("rustc")
        .arg(source)
        .args([
            "--edition=2024",
            "--crate-type=lib",
            "--emit=obj",
            "--target=x86_64-apple-darwin",
            "-Copt-level=2",
            "-Cpanic=abort",
            "-Coverflow-checks=off",
            "-Cdebuginfo=0",
            "-Dwarnings",
            "-o",
        ])
        .arg(&object)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = extract(&std::fs::read(&object).unwrap());
    std::fs::remove_file(object).unwrap();
    code
}
