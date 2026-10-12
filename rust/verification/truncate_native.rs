//! Focused ARM64 scalar-load oracle. verify compiles generated assembly and a
//! driver against the supplied matching local release library; no host FP math.
use std::{path::Path, process::Command};
fn run(command: &mut Command) -> Result<(), String> {
    let result = command.output().map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned());
    }
    print!("{}", String::from_utf8_lossy(&result.stdout));
    Ok(())
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let cases: [(&str, &[u8]); 3] = [
        ("truncate_low", &[0xc5, 0xfb, 0x2c, 0xc0]),
        ("truncate_high", &[0xc4, 0x41, 0x7b, 0x2c, 0xcf]),
        ("truncate_memory", &[0xc5, 0xfb, 0x2c, 0x07]),
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0xc3f2e, name,
        )?);
    }
    let asm = directory.join("truncate.S");
    let obj = directory.join("truncate.o");
    let source = directory.join("truncate-driver.rs");
    let binary = directory.join("truncate-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(&source, DRIVER).map_err(|e| e.to_string())?;
    run(Command::new("xcrun")
        .args(["clang", "-arch", "arm64", "-c"])
        .arg(&asm)
        .arg("-o")
        .arg(&obj))?;
    let profile = archive.parent().ok_or("Archive missing parent")?;
    run(
        Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
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
            .arg(&binary),
    )?;
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu, guest_memory::Memory};
unsafe extern "C" { fn truncate_low(cpu:*mut Cpu); fn truncate_high(cpu:*mut Cpu); fn truncate_memory(cpu:*mut Cpu); }
fn main() {
 let cases:[(unsafe extern "C" fn(*mut Cpu),usize,usize,bool);3] = [(truncate_low,0,0,false),(truncate_high,9,15,false),(truncate_memory,0,0,true)];
 for (block,dst,src,from_memory) in cases {
  for (bits,expected,status) in [(0u64,0u64,0u32),(0x3ff8000000000000,1,32),(0xbff8000000000000,0xffffffff,32),(0xc1e0000000000000,0x80000000,0),(0xc1e0000000100000,0x80000000,32),(0x41e0000000000000,0x80000000,1),(0x7ff8000000000001,0x80000000,1),(0x7ff0000000000001,0x80000000,1),(0xfff0000000000000,0x80000000,1),(1,0,32)] {
   for rounding in 0..4u32 { for daz in [0u32,64] { for unmask in [false,true] {
    let status=if daz!=0 && bits==1 {0}else{status};
    let mut control=0x1f80|(rounding<<13)|daz|4;
    if unmask {control&=!(status<<7);}
    let mut data=bits.to_le_bytes();let mut memory=Memory::default();memory.add(0x300000,&mut data,false,false).unwrap();
    let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,mxcsr:control,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};
    cpu.registers=[0xa5a5a5a5a5a5a5a5;16];cpu.registers[7]=0x300000;cpu.vectors=[[0x5a;32];16];cpu.vectors[src][..8].copy_from_slice(&bits.to_le_bytes());let before=cpu.clone();
    cpu.x87_control=0x0b7f;cpu.x87_status=0x20;unsafe{block(&mut cpu)};assert_eq!((cpu.x87_control,cpu.x87_status),(0x0b7f,0x20));
    assert_eq!(cpu.flags,before.flags);assert_eq!(cpu.vectors,before.vectors);assert_eq!(cpu.mxcsr,(control|status));
    let fault=unmask&&status!=0;assert_eq!(cpu.fault,if fault{15}else{0});
    assert_eq!(cpu.registers[dst],if fault{before.registers[dst]}else{expected});
    if fault{assert_eq!(cpu.rip,before.rip);}
    for index in 0..16{if index!=dst{assert_eq!(cpu.registers[index],before.registers[index]);}}
   }}}
  }
  if from_memory {
   let mut data=[0u8;7];let mut memory=Memory::default();memory.add(0x300000,&mut data,false,false).unwrap();
   let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;cpu.registers[dst]=u64::MAX;let before=cpu.clone();
   cpu.x87_control=0x0b7f;cpu.x87_status=0x20;unsafe{block(&mut cpu)};assert_eq!((cpu.x87_control,cpu.x87_status),(0x0b7f,0x20));assert_eq!(cpu.fault,1);assert_eq!(cpu.registers,before.registers);assert_eq!(cpu.mxcsr,before.mxcsr);assert_eq!(cpu.rip,before.rip);
  }
 }
 println!("PASS ARM64 signed32 VCVTTSD2SI register/high-register/guarded memory, integer bounds, DAZ, RC-independent truncation, NaN/infinity, sticky flags, unmasked destination/RIP preservation and GPR zero extension");
}
"#;
