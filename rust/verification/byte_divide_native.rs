//! DIV/IDIV8 native verifier; independent u32/i32 AX arithmetic reference.
//! #DE is modeled as fault13; arithmetic flags follow deterministic preservation.
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
    let mut assembly = String::new();
    let mut declarations = String::new();
    let mut table = String::new();
    for signed in [false, true] {
        for mode in 0..8 {
            let mut bytes = Vec::new();
            if mode == 3 {
                bytes.push(0x41);
            }
            bytes.push(0xf6);
            let operand = match mode {
                0 => 0xc1,
                1 => 0xc0,
                2 => 0xc4,
                3 => 0xc7,
                4 => 0x07,
                5 => 0x00,
                6 => 0x02,
                7 => 0x05,
                _ => unreachable!(),
            };
            bytes.push(operand | if signed { 7 << 3 } else { 6 << 3 });
            if mode == 7 {
                bytes.extend_from_slice(&32u32.to_le_bytes());
            }
            let decoded = native_ps5::decode::instruction(&bytes, 0x100)?;
            if format!("{:?}", decoded.mnemonic()) != if signed { "Idiv" } else { "Div" }
                || decoded.len() != bytes.len()
                || decoded.op_count() != 1
                || format!("{:?}", decoded.op0_kind())
                    != if mode < 4 { "Register" } else { "Memory" }
                || (mode < 4
                    && (decoded.op0_register().size() != 1
                        || format!("{:?}", decoded.op0_register())
                            != match mode {
                                0 => "CL",
                                1 => "AL",
                                2 => "AH",
                                _ => "R15L",
                            }))
                || (mode >= 4
                    && (decoded.memory_size().size() != 1
                        || format!("{:?}", decoded.memory_base())
                            != match mode {
                                4 => "RDI",
                                5 => "RAX",
                                6 => "RDX",
                                _ => "RIP",
                            }
                        || format!("{:?}", decoded.memory_index()) != "None"))
                || (mode == 7 && decoded.ip_rel_memory_address() != decoded.next_ip() + 32)
            {
                return Err(format!(
                    "Byte divide encoding mismatch {bytes:02x?}: {decoded:?}"
                ));
            }
            let name = format!("byte_divide_{}_{mode}", u8::from(signed));
            assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                &bytes, 0x100, &name,
            )?);
            declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
            table.push_str(&format!(
                "({name} as Block,{signed},{mode},{}),\n",
                decoded.next_ip() + 32
            ));
        }
    }
    for bytes in [
        &[0x67, 0xf6, 0x34, 0x25, 0, 0, 0, 0][..],
        &[0x64, 0xf6, 0x37][..],
        &[0xf0, 0xf6, 0xf0][..],
        &[0xf3, 0xf6, 0xf0][..],
    ] {
        if native_ps5::aot::translate_rust_memory_region(bytes, 0x100, "unsupported_div8").is_ok() {
            return Err(format!("Unsupported byte divide accepted {bytes:02x?}"));
        }
    }
    let driver = DRIVER
        .replace("DECLARATIONS", &declarations)
        .replace("CASES", &table);
    let asm = directory.join("byte-divide.S");
    let obj = directory.join("byte-divide.o");
    let source = directory.join("byte-divide-driver.rs");
    let binary = directory.join("byte-divide-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(&source, driver).map_err(|e| e.to_string())?;
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
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:10,flags:0xad7,mxcsr:0x3fa0,..Cpu::default()};c.registers=[0xfedcba9876543210;16];c.vectors=[[0xa5;32];16];c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.flags,b.flags);assert_eq!(c.vectors,b.vectors);assert_eq!(c.x87,b.x87);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));}
fn reference(ax:u16,divisor:u8,signed:bool)->Option<u16>{
 if divisor==0{return None;}
 if signed{
  let dividend=i32::from(ax as i16);let divisor=i32::from(divisor as i8);let q=dividend/divisor;
  if !(-128..=127).contains(&q){return None;}
  let r=dividend%divisor;Some(u16::from(q as u8)|(u16::from(r as u8)<<8))
 }else{
  let dividend=u32::from(ax);let divisor=u32::from(divisor);let q=dividend/divisor;
  if q>255{return None;}let r=dividend%divisor;Some(q as u16|((r as u16)<<8))
 }
}
fn result(c:&Cpu,b:&Cpu,divisor:u8,signed:bool){
 let out=reference(b.registers[0]as u16,divisor,signed);let mut regs=b.registers;
 if let Some(ax)=out{regs[0]=(regs[0]&!65535)|u64::from(ax);}
 assert_eq!(c.fault,if out.is_some(){0}else{13},"ax{:04x} divisor{divisor:02x} signed{signed}",b.registers[0]as u16);
 assert_eq!(c.registers,regs);assert_eq!(c.fuel,9);if out.is_none(){assert_eq!(c.rip,b.rip);}preserved(c,b);
}
fn set_address(c:&mut Cpu,mode:u32,address:u64,rip:u64){match mode{4=>c.registers[7]=address,5=>c.registers[0]=address,6=>c.registers[2]=address,_=>c.bias=address-rip}}
fn main(){
 let cases:&[(Block,bool,u32,u64)]=&[CASES];let mut checks=0usize;let mut random=0x739a4051bf862dceu64;
 for &(block,signed,mode,rip)in cases{
  if mode<4{
   let count=if mode==3{8192}else{65536};
   for n in 0..count{
    random^=random<<13;random^=random>>7;random^=random<<17;
    let ax=if mode==3{random as u16}else{n as u16};
    let divisors:Vec<u8>=if mode==0{vec![0,1,2,127,128,255]}else if mode==3{vec![n as u8]}else{vec![0]};
    for divisor in divisors{
     let mut c=seed();c.flags=if n&1==0{2}else{0xad7};c.registers[0]=(c.registers[0]&!65535)|u64::from(ax);
     if mode==0{c.registers[1]=(c.registers[1]&!255)|u64::from(divisor);}if mode==3{c.registers[15]=(c.registers[15]&!255)|u64::from(divisor);}
     let actual=match mode{1=>c.registers[0]as u8,2=>(c.registers[0]>>8)as u8,_=>divisor};
     let b=c.clone();unsafe{block(&mut c)};result(&c,&b,actual,signed);checks+=1;
    }
   }
  }else{
   for ax in [0u16,1,127,128,255,256,32767,32768,65535]{for divisor in 0..=255u8{for offset in [0usize,1,3]{
    let mut bytes=[0xa5;8];bytes[offset]=divisor;let original=bytes;let address=0x1_00301201u64;
    let mut memory=Memory::default();memory.add(address,&mut bytes,false,false).unwrap();
    let mut c=seed();c.registers[0]=(c.registers[0]&!65535)|u64::from(ax);c.flags=if divisor&1==0{2}else{0xad7};
    set_address(&mut c,mode,address+offset as u64,rip);c.memory=(&mut memory as *mut Memory<'_>)as u64;let b=c.clone();
    unsafe{block(&mut c)};result(&c,&b,divisor,signed);drop(memory);assert_eq!(bytes,original);checks+=1;
   }}}
   for address in [0x1_00301201,u64::MAX]{
    // A failed operand load takes priority over a divide error; AX already
    // overflows division by many byte divisors, but the divisor is unmapped.
    let mut memory=Memory::default();let mut c=seed();c.registers[0]=u64::MAX;set_address(&mut c,mode,address,rip);
    c.memory=(&mut memory as *mut Memory<'_>)as u64;let b=c.clone();unsafe{block(&mut c)};
    assert_eq!((c.fault,c.fuel,c.rip),(1,9,0x100));assert_eq!(c.registers,b.registers);preserved(&c,&b);checks+=1;
   }
  }
  let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));assert_eq!(c.registers,b.registers);preserved(&c,&b);checks+=1;
  for existing_fault in [1,13,27]{let mut c=seed();c.fault=existing_fault;let b=c.clone();unsafe{block(&mut c)};
   assert_eq!((c.fault,c.fuel,c.rip,c.stop),(existing_fault,10,0x100,b.stop));assert_eq!(c.registers,b.registers);preserved(&c,&b);checks+=1;}
 }
 println!("PASS: {checks} native DIV/IDIV8 checks: exhaustive AX against six divisors, random all-byte divisors, independent signed/unsigned quotient/remainder, AL/AH/high aliases, precise divide-error atomicity, readonly high/RIP/aliased-base memory, load-fault priority, preserved flags/FP, fuel and preexisting faults");
}
"#;
