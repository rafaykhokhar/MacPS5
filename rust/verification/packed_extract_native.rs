//! Independent lane-byte extraction oracle for both VEX VPEXTRW encodings and B/D/Q.
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
    for (width, w, old) in [
        (1usize, false, false),
        (2, false, false),
        (4, false, false),
        (8, true, false),
        (1, true, false),
        (2, true, false),
        (2, false, true),
        (2, true, true),
    ] {
        for mode in 0..if old { 5 } else { 8 } {
            let indices: Vec<u8> = if mode == 0 {
                (0..=255).collect()
            } else {
                vec![0, 1, 2, 3, 7, 8, 15, 16, 31, 127, 128, 255]
            };
            for imm in indices {
                let (dst, src) = match mode {
                    0 => (0, 1),
                    1 => (15, 8),
                    2 => (2, 15),
                    3 => (15, 15),
                    4 => (8, 2),
                    5 => (7, 2),
                    6 => (15, 15),
                    7 => (5, 8),
                    _ => unreachable!(),
                };
                let memory = mode >= 5;
                let (reg, rm) = if old { (dst, src) } else { (src, dst) };
                let opcode = if old {
                    0xc5
                } else {
                    match width {
                        1 => 0x14,
                        2 => 0x15,
                        4 | 8 => 0x16,
                        _ => unreachable!(),
                    }
                };
                let mut bytes = vec![
                    0xc4,
                    if old { 1 } else { 3 }
                        | 0x40
                        | if reg < 8 { 0x80 } else { 0 }
                        | if rm < 8 { 0x20 } else { 0 },
                    0x79 | if w { 0x80 } else { 0 },
                    opcode,
                    if memory { 0 } else { 0xc0 } | ((reg & 7) << 3) | (rm & 7),
                ];
                if mode == 7 {
                    bytes.extend_from_slice(&32u32.to_le_bytes());
                }
                bytes.push(imm);
                let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                let code = if old {
                    format!("VEX_Vpextrw_r{}_xmm_imm8", if w { 64 } else { 32 })
                } else {
                    match width {
                        1 => format!("VEX_Vpextrb_r{}m8_xmm_imm8", if w { 64 } else { 32 }),
                        2 => format!("VEX_Vpextrw_r{}m16_xmm_imm8", if w { 64 } else { 32 }),
                        4 => "VEX_Vpextrd_rm32_xmm_imm8".into(),
                        8 => "VEX_Vpextrq_rm64_xmm_imm8".into(),
                        _ => unreachable!(),
                    }
                };
                let dest = if memory {
                    String::new()
                } else {
                    match (dst, w) {
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
                    || d.op_count() != 3
                    || format!("{:?}", d.op0_kind()) != if memory { "Memory" } else { "Register" }
                    || (!memory && format!("{:?}", d.op0_register()) != dest)
                    || format!("{:?}", d.op1_kind()) != "Register"
                    || format!("{:?}", d.op1_register()) != format!("XMM{src}")
                    || format!("{:?}", d.op2_kind()) != "Immediate8"
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
                        "Bad extraction fixture {width}/{w}/{old}/{mode}/{imm}: {d:?}"
                    ));
                }
                if imm == 0 {
                    if old && mode == 0 {
                        let mut invalid = bytes.clone();
                        invalid[4] &= 0x3f;
                        if native_ps5::aot::translate_rust_memory_region(
                            &invalid,
                            0x100,
                            "invalid_old_word_memory",
                        )
                        .is_ok()
                        {
                            return Err("Register-only VPEXTRW memory encoding accepted".into());
                        }
                    }
                    for (bit, set) in [(4, true), (8, false)] {
                        let mut invalid = bytes.clone();
                        if set {
                            invalid[2] |= bit;
                        } else {
                            invalid[2] &= !bit;
                        }
                        if native_ps5::aot::translate_rust_memory_region(
                            &invalid,
                            0x100,
                            "reserved_extract",
                        )
                        .is_ok()
                        {
                            return Err(format!("Reserved VPEXTR encoding accepted: {bit}/{set}"));
                        }
                    }
                    if memory {
                        for prefix in [0x64, 0x65, 0x67] {
                            let mut invalid = vec![prefix];
                            invalid.extend_from_slice(&bytes);
                            if native_ps5::aot::translate_rust_memory_region(
                                &invalid,
                                0x100,
                                "guard_extract",
                            )
                            .is_ok()
                            {
                                return Err(format!("Unsupported extraction prefix {prefix:x}"));
                            }
                        }
                    }
                }
                let name = format!(
                    "packed_extract_{width}_{}_{}_{mode}_{imm}",
                    u8::from(w),
                    u8::from(old)
                );
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                table.push_str(&format!(
                    "({name},{width},{dst},{src},{mode},{imm},{}),\n",
                    bytes.len()
                ));
                forms += 1;
            }
        }
    }
    let asm = directory.join("packed-extract.S");
    let obj = directory.join("packed-extract.o");
    let source = directory.join("packed-extract-driver.rs");
    let binary = directory.join("packed-extract-driver");
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
    println!("Decoded {forms} VPEXTR fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.vectors,b.vectors);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.registers,b.registers);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==7{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==6{15}else{7}]=addr;}}
fn scalar(c:&Cpu,src:usize,imm:usize,width:usize)->Vec<u8>{let at=(imm%(16/width))*width;c.vectors[src][at..at+width].to_vec()}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,width,dst,src,mode,imm,len) in cases{
 if mode<5{for pattern in 0..8{let mut c=seed();for(i,x)in c.vectors[src].iter_mut().enumerate(){*x=match pattern{0=>0,1=>255,2=>128,3=>1,_=>(i*53+pattern*31)as u8};}let b=c.clone();let mut value=[0;8];value[..width].copy_from_slice(&scalar(&b,src,imm,width));let mut regs=b.registers;regs[dst]=u64::from_le_bytes(value);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.registers,regs);assert_eq!(c.fuel,19);preserved(&c,&b);checks+=1;}}
 else{for offset in [0usize,1,3,7,15]{for tail in [0usize,7]{let mut bytes:Vec<u8>=(0..offset+width+tail).map(|i|(i*53+131)as u8).collect();let mut expected=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();memory.add(addr,&mut bytes,true,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr+offset as u64);let b=c.clone();expected[offset..offset+width].copy_from_slice(&scalar(&b,src,imm,width));unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.fuel,19);unchanged(&c,&b);drop(memory);assert_eq!(bytes,expected);checks+=1;}}
  for length in 0..width{let mut bytes=vec![0xaa;length];let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();if length>0{memory.add(addr,&mut bytes,true,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(2,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
  let mut bytes=vec![0xaa;width+2];let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();memory.add(addr,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr+1);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(2,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;
 }
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VPEXTR B/W/D/Q lane-byte checks: both word encodings, all256 immediates, zero-extended GPR output, high registers, unchanged vectors, exact unaligned/high/RIP stores, every short span and readonly store fault, flags/FP/fuel/preexisting faults");}
"#;
