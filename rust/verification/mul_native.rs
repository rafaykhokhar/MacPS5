//! Unsigned MUL native verification. Independent u128 product reference.
//! AMD24594 Rev3.31 MUL: CF/OF indicate nonzero upper product half;
//! remaining arithmetic flags use the prototype's preserve policy.
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
    for width in [8usize, 16, 32, 64] {
        for mode in 0..9 {
            if mode == 4 && width != 8 {
                continue;
            }
            let mut bytes = Vec::new();
            if width == 16 {
                bytes.push(0x66);
            }
            if width == 64 || mode == 3 {
                bytes.push(0x40 | if width == 64 { 8 } else { 0 } | if mode == 3 { 1 } else { 0 });
            }
            bytes.push(if width == 8 { 0xf6 } else { 0xf7 });
            bytes.push(match mode {
                0 => 0xe1,
                1 => 0xe0,
                2 => 0xe2,
                3 => 0xe7,
                4 => 0xe4,
                5 => 0x27,
                6 => 0x20,
                7 => 0x22,
                8 => 0x25,
                _ => unreachable!(),
            });
            if mode == 8 {
                bytes.extend_from_slice(&32u32.to_le_bytes());
            }
            let decoded = native_ps5::decode::instruction(&bytes, 0x100)?;
            let register = match (mode, width) {
                (0, 8) => "CL",
                (0, 16) => "CX",
                (0, 32) => "ECX",
                (0, 64) => "RCX",
                (1, 8) => "AL",
                (1, 16) => "AX",
                (1, 32) => "EAX",
                (1, 64) => "RAX",
                (2, 8) => "DL",
                (2, 16) => "DX",
                (2, 32) => "EDX",
                (2, 64) => "RDX",
                (3, 8) => "R15L",
                (3, 16) => "R15W",
                (3, 32) => "R15D",
                (3, 64) => "R15",
                (4, 8) => "AH",
                _ => "None",
            };
            if format!("{:?}", decoded.mnemonic()) != "Mul"
                || decoded.len() != bytes.len()
                || decoded.op_count() != 1
                || format!("{:?}", decoded.op0_kind())
                    != if mode < 5 { "Register" } else { "Memory" }
                || (mode < 5
                    && (format!("{:?}", decoded.op0_register()) != register
                        || decoded.op0_register().size() * 8 != width))
                || (mode >= 5
                    && (decoded.memory_size().size() * 8 != width
                        || format!("{:?}", decoded.memory_base())
                            != match mode {
                                5 => "RDI",
                                6 => "RAX",
                                7 => "RDX",
                                _ => "RIP",
                            }
                        || format!("{:?}", decoded.memory_index()) != "None"))
                || (mode == 8 && decoded.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
            {
                return Err(format!(
                    "MUL fixture encoding mismatch {bytes:02x?}: {decoded:?}"
                ));
            }
            let name = format!("mul_{width}_{mode}");
            assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                &bytes, 0x100, &name,
            )?);
            declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
            table.push_str(&format!(
                "({name} as Block,{width},{mode},{}),\n",
                decoded.next_ip() + 32
            ));
        }
    }
    for bytes in [
        &[0x67, 0xf7, 0x24, 0x25, 0, 0, 0, 0][..],
        &[0x64, 0xf7, 0x27][..],
        &[0xf0, 0xf7, 0xe0][..],
        &[0xf3, 0xf7, 0xe0][..],
    ] {
        if native_ps5::aot::translate_rust_memory_region(bytes, 0x100, "unsupported_mul").is_ok() {
            return Err(format!("Unsupported MUL accepted {bytes:02x?}"));
        }
    }
    let driver = DRIVER
        .replace("DECLARATIONS", &declarations)
        .replace("CASES", &table);
    let asm = directory.join("mul.S");
    let obj = directory.join("mul.o");
    let source = directory.join("mul-driver.rs");
    let binary = directory.join("mul-driver");
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
unsafe extern "C" {DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:10,flags:0xad7,mxcsr:0x3fa0,..Cpu::default()};c.registers=[0xfedcba9876543210;16];c.vectors=[[0xa5;32];16];c}
fn untouched(c:&Cpu,b:&Cpu){assert_eq!(c.vectors,b.vectors);assert_eq!(c.x87,b.x87);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));}
fn mask(width:u32)->u64{if width==64{u64::MAX}else{(1u64<<width)-1}}
fn result(c:&Cpu,b:&Cpu,width:u32,source:u64){
 let product=u128::from(b.registers[0]&mask(width))*u128::from(source&mask(width));
 let low=product as u64&mask(width);let high=(product>>width)as u64;
 let mut registers=b.registers;
 match width{
  8=>{registers[0]=(registers[0]&!65535)|(product as u64);}
  16=>{registers[0]=(registers[0]&!65535)|low;registers[2]=(registers[2]&!65535)|high;}
  _=>{registers[0]=low;registers[2]=high;}
 }
 assert_eq!((c.fault,c.fuel),(0,9));assert_eq!(c.registers,registers,"width{width} source{source:x}");
 assert_eq!(c.flags,(b.flags&!0x801)|if high!=0{0x801}else{0});untouched(c,b);
}
fn fault(c:&Cpu,b:&Cpu){assert_eq!((c.fault,c.fuel,c.rip),(1,9,b.rip));assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);untouched(c,b);}
fn main(){
 let cases:&[(Block,u32,u32,u64)]=&[CASES];let mut checks=0usize;let mut random=0x8d7b243f641903abu64;
 for &(block,width,mode,rip_address)in cases{
  if mode<5{
   let iterations=if width==8{65536}else{4096};
   for n in 0..iterations{
    random^=random<<13;random^=random>>7;random^=random<<17;
    let a=if width==8{(n>>8)as u64}else{match n{0=>0,1=>u64::MAX,2=>1,3=>1u64<<(width-1),_=>random}};
    let source=if width==8{n as u64&255}else{match n{0=>u64::MAX,1=>u64::MAX,2=>1,3=>2,_=>random.rotate_left(27)}};
    for flags in [2,0xad7]{
     let mut c=seed();c.flags=flags;c.registers[0]=(c.registers[0]&!mask(width))|(a&mask(width));
     let src=match mode{0=>1,1|4=>0,2=>2,3=>15,_=>unreachable!()};
     if mode==4{c.registers[0]=(c.registers[0]&!0xff00)|(source<<8);}else if mode!=1{c.registers[src]=(c.registers[src]&!mask(width))|(source&mask(width));}
     let b=c.clone();let actual_source=if mode==4{(b.registers[0]>>8)&255}else{b.registers[src]&mask(width)};
     unsafe{block(&mut c)};result(&c,&b,width,actual_source);checks+=1;
    }
   }
  }else{
   let length=(width/8)as usize;
   for offset in [0usize,1,3,7] {for source in [0,u64::MAX,1,1u64<<(width-1),0x9837aacc5501ef32] {for flags in [2,0xad7]{
    let mut bytes=[0xa5;24];bytes[offset..offset+length].copy_from_slice(&source.to_le_bytes()[..length]);let original=bytes;
    let address=0x1_00300001u64;let mut memory=Memory::default();memory.add(address,&mut bytes,false,false).unwrap();
    let mut c=seed();c.flags=flags;c.memory=(&mut memory as *mut Memory<'_>)as u64;
    match mode{5=>c.registers[7]=address+offset as u64,6=>c.registers[0]=address+offset as u64,7=>c.registers[2]=address+offset as u64,_=>c.bias=address+offset as u64-rip_address}
    let b=c.clone();unsafe{block(&mut c)};result(&c,&b,width,source);drop(memory);assert_eq!(bytes,original);checks+=1;
   }}}
   for size in 0..length{
    let mut bytes=vec![0xa5;size];let original=bytes.clone();let address=0x1_00300001;
    let mut memory=Memory::default();if size!=0{memory.add(address,&mut bytes,false,false).unwrap();}
    let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;
    match mode{5=>c.registers[7]=address,6=>c.registers[0]=address,7=>c.registers[2]=address,_=>c.bias=address-rip_address}
    let b=c.clone();unsafe{block(&mut c)};fault(&c,&b);drop(memory);assert_eq!(bytes,original);checks+=1;
   }
   // Overflowing access at the top of the guest address space, no valid span.
   let mut memory=Memory::default();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;
   match mode{5=>c.registers[7]=u64::MAX,6=>c.registers[0]=u64::MAX,7=>c.registers[2]=u64::MAX,_=>c.bias=u64::MAX-rip_address}
   let b=c.clone();unsafe{block(&mut c)};fault(&c,&b);checks+=1;
  }
  let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);untouched(&c,&b);checks+=1;
 }
 println!("PASS: {checks} native unsigned MUL8/16/32/64 checks: exhaustive byte pairs, edge/random wide inputs, u128 products, accumulator/high-half aliases, AH/high registers, partial writes, CF/OF and preserved undefined flags, readonly unaligned high/RIP/aliased-base memory, atomic short/unmapped/overflow loads, FP/fuel");
}
"#;
