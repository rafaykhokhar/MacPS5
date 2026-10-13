//! Independent repeated-byte reference for VEX scalar floating-point broadcasts.
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
    for (width, suffix, opcode) in [(4usize, "ss", 0x18u8), (8, "sd", 0x19)] {
        for wide in if width == 8 { 1..2 } else { 0..2 } {
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
                    "VEX_Vbroadcast{suffix}_{}_{}",
                    if wide == 0 { "xmm" } else { "ymm" },
                    if memory {
                        format!("m{}", width * 8)
                    } else {
                        "xmm".to_owned()
                    }
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
                if width == 8 {
                    let mut invalid = bytes.clone();
                    invalid[2] &= !4;
                    if native_ps5::aot::translate_rust_memory_region(&invalid, 0x100, "bad_sd128")
                        .is_ok()
                    {
                        return Err("Unsupported VBROADCASTSD128 accepted".into());
                    }
                }
                for prefix in [0x66, 0xf2, 0xf3, 0xf0] {
                    let mut invalid = vec![prefix];
                    invalid.extend_from_slice(&bytes);
                    if native_ps5::aot::translate_rust_memory_region(
                        &invalid,
                        0x100,
                        "bad_broadcast_prefix",
                    )
                    .is_ok()
                    {
                        return Err(format!("Unsupported prefix accepted: {invalid:02x?}"));
                    }
                }
                for invalid in [
                    vec![0x66, 0x0f, 0x38, opcode, 0xc1],
                    vec![
                        0x62,
                        0xf2,
                        if width == 8 { 0xfd } else { 0x7d },
                        0x28,
                        opcode,
                        0xc1,
                    ],
                ] {
                    if native_ps5::aot::translate_rust_memory_region(
                        &invalid,
                        0x100,
                        "unsupported_broadcast_form",
                    )
                    .is_ok()
                    {
                        return Err(format!(
                            "Unsupported broadcast form accepted: {invalid:02x?}"
                        ));
                    }
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
                let name = format!("scalar_broadcast_{width}_{wide}_{mode}");
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                forms += 1;
                table.push_str(&format!(
                    "({name},{width},{},{dst},{src},{mode},{}),\n",
                    if wide == 0 { 16 } else { 32 },
                    bytes.len()
                ));
            }
        }
    }
    let asm = directory.join("scalar-broadcast.S");
    let obj = directory.join("scalar-broadcast.o");
    let source = directory.join("scalar-broadcast-driver.rs");
    let binary = directory.join("scalar-broadcast-driver");
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
    println!("Decoded {forms} scalar broadcast fixtures");
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
fn inputs(width:usize)->Vec<u64>{let mut values=if width==4{vec![0,0x80000000,1,0x007fffff,0x00800000,0x7f800000,0xff800000,0x7f800001,0x7fc12345,0xffcabcde,0x7f7fffff,0xffffffff]}else{vec![0,0x8000000000000000,1,0x000fffffffffffff,0x0010000000000000,0x7ff0000000000000,0xfff0000000000000,0x7ff0000000000001,0x7ff8123456789abc,0xfff8fedcba987654,0x7fefffffffffffff,u64::MAX]};for bit in 0..width*8{values.push(1u64<<bit);values.push(!(1u64<<bit));}let mut random=0x831725764bada557u64;for _ in 0..128{random^=random<<13;random^=random>>7;random^=random<<17;values.push(random);}values}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,width,lanes,dst,src,mode,len) in cases{
 if mode<6{for bits in inputs(width){for poison in [0u8,255]{for control in [0u32,0xffff,0x10000]{let mut c=seed();c.mxcsr=control;c.vectors[src]=[poison;32];c.vectors[src][..width].copy_from_slice(&bits.to_le_bytes()[..width]);let b=c.clone();let e=expected(&b,dst,lanes,&b.vectors[src][..width]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e);assert_eq!(c.fuel,19);preserved(&c,&b);checks+=1;}}}}
 else{
  // The mapping ends immediately after the scalar, detecting widened loads.
  for offset in [0usize,1,3,7,15]{for bits in inputs(width){let mut bytes=vec![0xa5;offset+width];bytes[offset..].copy_from_slice(&bits.to_le_bytes()[..width]);let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();memory.add(addr,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr+offset as u64);let b=c.clone();let e=expected(&b,dst,lanes,&before[offset..]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e);assert_eq!(c.fuel,19);preserved(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}}
  for length in 0..width{let mut bytes=vec![0xaa;length];let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();if length>0{memory.add(addr,&mut bytes,false,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(1,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
 }
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VBROADCASTSS/SD checks: 6 exact decoded codes, bit-copy oracle with NaN payloads/subnormals/signedzero/infinity/walkingbits/random, unused upper-source poison, low/high/self aliases, XMM upperzero, exact4/8byte readonly high/unaligned/RIP loads, everyshortspan, flags/FP/fuel/preexisting faults");}
"#;
