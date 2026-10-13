//! Independent byte-array merge oracle for VEX VPINSRB/W/D/Q.
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
    let mut forms = 0;
    for (width, w) in [
        (1usize, false),
        (2, false),
        (4, false),
        (8, true),
        (1, true),
        (2, true),
    ] {
        for mode in 0..8 {
            let indices: Vec<u8> = if mode == 0 {
                (0..=255).collect()
            } else {
                vec![0, 1, 2, 3, 7, 8, 15, 16, 31, 127, 128, 255]
            };
            for imm in indices {
                let (dst, merge, src) = match mode {
                    0 => (2, 1, 0),
                    1 => (15, 8, 15),
                    2 => (15, 15, 2),
                    3 => (2, 2, 2),
                    4 => (8, 15, 8),
                    5 => (2, 1, 7),
                    6 => (15, 15, 15),
                    7 => (8, 2, 5),
                    _ => unreachable!(),
                };
                let memory = mode >= 5;
                let map = if width == 2 { 1 } else { 3 };
                let opcode = match width {
                    1 => 0x20,
                    2 => 0xc4,
                    4 | 8 => 0x22,
                    _ => unreachable!(),
                };
                let mut bytes = vec![
                    0xc4,
                    map | 0x40 | if dst < 8 { 0x80 } else { 0 } | if src < 8 { 0x20 } else { 0 },
                    if w { 0x80 } else { 0 } | ((!merge & 15) << 3) | 1,
                    opcode,
                    if memory { 0 } else { 0xc0 } | ((dst & 7) << 3) | (src & 7),
                ];
                if mode == 7 {
                    bytes.extend_from_slice(&32u32.to_le_bytes());
                }
                bytes.push(imm);
                let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                let code = match width {
                    1 => format!("VEX_Vpinsrb_xmm_xmm_r{}m8_imm8", if w { 64 } else { 32 }),
                    2 => format!("VEX_Vpinsrw_xmm_xmm_r{}m16_imm8", if w { 64 } else { 32 }),
                    4 => "VEX_Vpinsrd_xmm_xmm_rm32_imm8".into(),
                    8 => "VEX_Vpinsrq_xmm_xmm_rm64_imm8".into(),
                    _ => unreachable!(),
                };
                let reg = if memory {
                    String::new()
                } else {
                    match (src, w) {
                        (0, false) => "EAX".into(),
                        (0, true) => "RAX".into(),
                        (2, false) => "EDX".into(),
                        (2, true) => "RDX".into(),
                        (n, false) => format!("R{n}D"),
                        (n, true) => format!("R{n}"),
                    }
                };
                if format!("{:?}", d.code()) != code
                    || d.len() != bytes.len()
                    || d.op_count() != 4
                    || format!("{:?}", d.op0_kind()) != "Register"
                    || format!("{:?}", d.op1_kind()) != "Register"
                    || format!("{:?}", d.op0_register()) != format!("XMM{dst}")
                    || format!("{:?}", d.op1_register()) != format!("XMM{merge}")
                    || format!("{:?}", d.op2_kind()) != if memory { "Memory" } else { "Register" }
                    || (!memory && format!("{:?}", d.op2_register()) != reg)
                    || format!("{:?}", d.op3_kind()) != "Immediate8"
                    || d.immediate8() != imm
                    || (memory
                        && (d.memory_size().size() != width
                            || format!("{:?}", d.memory_base())
                                != match mode {
                                    5 => "RDI",
                                    6 => "R15",
                                    7 => "RIP",
                                    _ => unreachable!(),
                                }
                            || format!("{:?}", d.memory_index()) != "None"))
                    || (mode == 7 && d.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
                {
                    return Err(format!(
                        "Bad insertion fixture {width}/{w}/{mode}/{imm}: {d:?}"
                    ));
                }
                if imm == 0 {
                    let mut invalid = bytes.clone();
                    invalid[2] |= 4;
                    if native_ps5::aot::translate_rust_memory_region(
                        &invalid,
                        0x100,
                        "reserved_insert",
                    )
                    .is_ok()
                    {
                        return Err("Reserved VPINSR L=1 accepted".into());
                    }
                    if memory {
                        for prefix in [0x64, 0x65, 0x67] {
                            let mut invalid = vec![prefix];
                            invalid.extend_from_slice(&bytes);
                            if native_ps5::aot::translate_rust_memory_region(
                                &invalid,
                                0x100,
                                "guard_insert",
                            )
                            .is_ok()
                            {
                                return Err(format!("Unsupported insertion prefix {prefix:x}"));
                            }
                        }
                    }
                }
                let name = format!("packed_insert_{width}_{}_{mode}_{imm}", u8::from(w));
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                table.push_str(&format!(
                    "({name},{width},{dst},{merge},{src},{mode},{imm},{}),\n",
                    bytes.len()
                ));
                forms += 1;
            }
        }
    }
    let asm = directory.join("packed-insert.S");
    let obj = directory.join("packed-insert.o");
    let source = directory.join("packed-insert-driver.rs");
    let binary = directory.join("packed-insert-driver");
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
    println!("Decoded {forms} VPINSR fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.vectors,b.vectors);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==7{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==6{15}else{7}]=addr;}}
fn expected(b:&Cpu,dst:usize,merge:usize,imm:usize,scalar:&[u8])->[[u8;32];16]{let mut result=b.vectors;let mut output=[0;32];output[..16].copy_from_slice(&b.vectors[merge][..16]);let at=(imm%(16/scalar.len()))*scalar.len();output[at..at+scalar.len()].copy_from_slice(scalar);result[dst]=output;result}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,width,dst,merge,src,mode,imm,len) in cases{
 if mode<5{for value in [0,1,u64::MAX,0x8000000000000000,0xffffffff00000000,0x0123456789abcdef,0xfedcba9876543210,0x80ff7f008001ffff]{let mut c=seed();c.registers[src]=value;let b=c.clone();let e=expected(&b,dst,merge,imm,&value.to_le_bytes()[..width]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e);assert_eq!(c.fuel,19);preserved(&c,&b);checks+=1;}}
 else{for offset in [0usize,1,3,7,15]{let mut bytes:Vec<u8>=(0..offset+width).map(|i|(i*53+131)as u8).collect();let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();memory.add(addr,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr+offset as u64);let b=c.clone();let e=expected(&b,dst,merge,imm,&before[offset..]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e);assert_eq!(c.fuel,19);preserved(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
  for length in 0..width{let mut bytes=vec![0xaa;length];let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();if length>0{memory.add(addr,&mut bytes,false,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(1,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
 }
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VPINSR B/W/D/Q byte-merge checks: all256 immediate encodings, masked lane indices, merge/destination aliases, high registers, upperYMMzero, exact readonly high/unaligned/RIP scalar reads, all short spans, atomic faults, flags/FP/fuel/preexisting faults");}
"#;
