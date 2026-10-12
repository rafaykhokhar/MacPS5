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
    let cases: [(&str, &[u8]); 10] = [
        ("control_load", &[0x0f, 0xae, 0x17]),
        ("control_store", &[0x0f, 0xae, 0x1f]),
        ("control_vload", &[0xc5, 0xf8, 0xae, 0x17]),
        ("control_vstore", &[0xc5, 0xf8, 0xae, 0x1f]),
        ("convert_reg", &[0xc4, 0xc1, 0x03, 0x2a, 0xe1]),
        ("convert_alias", &[0xc4, 0x41, 0x03, 0x2a, 0xf9]),
        ("convert_memory", &[0xc4, 0xe1, 0x03, 0x2a, 0x27]),
        ("scalar_low", &[0xc5, 0xfb, 0x10, 0x07]),
        ("scalar_high", &[0xc5, 0x7b, 0x10, 0x3f]),
        ("scalar_rip", &[0xc5, 0xfb, 0x10, 0x05, 0x2a, 0xdd, 0x1c, 0]),
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0xc3f2e, name,
        )?);
    }
    let asm = directory.join("vmovsd.S");
    let obj = directory.join("vmovsd.o");
    let source = directory.join("vmovsd-driver.rs");
    let binary = directory.join("vmovsd-driver");
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
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C" {fn control_load(cpu:*mut Cpu);fn control_store(cpu:*mut Cpu);fn control_vload(cpu:*mut Cpu);fn control_vstore(cpu:*mut Cpu);fn convert_reg(cpu:*mut Cpu);fn convert_alias(cpu:*mut Cpu);fn convert_memory(cpu:*mut Cpu);fn scalar_low(cpu:*mut Cpu);fn scalar_high(cpu:*mut Cpu);fn scalar_rip(cpu:*mut Cpu);}
fn main(){control_checks();conversion_checks();
 let cases:[(unsafe extern "C" fn(*mut Cpu),usize,u64);3]=[(scalar_low,0,0x300000),(scalar_high,15,0x300000),(scalar_rip,0,0x291c60)];
 for(block,dst,address)in cases{
  for payload in [0u64,0x8000000000000000,0x7ff0000000000001,0x7ff8123456789abc,u64::MAX]{
   let mut bytes=payload.to_le_bytes();let mut memory=Memory::default();memory.add(address,&mut bytes,false,false).unwrap();
   let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=address;cpu.vectors=[[0xa5;32];16];let before=cpu.vectors;
   unsafe{block(&mut cpu)};assert_eq!(cpu.fault,0);assert_eq!(cpu.flags,0xad7);assert_eq!(cpu.vectors[dst][..8],payload.to_le_bytes());assert_eq!(cpu.vectors[dst][8..],[0;24]);
   for i in 0..16{if i!=dst{assert_eq!(cpu.vectors[i],before[i]);}}
  }
  let mut bytes=[0u8;7];let mut memory=Memory::default();memory.add(address,&mut bytes,false,false).unwrap();
  let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=address;cpu.vectors=[[0x5a;32];16];let before=cpu.vectors;
  unsafe{block(&mut cpu)};assert_eq!(cpu.fault,1);assert_eq!(cpu.flags,0xad7);assert_eq!(cpu.vectors,before);
 }
 println!("PASS: ARM64 VMOVSD guarded8-byte bit-copy, RIP/high-register addressing, VEX upperzero, NaN/negativezero payloads and fault atomicity");
}
// Reference expected binary64 constants avoid depending on host FP conversion.
fn conversion_checks(){
 let cases:[(unsafe extern "C" fn(*mut Cpu),usize,bool);3]=[(convert_reg,4,false),(convert_alias,15,false),(convert_memory,4,true)];
 for(block,dst,from_memory)in cases{
  for(value,bits)in [(i32::MIN,0xc1e0000000000000u64),(-1,0xbff0000000000000),(0,0),(1,0x3ff0000000000000),(i32::MAX,0x41dfffffffc00000)]{
   let mut bytes=value.to_le_bytes();let mut memory=Memory::default();memory.add(0x300000,&mut bytes,false,false).unwrap();
   let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;cpu.registers[9]=0xdeadbeef00000000|value as u32 as u64;cpu.vectors=[[0xa5;32];16];cpu.vectors[15][8..16].copy_from_slice(&0x7ff0000000000001u64.to_le_bytes());let before=cpu.vectors;
   unsafe{block(&mut cpu)};assert_eq!(cpu.fault,0);assert_eq!(cpu.flags,0xad7);assert_eq!(cpu.vectors[dst][..8],bits.to_le_bytes());assert_eq!(cpu.vectors[dst][8..16],before[15][8..16]);assert_eq!(cpu.vectors[dst][16..],[0;16]);for i in 0..16{if i!=dst{assert_eq!(cpu.vectors[i],before[i]);}}
  }
  if from_memory{let mut bytes=[0u8;3];let mut memory=Memory::default();memory.add(0x300000,&mut bytes,false,false).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;cpu.vectors=[[0x5a;32];16];let before=cpu.vectors;unsafe{block(&mut cpu)};assert_eq!(cpu.fault,1);assert_eq!(cpu.vectors,before);}
 }
 println!("PASS: ARM64 exact signed32 VCVTSI2SD conversion, aliased merge/high-register/upperzero and guarded memory atomicity");
}
fn control_checks(){
 for load in [control_load as unsafe extern "C" fn(*mut Cpu),control_vload]{
  for value in [0u32,0xffff,0x1f80,0x10000]{let mut data=value.to_le_bytes();let mut memory=Memory::default();memory.add(0x300000,&mut data,false,false).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;unsafe{load(&mut cpu)};assert_eq!(cpu.flags,0xad7);if value<=0xffff{assert_eq!(cpu.fault,0);assert_eq!(cpu.padding,value as u64);}else{assert_eq!(cpu.fault,14);assert_eq!(cpu.padding,0x1f80);}}
  let mut data=[0u8;3];let mut memory=Memory::default();memory.add(0x300000,&mut data,false,false).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;unsafe{load(&mut cpu)};assert_eq!(cpu.fault,1);assert_eq!(cpu.padding,0x1f80);
 }
 for store in [control_store as unsafe extern "C" fn(*mut Cpu),control_vstore]{
  for writable in [true,false]{let mut data=[0x5au8;4];let mut memory=Memory::default();memory.add(0x300000,&mut data,writable,false).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,padding:0x8040,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;unsafe{store(&mut cpu)};assert_eq!(cpu.padding,0x8040);assert_eq!(cpu.fault,if writable{0}else{2});assert_eq!(memory.read(0x300000,4).unwrap(),if writable{&[0x40,0x80,0,0]}else{&[0x5a;4]});}
 }
 println!("PASS: ARM64 legacy/VEX MXCSR load-store, real zero/reset, reserved-bit fault14, guard atomicity, no host FP state dependency");
}
"#;
