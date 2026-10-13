//! Compile actual AOT assembly and test the resulting ARM64 functions.
use std::{path::PathBuf, process::Command};
fn main() -> Result<(), String> {
    let rlib = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("Expected release rlib path")?,
    );
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(2)
            .ok_or("Expected output directory")?,
    );
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let mut asm = String::new();
    let mut declarations = String::new();
    let mut cases = String::new();
    for (kind, bytes) in [
        vec![0x66, 0x98],
        vec![0x98],
        vec![0x48, 0x98],
        vec![0x66, 0x99],
        vec![0x99],
        vec![0x48, 0x99],
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("accumulator_extend_{kind}");
        asm.push_str(&native_ps5::aot::translate_rust_memory_region(
            &bytes, 0x100, &name,
        )?);
        declarations.push_str(&format!("fn {name}(cpu: *mut Cpu);\n"));
        cases.push_str(&format!(
            "({kind}, {name} as unsafe extern \"C\" fn(*mut Cpu), &{bytes:?}[..]),\n"
        ));
    }
    std::fs::write(directory.join("accumext.S"), asm).map_err(|e| e.to_string())?;
    let output = Command::new("xcrun")
        .args(["clang", "-arch", "arm64", "-c"])
        .arg(directory.join("accumext.S"))
        .arg("-o")
        .arg(directory.join("accumext.o"))
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let source = DRIVER
        .replace("DECLARATIONS", &declarations)
        .replace("CASES", &cases);
    std::fs::write(directory.join("driver.rs"), source).map_err(|e| e.to_string())?;
    let output = Command::new("rustc")
        .args(["--edition=2024", "-O", "-Dwarnings", "--extern"])
        .arg(format!("native_ps5={}", rlib.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            rlib.parent()
                .ok_or("No rlib parent")?
                .join("deps")
                .display()
        ))
        .arg(directory.join("driver.rs"))
        .arg("-C")
        .arg(format!(
            "link-arg={}",
            directory.join("accumext.o").display()
        ))
        .arg("-o")
        .arg(directory.join("driver"))
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let output = Command::new(directory.join("driver"))
        .output()
        .map_err(|e| e.to_string())?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(())
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu, direct_cpu, jit_memory::JitArena};
unsafe extern "C" { DECLARATIONS }
fn main() { unsafe {
    let mut checks = 0;
    let arena = JitArena::new(1024 * 1024).unwrap();
    for (kind, run, instruction) in [CASES] {
        let mut bytes = instruction.to_vec();
        bytes.push(0xc3);
        let code = arena.publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap()).unwrap();
        let direct: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
        let mut values = vec![0u64, 1, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff, 0x7fffffff, 0x80000000, 0x7fffffffffffffff, 0x8000000000000000, u64::MAX];
        if kind == 0 { values.extend((0..256).map(|v| 0x123456789abcde00 | v)); }
        if kind == 1 || kind == 3 { values.extend((0..65536).map(|v| 0x123456789abc0000 | v)); }
        for value in values {
            let mut cpu = Cpu { rip: 0x100, fuel: 1, flags: 0xadff, registers: [0x123456789abcdef0;16], ..Cpu::default() };
            cpu.registers[0] = value;
            let mut expected = cpu.registers;
            match kind {
                0 => expected[0] = (value & !65535) | u64::from(value as i8 as i16 as u16),
                1 => expected[0] = u64::from(value as i16 as i32 as u32),
                2 => expected[0] = value as i32 as i64 as u64,
                3 => expected[2] = (expected[2] & !65535) | if (value as i16) < 0 {65535} else {0},
                4 => expected[2] = if (value as i32) < 0 {u32::MAX as u64} else {0},
                _ => expected[2] = if (value as i64) < 0 {u64::MAX} else {0},
            }
            let mut direct_state = cpu.clone();
            run(&mut cpu);
            direct(&mut direct_state);
            assert_eq!(cpu.registers, expected, "AOT kind={kind} value={value:x}");
            assert_eq!(cpu.flags, 0xadff);
            assert_eq!(cpu.fault, 0);
            assert_eq!(direct_state.registers, expected, "direct kind={kind} value={value:x}");
            assert_eq!(direct_state.flags, cpu.flags);
            assert_eq!(direct_state.fault, 0);
            checks += 1;
        }
    }
    println!("AOT/direct accumulator extension: {checks} paired native state checks passed");
} }
"#;
