//! AMD APM Volume 3 rev. 3.31, pp. 227–228: CF=input zero, ZF=count zero.
//! OF/SF/AF/PF are undefined; this implementation preserves them deterministically.
//! Unmodified AMD manual mirror:
//! https://kib.kiev.ua/x86docs/AMD/AMD64/24594_APM_v3-r3.31.pdf
//! Independent driver counts bits with a shift loop, not the emitter's CLZ.
use std::{path::Path, process::Command};
fn run(c: &mut Command) -> Result<(), String> {
    let o = c.output().map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ));
    }
    print!("{}", String::from_utf8_lossy(&o.stdout));
    Ok(())
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let cases: [(&str, &[u8]); 7] = [
        ("observed", &[0xf3, 0x0f, 0xbd, 0xf2]), // esi,edx; pinned0x1b58c3
        ("alias", &[0xf3, 0x0f, 0xbd, 0xc0]),    // eax,eax
        ("high", &[0xf3, 0x45, 0x0f, 0xbd, 0xf7]), // r14d,r15d
        ("memory", &[0xf3, 0x0f, 0xbd, 0x07]),   // eax,[rdi]
        ("memory_alias", &[0xf3, 0x0f, 0xbd, 0x36]), // esi,[rsi]
        ("memory_high", &[0xf3, 0x45, 0x0f, 0xbd, 0x37]), // r14d,[r15]
        ("rip", &[0xf3, 0x0f, 0xbd, 0x05, 0x20, 0, 0, 0]), // eax,[rip+32]
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes,
            0x100,
            &format!("lzcnt_{name}"),
        )?);
    }
    for bytes in [
        &[0xf3, 0x66, 0x0f, 0xbd, 0xc0][..],
        &[0xf3, 0x48, 0x0f, 0xbd, 0xc0],
        &[0x0f, 0xbd, 0xc0],
    ] {
        if native_ps5::aot::translate_rust_memory_region(bytes, 0, "unsupported_lzcnt").is_ok() {
            return Err("Unimplemented LZCNT width or BSR accepted".into());
        }
    }
    let asm = directory.join("lzcnt.S");
    let obj = directory.join("lzcnt.o");
    let source = directory.join("lzcnt-driver.rs");
    let binary = directory.join("lzcnt-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(&source, DRIVER).map_err(|e| e.to_string())?;
    run(Command::new("xcrun")
        .args(["clang", "-arch", "arm64", "-c"])
        .arg(&asm)
        .arg("-o")
        .arg(&obj))?;
    let profile = archive.parent().ok_or("Missing archive parent")?;
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
unsafe extern "C" {
 fn lzcnt_observed(c:*mut Cpu); fn lzcnt_alias(c:*mut Cpu); fn lzcnt_high(c:*mut Cpu);
 fn lzcnt_memory(c:*mut Cpu); fn lzcnt_memory_alias(c:*mut Cpu); fn lzcnt_memory_high(c:*mut Cpu); fn lzcnt_rip(c:*mut Cpu);
}
type Block = unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu {
 let mut c=Cpu{rip:0x100,fuel:10,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};
 c.registers=[0xfedcba9876543210;16]; c.vectors=[[0xa5;32];16];
 c.x87.push_raw(c.x87_control,&mut c.x87_status,[0;10]).unwrap(); c
}
fn count(value:u32)->u64 { let mut count=0; for bit in (0..32).rev() {if value&(1u32<<bit)!=0 {break} count+=1;} count }
fn untouched(c:&Cpu,b:&Cpu) { assert_eq!(c.vectors,b.vectors); assert_eq!(c.x87,b.x87); assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status)); }
fn result(c:&Cpu,b:&Cpu,dst:usize,value:u32) {
 let result=count(value); let mut regs=b.registers;regs[dst]=result;
 assert_eq!(c.registers,regs);assert_eq!(c.fault,0); assert_eq!(c.fuel,9);
 assert_eq!(c.flags&0x41,u64::from(value==0)|(u64::from(result==0)<<6));
 // Undefined arithmetic flags are deliberately preserved by this implementation.
 assert_eq!(c.flags&!0x41,b.flags&!0x41); untouched(c,b);
}
fn main() {
 let mut checks=0; let mut random=0x6a21e98745bcd30fu64;
 for n in 0..4096 {
  random^=random<<13; random^=random>>7; random^=random<<17;
  let value=match n {0=>0,1=>u64::MAX,2=>0x80000000,3=>0xffffffff00000000,4..=35=>1u64<<(n-4),_=>random};
  for (block,dst,src) in [(lzcnt_observed as Block,6,2),(lzcnt_alias,0,0),(lzcnt_high,14,15)] {
   for flags in [2u64,0xad7] {let mut c=seed();c.flags=flags;c.registers[src]=value;let b=c.clone();unsafe {block(&mut c)};result(&c,&b,dst,value as u32);checks+=1;}
  }
 }
 for offset in [0,1,3,7,15,28] {
  for (block,dst,base,rip) in [(lzcnt_memory as Block,0,7,false),(lzcnt_memory_alias,6,6,false),(lzcnt_memory_high,14,15,false),(lzcnt_rip,0,7,true)] {
   let mut bytes:Vec<u8>=(0..32).map(|i|(i*17+3)as u8).collect();let original=bytes.clone();let address=0x1_00300001u64;
   let mut memory=Memory::default();memory.add(address,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[base]=address+offset;
   if rip {c.bias=address+offset-0x128;}
   let b=c.clone();let value=u32::from_le_bytes(original[offset as usize..offset as usize+4].try_into().unwrap());unsafe{block(&mut c)};result(&c,&b,dst,value);drop(memory);assert_eq!(bytes,original);checks+=1;
  }
 }
 for length in 0..4 {
  for (block,base) in [(lzcnt_memory as Block,7),(lzcnt_memory_alias,6),(lzcnt_memory_high,15)] {
   let mut bytes=vec![0xaa;length];let mut memory=Memory::default();let address=0x1_00300001;
   if length>0 {memory.add(address,&mut bytes,false,false).unwrap();}
   let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[base]=address;let b=c.clone();unsafe{block(&mut c)};
   assert_eq!(c.fault,1);assert_eq!(c.rip,b.rip);assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);untouched(&c,&b);drop(memory);assert_eq!(bytes,vec![0xaa;length]);checks+=1;
  }
 }
 // Whole-range failure at the maximum guest address must also be atomic.
 let mut bytes=[0xff;3];let mut memory=Memory::default();memory.add(u64::MAX-3,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[7]=u64::MAX-3;let b=c.clone();unsafe{lzcnt_memory(&mut c)};assert_eq!(c.fault,1);assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);untouched(&c,&b);checks+=1;
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{lzcnt_observed(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);untouched(&c,&b);checks+=1;
 println!("PASS: {checks} native LZCNT32 checks: observed bytes, all bit positions/random inputs, CF/ZF set+clear, upper32 clearing, alias/high registers, full64 unaligned/RIP-relative readonly memory, four-byte guards, atomic load faults, preserved FP state, and fuel stop");
}
"#;
