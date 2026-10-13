//! Independent repeated-scalar-byte reference for VEX VPBROADCAST B/W/D/Q.
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
    for (width, suffix, opcode) in [
        (1, "b", 0x78u8),
        (2, "w", 0x79),
        (4, "d", 0x58),
        (8, "q", 0x59),
    ] {
        for wide in 0..2 {
            for mode in 0..9 {
                let (dst, src) = match mode {
                    0 => (2, 1),
                    1 => (15, 1),
                    2 => (2, 15),
                    3 => (15, 15),
                    4 => (2, 2),
                    5 => (8, 9),
                    6 => (2, 7),
                    7 => (15, 15),
                    8 => (15, 5),
                    _ => unreachable!(),
                };
                let memory = mode >= 6;
                let bytes3 = 0x79 | wide * 4;
                let mut bytes = vec![
                    0xc4,
                    0x42 | if dst < 8 { 0x80 } else { 0 } | if src < 8 { 0x20 } else { 0 },
                    bytes3,
                    opcode,
                    if memory { 0 } else { 0xc0 } | ((dst & 7) << 3) | (src & 7),
                ];
                if mode == 8 {
                    bytes.extend_from_slice(&32u32.to_le_bytes());
                }
                let decoded = native_ps5::decode::instruction(&bytes, 0x100)?;
                let code = format!(
                    "VEX_Vpbroadcast{suffix}_{}_xmmm{}",
                    if wide == 0 { "xmm" } else { "ymm" },
                    width * 8
                );
                let dest = format!("{}{dst}", if wide == 0 { "XMM" } else { "YMM" });
                if format!("{:?}", decoded.code()) != code
                    || decoded.len() != bytes.len()
                    || decoded.op_count() != 2
                    || format!("{:?}", decoded.op0_kind()) != "Register"
                    || format!("{:?}", decoded.op0_register()) != dest
                    || format!("{:?}", decoded.op1_kind())
                        != if memory { "Memory" } else { "Register" }
                    || (!memory && format!("{:?}", decoded.op1_register()) != format!("XMM{src}"))
                    || (memory
                        && (decoded.memory_size().size() != width
                            || format!("{:?}", decoded.memory_base())
                                != match mode {
                                    6 => "RDI",
                                    7 => "R15",
                                    8 => "RIP",
                                    _ => unreachable!(),
                                }
                            || format!("{:?}", decoded.memory_index()) != "None"))
                    || (mode == 8
                        && decoded.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
                {
                    return Err(format!(
                        "Bad broadcast fixture {width}/{wide}/{mode}: {decoded:?}"
                    ));
                }
                let mut reserved = bytes.clone();
                reserved[2] &= !8;
                if native_ps5::aot::translate_rust_memory_region(
                    &reserved,
                    0x100,
                    "reserved_broadcast",
                )
                .is_ok()
                {
                    return Err("Reserved VEX vvvv accepted".into());
                }
                let mut reserved_w = bytes.clone();
                reserved_w[2] |= 0x80;
                if native_ps5::aot::translate_rust_memory_region(
                    &reserved_w,
                    0x100,
                    "reserved_broadcast_w",
                )
                .is_ok()
                {
                    return Err("Reserved VEX W=1 accepted".into());
                }
                if memory {
                    for prefix in [0x64, 0x65, 0x67] {
                        let mut invalid = vec![prefix];
                        invalid.extend_from_slice(&bytes);
                        if native_ps5::aot::translate_rust_memory_region(
                            &invalid,
                            0x100,
                            "guard_broadcast",
                        )
                        .is_ok()
                        {
                            return Err(format!(
                                "Unsupported broadcast prefix {prefix:x} accepted"
                            ));
                        }
                    }
                }
                let name = format!("packed_broadcast_{width}_{wide}_{mode}");
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                table.push_str(&format!(
                    "({name},{width},{},{dst},{src},{mode},{}),\n",
                    if wide == 0 { 16 } else { 32 },
                    bytes.len()
                ));
            }
        }
    }
    let asm = directory.join("packed-broadcast.S");
    let obj = directory.join("packed-broadcast.o");
    let source = directory.join("packed-broadcast-driver.rs");
    let binary = directory.join("packed-broadcast-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(
        &source,
        DRIVER
            .replace("DECLARATIONS", &declarations)
            .replace("TABLE", &table),
    )
    .map_err(|e| e.to_string())?;
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
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.vectors,b.vectors);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==8{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==7{15}else{7}]=addr;}}
fn expected(b:&Cpu,dst:usize,lanes:usize,scalar:&[u8])->[[u8;32];16]{let mut result=b.vectors;result[dst]=[0;32];for(i,x)in result[dst][..lanes].iter_mut().enumerate(){*x=scalar[i%scalar.len()];}result}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,width,lanes,dst,src,mode,len) in cases{
 if mode<6{for byte in 0..=255u8{for pattern in 0..2{let mut c=seed();for(i,b)in c.vectors[src].iter_mut().enumerate(){*b=if pattern==0{byte}else{byte.wrapping_add((i*53)as u8)};}let b=c.clone();let e=expected(&b,dst,lanes,&b.vectors[src][..width]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e);assert_eq!(c.fuel,19);preserved(&c,&b);checks+=1;}}}
 else{
  // The mapping ends immediately after the scalar, detecting widened loads.
  for offset in [0usize,1,3,7,15]{let mut bytes:Vec<u8>=(0..offset+width).map(|i|(i*53+131)as u8).collect();let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();memory.add(addr,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr+offset as u64);let b=c.clone();let e=expected(&b,dst,lanes,&before[offset..]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e);assert_eq!(c.fuel,19);preserved(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
  for length in 0..width{let mut bytes=vec![0xaa;length];let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();if length>0{memory.add(addr,&mut bytes,false,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(1,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
 }
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VPBROADCAST B/W/D/Q checks across 72 exact decoded XMM/YMM forms: repeated-byte reference, all 256 byte patterns and distinct-byte patterns, low/high/self aliases, upper clearing, exact scalar readonly high/unaligned/RIP loads, every short span, fault atomicity, flags/FP/fuel/preexisting faults");}
"#;
