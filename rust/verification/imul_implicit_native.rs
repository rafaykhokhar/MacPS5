//! Native signed full-product IMUL oracle using independent i128 products.
//! Checks implicit destinations, signed overflow flags and guarded memory.
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
    let cases: [(&str, &[u8]); 9] = [
        ("imul8", &[0xf6,0xea]), ("imul16", &[0x66,0xf7,0xea]),
        ("imul32", &[0xf7,0xea]), ("imul64", &[0x48,0xf7,0xea]),
        ("imul8m", &[0xf6,0x2f]), ("imul16m", &[0x66,0xf7,0x2f]),
        ("imul32m", &[0xf7,0x2f]), ("imul64m", &[0x48,0xf7,0x2f]),
        ("imul_ah", &[0xf6,0xec]),
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0xc3f2e, name,
        )?);
    }
    let asm = directory.join("imul_implicit.S");
    let obj = directory.join("imul_implicit.o");
    let source = directory.join("imul_implicit-driver.rs");
    let binary = directory.join("imul_implicit-driver");
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
const DRIVER:&str=r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C" {fn imul8(c:*mut Cpu);fn imul16(c:*mut Cpu);fn imul32(c:*mut Cpu);fn imul64(c:*mut Cpu);fn imul8m(c:*mut Cpu);fn imul16m(c:*mut Cpu);fn imul32m(c:*mut Cpu);fn imul64m(c:*mut Cpu);fn imul_ah(c:*mut Cpu);}
fn main(){
 let cases:[(usize,unsafe extern "C" fn(*mut Cpu),bool);8]=[(8,imul8,false),(16,imul16,false),(32,imul32,false),(64,imul64,false),(8,imul8m,true),(16,imul16m,true),(32,imul32m,true),(64,imul64m,true)];
 let mut seed=0xdac4053898ff2731u64;
 for (width,block,mem) in cases{
  let mask=u64::MAX>>(64-width);let sign=1u64<<(width-1);
  let edges=[0,1,2,sign-1,sign,sign+1,mask-1,mask];
  let mut vectors=Vec::new();for a in edges{for b in edges{vectors.push((a,b));}}
  for _ in 0..4096{seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;let a=seed;seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;vectors.push((a,seed));}
  for (a,b) in vectors{
   let signed=|v:u64| ((v as i128)<<(128-width))>>(128-width);
   let product=signed(a)*signed(b);let overflow=product < -(1i128<<(width-1))||product>=(1i128<<(width-1));
   let mut data=b.to_le_bytes();let mut memory=Memory::default();memory.add(0x300000,&mut data,false,false).unwrap();
   let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers=[0x5a5a5a5a5a5a5a5a;16];cpu.registers[0]=a;cpu.registers[2]=b;cpu.registers[7]=0x300000;let before=cpu.clone();
   let mut expected=before.registers;
   if width==8{expected[0]=(a&!0xffff)|(product as u64&0xffff);}else{let lo=product as u64&mask;let hi=(product>>width)as u64&mask;expected[0]=if width==16{(a&!mask)|lo}else{lo};expected[2]=if width==16{(b&!mask)|hi}else{hi};}
   unsafe{block(&mut cpu)};assert_eq!(cpu.fault,0);assert_eq!(cpu.registers,expected,"width{width} {a:x} {b:x}");assert_eq!(cpu.flags,(before.flags&!0x801)|2|if overflow{0x801}else{0});assert_eq!(cpu.vectors,before.vectors);assert_eq!((cpu.mxcsr,cpu.x87_control,cpu.x87_status),(before.mxcsr,before.x87_control,before.x87_status));
  }
  if mem{let mut bytes=vec![0x5a;width/8-1];let mut memory=Memory::default();if !bytes.is_empty(){memory.add(0x300000,&mut bytes,false,false).unwrap();}let mut cpu=Cpu{rip:0xc3f2e,fuel:20,flags:0xad7,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;let before=cpu.clone();unsafe{block(&mut cpu)};assert_eq!(cpu.fault,1);assert_eq!(cpu.registers,before.registers);assert_eq!(cpu.flags,before.flags);assert_eq!(cpu.rip,before.rip);}
 }
 for value in [0x807fu64,0xff80,0x0101,0xffff]{let mut cpu=Cpu{rip:0xc3f2e,fuel:20,..Cpu::default()};cpu.registers[0]=0xa55a000000000000|value;let expected=(value as u8 as i8 as i16)*((value>>8)as u8 as i8 as i16);unsafe{imul_ah(&mut cpu)};assert_eq!(cpu.registers[0],0xa55a000000000000|expected as u16 as u64);}
 println!("PASS: ARM64 implicit signed IMUL8/16/32/64 edge/random i128 oracle, RDX operand alias, AH source, guarded memory, CF/OF and preserved undefined flags/state");
}
"#;
