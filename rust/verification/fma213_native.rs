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
    let cases: [(&str, &[u8]); 4] = [
        ("fma_reg", &[0xc4, 0xe2, 0xe1, 0xa9, 0xe2]),
        ("fma_memory", &[0xc4, 0xe2, 0xe1, 0xa9, 0x27]),
        ("fma_high", &[0xc4, 0x42, 0x89, 0xa9, 0xfd]),
        ("fma_alias", &[0xc4, 0xe2, 0xd9, 0xa9, 0xe4]),
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0xc3f2e, name,
        )?);
    }
    let asm = directory.join("fma213.S");
    let obj = directory.join("fma213.o");
    let source = directory.join("fma213-driver.rs");
    let binary = directory.join("fma213-driver");
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
unsafe extern "C" {fn fma_reg(cpu:*mut Cpu);fn fma_memory(cpu:*mut Cpu);fn fma_high(cpu:*mut Cpu);fn fma_alias(cpu:*mut Cpu);}
fn main(){
 let cases:[(unsafe extern "C" fn(*mut Cpu),usize,usize,usize,bool);3]=[(fma_reg,4,3,2,false),(fma_memory,4,3,2,true),(fma_high,15,14,13,false)];
 for(block,dst,first,second,mem)in cases{
  for(a,b,c,control,result,status,fault)in [
   (0x4000000000000000u64,0x4008000000000000u64,0x4010000000000000u64,0x1f80u32,0x4024000000000000u64,0u32,false),
   // Sourced FMA213 NaN precedence: explicit register, old destination, addend.
   (0x7ff8111111111111,0x7ff8222222222222,0x7ff8333333333333,0x1f80,0x7ff8222222222222,0,false),
   (0x7ff0111111111111,0x7ff8222222222222,0x7ff8333333333333,0x1f80,0x7ff8222222222222,1,false),
   (0x7ff8111111111111,0x7ff0222222222222,0x7ff8333333333333,0x1f80,0x7ff8222222222222,1,false),
   (0x7ff8111111111111,0x7ff8222222222222,0x7ff0333333333333,0x1f00,0,1,true),
   (0x7ff8111111111111,0x3ff0000000000000,0x7ff8333333333333,0x1f80,0x7ff8111111111111,0,false),
   (0,0x7ff0000000000000,0x7ff8333333333333,0x1f80,0x7ff8333333333333,0,false),
   (0,0x7ff0000000000000,0x3ff0000000000000,0x1f80,0xfff8000000000000,1,false),
   (0,0x7ff0000000000000,0x3ff0000000000000,0x1f00,0,1,true),
   (0x7ff0000000000000,0x3ff0000000000000,0xfff0000000000000,0x1f80,0xfff8000000000000,1,false),
   // (1+2^-52)*(1-2^-52)-1 = -2^-104 exactly, unlike separate rounded mul/add.
   (0x3ff0000000000001,0x3feffffffffffffe,0xbff0000000000000,0x1f80,0xb970000000000000,0,false),
   (0x3ff0000000000000,0x3ff0000000000000,0xbff0000000000000,0x3f80,0x8000000000000000,0,false),
   (0x3ff0000000000000,0x3ff0000000000000,0xbff0000000000000,0x1f80,0,0,false),
   (0x3ff0000000000000,0x3ff0000000000000,0x3ca0000000000000,0x1f80,0x3ff0000000000000,32,false),
   (0x3ff0000000000000,0x3ff0000000000000,0x3ca0000000000000,0x5f80,0x3ff0000000000001,32,false),
   (0x3ff0000000000000,0x3ff0000000000000,0x3ca0000000000000,0x0f80,0,32,true),
  ]{
   let mut data=c.to_le_bytes();let mut memory=Memory::default();memory.add(0x300000,&mut data,false,false).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,padding:control as u64,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;
   for i in 0..16{cpu.vectors[i]=[i as u8+0x40;32];}cpu.vectors[dst][..8].copy_from_slice(&a.to_le_bytes());cpu.vectors[first][..8].copy_from_slice(&b.to_le_bytes());if !mem{cpu.vectors[second][..8].copy_from_slice(&c.to_le_bytes());}let before=cpu.clone();unsafe{block(&mut cpu)};
   assert_eq!(cpu.fault,if fault{15}else{0});assert_eq!(cpu.padding,(control|status)as u64);assert_eq!(cpu.flags,before.flags);assert_eq!(cpu.registers,before.registers);
   if fault{assert_eq!(cpu.vectors,before.vectors);assert_eq!(cpu.rip,before.rip);}else{assert_eq!(cpu.vectors[dst][..8],result.to_le_bytes());assert_eq!(cpu.vectors[dst][8..16],before.vectors[dst][8..16]);assert_eq!(cpu.vectors[dst][16..],[0;16]);for i in 0..16{if i!=dst{assert_eq!(cpu.vectors[i],before.vectors[i]);}}}
  }
 }
 let mut cpu=Cpu{rip:0xc3f2e,fuel:20,..Cpu::default()};cpu.vectors=[[0xa5;32];16];cpu.vectors[4][..8].copy_from_slice(&0x4008000000000000u64.to_le_bytes());unsafe{fma_alias(&mut cpu)};assert_eq!(cpu.fault,0);assert_eq!(cpu.vectors[4][..8],0x4028000000000000u64.to_le_bytes());assert_eq!(cpu.vectors[4][8..16],[0xa5;8]);assert_eq!(cpu.vectors[4][16..],[0;16]);
 let mut data=[0u8;7];let mut memory=Memory::default();memory.add(0x300000,&mut data,false,false).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;cpu.vectors=[[0x5a;32];16];let before=cpu.clone();unsafe{fma_memory(&mut cpu)};assert_eq!(cpu.fault,1);assert_eq!(cpu.vectors,before.vectors);assert_eq!(cpu.padding,before.padding);assert_eq!(cpu.rip,before.rip);
 println!("PASS ARM64 VFMADD213SD NaN priority/sNaN/invalid infinity, fused cancellation, rounding/precision, exactzero sign, aliases/high-register, old-destination upper-lane preservation, YMM upperzero and guarded memory");
}
"#;
