use native_ps5::{
    jit_memory::JitArena,
    loader::Image,
    module_executor::{Execution, NativeExecutor},
    modules::Arguments,
};
use std::{path::Path, process::Command, rc::Rc};
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
fn reference(seed: u64, rounds: u64) -> u64 {
    let modulus = 1u128 << 64;
    let mut value = u128::from(seed);
    for step in 0..rounds {
        value ^= (u128::from(step) * 0x9e3779b97f4a7c15) % modulus;
        let shift = (value >> 58) as u32;
        value = if shift == 0 {
            value
        } else {
            ((value << shift) % modulus) | (value >> (64 - shift))
        };
        value = (value * 0xbf58476d1ce4e5b9) % modulus;
        value ^= value >> 27;
    }
    value as u64
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let source = Path::new(args.get(1).expect("guest .rs path"));
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
    assert!(!code.is_empty() && code.len() < 0x1f00);
    let mut file = native_ps5::modules::fixture();
    file.resize(0x2000, 0);
    for at in [96, 104] {
        file[at..at + 8].copy_from_slice(&0x2000u64.to_le_bytes());
    }
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut image = Image::load(&file, 0x100000, 16384).unwrap();
    let mut executor = NativeExecutor::new(0x900000, 65536, 10000, 1000000).unwrap();
    let arena = Rc::new(unsafe { JitArena::new(4 * 1024 * 1024).unwrap() });
    unsafe {
        executor
            .register_direct_source(&image, 10000, arena.clone())
            .unwrap();
    }
    unsafe {
        executor.set_automatic_translation(true);
    }
    let mut checks = 0;
    let mut warmed = None;
    for pass in 0..2 {
        for rounds in [0, 1, 2, 3, 7, 8, 9, 31, 64, 255] {
            for seed in [
                0,
                1,
                u64::MAX,
                1 << 63,
                0x123456789abcdef0,
                0xaaaaaaaa55555555,
            ] {
                let result = executor
                    .run(
                        &mut image,
                        0x100100,
                        Arguments {
                            size: seed,
                            pointer: rounds,
                        },
                        &mut [],
                    )
                    .unwrap();
                let mixed = reference(seed, rounds);
                let expected = (mixed ^ (mixed >> 32)) as u32 as i32;
                assert!(
                    matches!(result,Execution::Returned(value) if value==expected),
                    "pass={pass} seed={seed:x} rounds={rounds} expected={expected:x}"
                );
                checks += 1;
            }
        }
        let usage = (executor.direct_translation_usage(image.bias), arena.used());
        if let Some(expected) = warmed {
            assert_eq!(usage, expected, "warm execution must reuse translations");
        } else {
            warmed = Some(usage);
        }
    }
    let error = executor
        .run(
            &mut image,
            0x100100,
            Arguments {
                size: 1,
                pointer: u64::MAX,
            },
            &mut [],
        )
        .err()
        .expect("unbounded rounds must exhaust instruction fuel");
    assert!(error.contains("fuel"), "{error}");
    assert_eq!(executor.translation_cache_usage().libraries, 0);
    println!(
        "compiled Rust x86-64 workload: {checks} results passed; stable warm cache and fuel stop; {} guest bytes; {:?}; {} JIT bytes; zero dylibs",
        code.len(),
        executor.direct_translation_usage(image.bias),
        arena.used()
    );
    std::fs::remove_file(object).unwrap();
}
