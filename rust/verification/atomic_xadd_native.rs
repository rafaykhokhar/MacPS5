//! Actual guarded ARM64 execution of aligned LOCK XADD32, not host concurrency.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x31000000;
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    flags_probe(archive, directory)?;
    let mut f = memory_fixture();
    // Native module path exercises memory and returned register without PUSHFQ.
    let c = [0x8b, 0x47, 4, 0xf0, 0x0f, 0xc1, 0x07, 0xc3];
    f[0x100..0x100 + c.len()].copy_from_slice(&c);
    // Address/source alias: old edi is the addend, address snapshots full old rdi.
    let alias = [0xf0, 0x0f, 0xc1, 0x3f, 0x48, 0x89, 0xf8, 0xc3];
    f[0x180..0x180 + alias.len()].copy_from_slice(&alias);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    unsafe {
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x180]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&f, 0x100000, 65536)?;
    #[repr(align(4))]
    struct Bytes([u8; 16]);
    for (old, add, _flags) in [
        (u32::MAX, 1u32, 0x57u64),
        (0x7fffffff, 1, 0x896),
        (15, 1, 0x12),
        (0, 0, 0x46),
    ] {
        let mut b = Bytes([0; 16]);
        b.0[..4].copy_from_slice(&old.to_le_bytes());
        b.0[4..8].copy_from_slice(&add.to_le_bytes());
        assert!(
            matches!(n.run_worker_with_data(&mut image,0x100100,DATA,&mut[],&mut[DataRegion{address:DATA,bytes:&mut b.0,writable:true}])?,Execution::WorkerReturned(v)if v==old as u64)
        );
        assert_eq!(&b.0[..4], &old.wrapping_add(add).to_le_bytes());
    }
    let mut b = Bytes([0; 16]);
    b.0[..4].copy_from_slice(&7u32.to_le_bytes());
    assert!(matches!(
        n.run_worker_with_data(
            &mut image,
            0x100180,
            DATA,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b.0,
                writable: true
            }]
        )?,
        Execution::WorkerReturned(7)
    ));
    assert_eq!(&b.0[..4], &(7 + DATA as u32).to_le_bytes());
    for (address, len, writable) in [
        (DATA, 16, false),
        (DATA + 1, 16, true),
        (DATA + 12, 14, true),
    ] {
        let snapshot = b.0;
        assert!(
            n.run_worker_with_data(
                &mut image,
                0x100180,
                address,
                &mut [],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut b.0[..len],
                    writable
                }]
            )
            .is_err()
        );
        assert_eq!(snapshot, b.0);
    }
    let snapshot = b.0;
    assert!(
        n.run_worker_with_data(
            &mut image,
            0x100180,
            DATA,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b.0[1..],
                writable: true
            }]
        )
        .is_err()
    );
    assert_eq!(snapshot, b.0);
    println!(
        "Native aligned LOCK XADD32 flags, source/address alias, writable/alignment/range guards passed"
    );
    Ok(())
}

fn flags_probe(archive: &Path, directory: &Path) -> Result<(), String> {
    use std::process::Command;
    fn run(c: &mut Command) -> Result<(), String> {
        let out = c.output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        print!("{}", String::from_utf8_lossy(&out.stdout));
        Ok(())
    }
    let asm = directory.join("xadd-flags.S");
    let obj = directory.join("xadd-flags.o");
    let source = directory.join("xadd-flags.rs");
    let binary = directory.join("xadd-flags");
    std::fs::write(
        &asm,
        native_ps5::aot::translate_rust_memory_region(
            &[0xf0, 0x0f, 0xc1, 0x07],
            0x18b52,
            "xaddflags",
        )?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(&source, FLAGS_DRIVER).map_err(|e| e.to_string())?;
    run(Command::new("xcrun")
        .args(["clang", "-arch", "arm64", "-c"])
        .arg(&asm)
        .arg("-o")
        .arg(&obj))?;
    let profile = archive.parent().ok_or("Missing archive parent")?;
    run(Command::new("rustc")
        .args(["--edition=2024", "-O", "-Dwarnings"])
        .arg(&source)
        .arg("--extern")
        .arg(format!(
            "native_ps5={}",
            profile.join("libnative_ps5.rlib").display()
        ))
        .arg("-L")
        .arg(format!("dependency={}", profile.join("deps").display()))
        .arg("-C")
        .arg(format!("link-arg={}", obj.display()))
        .arg("-o")
        .arg(&binary))?;
    run(&mut Command::new(binary))
}
const FLAGS_DRIVER: &str = r#"
use native_ps5::{guest_memory::Memory,legacy_cpu::Cpu};
unsafe extern "C" {fn xaddflags(cpu:*mut Cpu);}
#[repr(align(4))]struct Bytes([u8;4]);
fn main(){for(old,add,expected)in [(u32::MAX,1u32,0x55u64),(0x7fffffff,1,0x894),(15,1,0x10),(0,0,0x44)]{
 let mut b=Bytes(old.to_le_bytes());let mut memory=Memory::default();memory.add(0x300000,&mut b.0,true,false).unwrap();
 let mut cpu=Cpu{memory:&mut memory as *mut _ as u64,rip:0x18b52,fuel:7,flags:0x202,..Cpu::default()};cpu.registers[0]=0xabcd000000000000|add as u64;cpu.registers[7]=0x300000;
 let before=cpu.clone();unsafe{xaddflags(&mut cpu)};assert_eq!(cpu.fault,0);assert_eq!(cpu.flags,0x202|expected);assert_eq!(cpu.registers[0],old as u64);assert_eq!(cpu.registers[1..],before.registers[1..]);assert_eq!(cpu.fuel,6);assert_eq!(cpu.vectors,before.vectors);assert_eq!(memory.load(0x300000,4).unwrap(),old.wrapping_add(add)as u64);
 }println!("Direct translated LOCK XADD32 flags/fuel/register preservation passed");}
"#;
