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
    let cases: [(&str, &[u8]); 5] = [
        ("multiply_alias", &[0xc5, 0xdb, 0x59, 0xe0]),
        ("multiply_separate", &[0xc5, 0xdb, 0x59, 0xd1]),
        ("multiply_memory", &[0xc5, 0xdb, 0x59, 0x17]),
        ("multiply_high", &[0xc4, 0x41, 0x83, 0x59, 0xfe]),
        ("multiply_allalias", &[0xc5, 0xdb, 0x59, 0xe4]),
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0xc3f2e, name,
        )?);
    }
    let asm = directory.join("vmulsd.S");
    let obj = directory.join("vmulsd.o");
    let source = directory.join("vmulsd-driver.rs");
    let binary = directory.join("vmulsd-driver");
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
unsafe extern "C" {fn multiply_alias(cpu:*mut Cpu);fn multiply_separate(cpu:*mut Cpu);fn multiply_memory(cpu:*mut Cpu);fn multiply_high(cpu:*mut Cpu);fn multiply_allalias(cpu:*mut Cpu);}
fn main(){control_vectors();
 let cases:[(unsafe extern "C" fn(*mut Cpu),usize,usize,usize,bool);4]=[(multiply_alias,4,4,0,false),(multiply_separate,2,4,1,false),(multiply_memory,2,4,1,true),(multiply_high,15,15,14,false)];
 // Explicit bit-pattern expectations; no host arithmetic or helper oracle.
 let vectors=[
 (0x4000000000000000u64,0x4008000000000000u64,0x4018000000000000u64,0u32),
 (0x8000000000000000,0x4008000000000000,0x8000000000000000,0),
 (0x7ff0000000000000,0x4000000000000000,0x7ff0000000000000,0),
 (0,0x7ff0000000000000,0xfff8000000000000,1),
 (0x7ff8123456789abc,0x4000000000000000,0x7ff8123456789abc,0),
 (0x7ff0123456789abc,0x4000000000000000,0x7ff8123456789abc,1),
 (1,0x3ff0000000000000,1,2),
 (0x0010000000000000,0x3fe0000000000000,0x0008000000000000,0),
 (0x7fefffffffffffff,0x4000000000000000,0x7ff0000000000000,40),
 ];
 for(block,dst,first,second,mem)in cases{
  for(a,b,result,status)in vectors{
   for unmask in [false,true]{
    // Exact tiny has no exception with UM masked. Overflow unmasked behavior
    // is tested separately by model; here unmask only the actual precondition.
    let control=if unmask {0x1f80 & !((status & 3)<<7)}else{0x1f80};
    let mut data=b.to_le_bytes();let mut memory=Memory::default();memory.add(0x300000,&mut data,false,false).unwrap();
    let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,padding:control as u64,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;cpu.vectors=[[0xa5;32];16];cpu.vectors[first][..8].copy_from_slice(&a.to_le_bytes());if !mem{cpu.vectors[second][..8].copy_from_slice(&b.to_le_bytes());}let before=cpu.clone();
    unsafe{block(&mut cpu)};let fault=unmask && status&3!=0;
    assert_eq!(cpu.fault,if fault{15}else{0});assert_eq!(cpu.padding,(control|status)as u64);assert_eq!(cpu.flags,before.flags);assert_eq!(cpu.registers,before.registers);
    if fault{assert_eq!(cpu.vectors,before.vectors);assert_eq!(cpu.rip,before.rip);}else{assert_eq!(cpu.vectors[dst][..8],result.to_le_bytes());assert_eq!(cpu.vectors[dst][8..16],before.vectors[first][8..16]);assert_eq!(cpu.vectors[dst][16..],[0;16]);for index in 0..16{if index!=dst{assert_eq!(cpu.vectors[index],before.vectors[index]);}}}
   }
  }
 }
 let mut cpu=Cpu{rip:0xc3f2e,fuel:20,..Cpu::default()};cpu.vectors=[[0x5a;32];16];cpu.vectors[4][..8].copy_from_slice(&0x4008000000000000u64.to_le_bytes());let high=cpu.vectors[4][8..16].to_vec();unsafe{multiply_allalias(&mut cpu)};assert_eq!(cpu.fault,0);assert_eq!(cpu.vectors[4][..8],0x4022000000000000u64.to_le_bytes());assert_eq!(cpu.vectors[4][8..16],high);assert_eq!(cpu.vectors[4][16..],[0;16]);
 let mut data=[0u8;7];let mut memory=Memory::default();memory.add(0x300000,&mut data,false,false).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;cpu.vectors=[[0x5a;32];16];let before=cpu.clone();unsafe{multiply_memory(&mut cpu)};assert_eq!(cpu.fault,1);assert_eq!(cpu.vectors,before.vectors);assert_eq!(cpu.padding,before.padding);assert_eq!(cpu.rip,before.rip);
 println!("PASS ARM64 VMULSD finite/nonfinite/denormal multiplication, masked flags, unmasked no-destination commit, high-register/alias/merge-upperzero and guarded memory");
}

fn control_vectors(){
 for (a,b,control,result,status,fault) in [
  (1u64,0x3ff0000000000000u64,0x1fc0u32,0u64,0u32,false),
  (0x0010000000000000,0x3fe0000000000000,0x9f80,0,48,false),
  (0x3ff0000000000001,0x3ff0000000000001,0x1f80,0x3ff0000000000002,32,false),
  (0x3ff0000000000001,0x3ff0000000000001,0x5f80,0x3ff0000000000003,32,false),
  (0x3ff0000000000001,0x3ff0000000000001,0x0f80,0,32,true),
  (0x7ff8123456789abc,0xfff056789abcdef0,0x1f80,0x7ff8123456789abc,1,false),
  (0xfff056789abcdef0,0x7ff8123456789abc,0x1f80,0xfff856789abcdef0,1,false),
 ] {
  let mut cpu=Cpu{rip:0xc3f2e,fuel:20,padding:control as u64,..Cpu::default()};cpu.vectors=[[0xa5;32];16];cpu.vectors[4][..8].copy_from_slice(&a.to_le_bytes());cpu.vectors[0][..8].copy_from_slice(&b.to_le_bytes());let before=cpu.clone();unsafe{multiply_alias(&mut cpu)};assert_eq!(cpu.padding,(control|status)as u64);assert_eq!(cpu.fault,if fault{15}else{0});if fault{assert_eq!(cpu.vectors,before.vectors);assert_eq!(cpu.rip,before.rip);}else{assert_eq!(cpu.vectors[4][..8],result.to_le_bytes());assert_eq!(cpu.vectors[4][8..16],before.vectors[4][8..16]);assert_eq!(cpu.vectors[4][16..],[0;16]);}
 }
}
"#;
