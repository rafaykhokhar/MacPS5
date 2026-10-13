use native_ps5::{
    arm64_code::{Arm64Code, Branch},
    jit_memory::JitArena,
};
use std::{path::PathBuf, process::Command};
fn main() {
    let directory = PathBuf::from(std::env::args_os().nth(1).expect("fresh output directory"));
    std::fs::create_dir(&directory).unwrap();
    let mut code = Arm64Code::new(30000);
    let mut source = String::from(".text\nLback:\nnop\n");
    code.bind(0).unwrap();
    code.emit(0xd503201f).unwrap();
    let mut cases = vec![
        (Branch::Jump, "b".to_owned()),
        (Branch::Call, "bl".to_owned()),
    ];
    for (condition, name) in [
        "eq", "ne", "hs", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le",
    ]
    .iter()
    .enumerate()
    {
        cases.push((Branch::Conditional(condition as u8), format!("b.{name}")));
    }
    for register in 0..32 {
        for nonzero in [false, true] {
            for wide in [false, true] {
                let reg = if register == 31 {
                    if wide { "xzr".into() } else { "wzr".into() }
                } else {
                    format!("{}{register}", if wide { 'x' } else { 'w' })
                };
                cases.push((
                    Branch::CompareZero {
                        register,
                        wide,
                        nonzero,
                    },
                    format!("{} {reg},", if nonzero { "cbnz" } else { "cbz" }),
                ));
            }
            for bit in 0..64 {
                let reg = if register == 31 {
                    if bit < 32 { "wzr".into() } else { "xzr".into() }
                } else {
                    format!("{}{register}", if bit < 32 { 'w' } else { 'x' })
                };
                cases.push((
                    Branch::TestBit {
                        register,
                        bit,
                        nonzero,
                    },
                    format!("{} {reg}, #{bit},", if nonzero { "tbnz" } else { "tbz" }),
                ));
            }
        }
    }
    // Keep branch distances below the smallest (TBZ) range, while testing both signs.
    for (index, (kind, assembly)) in cases.iter().enumerate() {
        let label = index as u32 + 1;
        code.bind(100000 + label).unwrap();
        code.branch(label, *kind).unwrap();
        code.emit(0xd503201f).unwrap();
        code.bind(label).unwrap();
        code.branch(100000 + label, *kind).unwrap();
        source.push_str(&format!(
            "Lback{label}:\n{assembly} L{label}\nnop\nL{label}:\n{assembly} Lback{label}\n"
        ));
    }
    code.emit(0xd65f03c0).unwrap();
    source.push_str("ret\n");
    for register in 0..31 {
        for value in [0, u64::MAX, 0x123456789abcdef0, 1 << 63, 0xffff00000000ffff] {
            code.load_u64(register, value).unwrap();
            for half in 0..4 {
                source.push_str(&format!(
                    "{} x{register}, #{}, lsl #{}\n",
                    if half == 0 { "movz" } else { "movk" },
                    (value >> (half * 16)) & 0xffff,
                    half * 16
                ));
            }
        }
    }
    code.call_absolute(0x123456789abcdef0).unwrap();
    source.push_str("movz x16, #57072\nmovk x16, #39612, lsl #16\nmovk x16, #22136, lsl #32\nmovk x16, #4660, lsl #48\nblr x16\n");
    for wide in [false, true] {
        let scale = if wide { 8 } else { 4 };
        for store in [false, true] {
            for register in 0..32 {
                for base in 0..32 {
                    for offset in [0, scale, 4095 * scale] {
                        code.context_access(register, base, offset, wide, store)
                            .unwrap();
                        let reg = if register == 31 {
                            if wide { "xzr".into() } else { "wzr".into() }
                        } else {
                            format!("{}{register}", if wide { 'x' } else { 'w' })
                        };
                        let base = if base == 31 {
                            "sp".into()
                        } else {
                            format!("x{base}")
                        };
                        source.push_str(&format!(
                            "{} {reg}, [{base}, #{offset}]\n",
                            if store { "str" } else { "ldr" }
                        ));
                    }
                }
            }
        }
    }
    let words = code.finish().unwrap();
    std::fs::write(directory.join("oracle.s"), source).unwrap();
    assert!(
        Command::new("xcrun")
            .args(["clang", "-arch", "arm64", "-c"])
            .arg(directory.join("oracle.s"))
            .arg("-o")
            .arg(directory.join("oracle.o"))
            .status()
            .unwrap()
            .success()
    );
    let dump = Command::new("xcrun")
        .args(["llvm-objdump", "-s", "--section=__text"])
        .arg(directory.join("oracle.o"))
        .output()
        .unwrap();
    assert!(dump.status.success());
    let text = String::from_utf8(dump.stdout).unwrap();
    let mut bytes = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields
            .first()
            .is_some_and(|field| field.chars().all(|c| c.is_ascii_hexdigit()))
        {
            for field in fields.iter().skip(1).take(4) {
                if field.len() != 8 || !field.chars().all(|c| c.is_ascii_hexdigit()) {
                    break;
                }
                for offset in (0..8).step_by(2) {
                    bytes.push(u8::from_str_radix(&field[offset..offset + 2], 16).unwrap());
                }
            }
        }
    }
    assert_eq!(
        bytes,
        words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>()
    );
    let mut loop_code = Arm64Code::new(8);
    loop_code.emit(0x52800000).unwrap(); // mov w0,0
    loop_code.emit(0x5284e201).unwrap(); // mov w1,10000
    loop_code.bind(0).unwrap();
    loop_code.emit(0x0b010000).unwrap(); // add w0,w0,w1
    loop_code.emit(0x71000421).unwrap(); // subs w1,w1,1
    loop_code.branch(0, Branch::Conditional(1)).unwrap();
    loop_code.emit(0xd65f03c0).unwrap();
    // SAFETY: standalone thread, known MOV/ADD/SUBS/B.NE/RET, u32 return ABI.
    unsafe {
        let arena = JitArena::new(16384).unwrap();
        let code = arena.publish(&loop_code.finish().unwrap()).unwrap();
        let run: unsafe extern "C" fn() -> u32 = std::mem::transmute(code.entry(0).unwrap());
        for _ in 0..100 {
            assert_eq!(run(), 50_005_000);
        }
        // Match a real runtime helper's six-register arguments and two-register
        // result through the generated call stub, including fault returns.
        use native_ps5::arithmetic::{FlagsResult, native_ps5_arithmetic_flags_v1};
        let mut stub = Arm64Code::new(8);
        stub.emit(0xa9bf7bfd).unwrap(); // stp x29,x30,[sp,#-16]!
        stub.call_absolute(native_ps5_arithmetic_flags_v1 as *const () as u64)
            .unwrap();
        stub.emit(0xa8c17bfd).unwrap(); // ldp x29,x30,[sp],#16
        stub.emit(0xd65f03c0).unwrap();
        let helper = arena.publish(&stub.finish().unwrap()).unwrap();
        let call: unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> FlagsResult =
            std::mem::transmute(helper.entry(0).unwrap());
        for operation in 0..9 {
            for bits in [8, 16, 32, 64, 65] {
                for a in [0, 1, 127, 128, 255, u64::MAX, 1 << 63, 0x123456789abcdef0] {
                    for b in [0, 1, 255, u64::MAX] {
                        let result = a.wrapping_add(b);
                        let expected =
                            native_ps5_arithmetic_flags_v1(0xa55a, a, b, result, bits, operation);
                        assert_eq!(call(0xa55a, a, b, result, bits, operation), expected);
                    }
                }
            }
        }
    }
    println!(
        "PASS: {} branch forms, 155 constants, 12288 context accesses and an absolute call match clang bytes; 100 native loop sums and 1440 runtime helper ABI checks",
        cases.len() * 2
    );
}
