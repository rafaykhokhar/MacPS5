//! Independent scalar-copy and merge oracle for both VEX VMOVSS/VMOVSD opcodes.
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
    for width in [4usize, 8] {
        for store in 0..2 {
            for variant in 0..4 {
                for mode in 0..12 {
                    let (dst, merge, source) = match mode {
                        0 => (0, 1, 2),
                        1 => (15, 8, 9),
                        2 => (1, 1, 2),
                        3 => (2, 1, 2),
                        4 => (15, 15, 15),
                        5 => (0, 1, 1),
                        6 => (8, 15, 2),
                        7 => (2, 1, 15),
                        8 => (9, 9, 8),
                        9 => (2, 0, 7),
                        10 => (15, 0, 15),
                        11 => (8, 0, 5),
                        _ => unreachable!(),
                    };
                    let memory = mode >= 9;
                    let (reg, rm) = if memory || store == 0 {
                        (dst, source)
                    } else {
                        (source, dst)
                    };
                    let pp = if width == 4 { 2 } else { 3 };
                    let opcode = 0x10 + store;
                    let mut bytes = vec![
                        0xc4,
                        0x41 | if reg < 8 { 0x80 } else { 0 } | if rm < 8 { 0x20 } else { 0 },
                        (if memory { 0x78 } else { (!merge & 15) << 3 })
                            | pp
                            | (variant % 2) * 4
                            | (variant / 2) * 128,
                        opcode,
                        if memory { 0 } else { 0xc0 } | ((reg & 7) << 3) | (rm & 7),
                    ];
                    if mode == 11 {
                        bytes.extend_from_slice(&32u32.to_le_bytes());
                    }
                    let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                    let mnemonic = if width == 4 { "ss" } else { "sd" };
                    let code = if memory {
                        if store == 0 {
                            format!("VEX_Vmov{mnemonic}_xmm_m{}", width * 8)
                        } else {
                            format!("VEX_Vmov{mnemonic}_m{}_xmm", width * 8)
                        }
                    } else {
                        format!(
                            "VEX_Vmov{mnemonic}_xmm_xmm_xmm{}",
                            if store == 0 { "" } else { "_0F11" }
                        )
                    };
                    let mut valid = format!("{:?}", d.code()) == code
                        && d.len() == bytes.len()
                        && d.op_count() == if memory { 2 } else { 3 };
                    if memory {
                        let regop = if store == 0 { 0 } else { 1 };
                        let memop = if store == 0 { 1 } else { 0 };
                        valid &= format!("{:?}", d.op_kind(memop)) == "Memory"
                            && format!("{:?}", d.op_register(regop)) == format!("XMM{dst}")
                            && d.memory_size().size() == width
                            && format!("{:?}", d.memory_base())
                                == match mode {
                                    9 => "RDI",
                                    10 => "R15",
                                    11 => "RIP",
                                    _ => unreachable!(),
                                }
                            && format!("{:?}", d.memory_index()) == "None";
                        if mode == 11 {
                            valid &= d.ip_rel_memory_address() == 0x100 + bytes.len() as u64 + 32;
                        }
                    } else {
                        valid &= format!("{:?}", d.op0_register()) == format!("XMM{dst}")
                            && format!("{:?}", d.op1_register()) == format!("XMM{merge}")
                            && format!("{:?}", d.op2_register()) == format!("XMM{source}");
                    }
                    if !valid {
                        return Err(format!(
                            "Bad scalar move {width}/{store}/{variant}/{mode}: {d:?}"
                        ));
                    }
                    let mut invalids = vec![
                        vec![if width == 4 { 0xf3 } else { 0xf2 }, 0x0f, opcode, 0xc1],
                        vec![
                            0x62,
                            0xf1,
                            if width == 4 { 0x7e } else { 0xff },
                            0x08,
                            opcode,
                            0xc1,
                        ],
                    ];
                    if memory {
                        let mut reserved = bytes.clone();
                        reserved[2] &= !8;
                        invalids.push(reserved);
                    }
                    for prefix in if memory {
                        vec![0x66, 0xf2, 0xf3, 0xf0, 0x64, 0x65, 0x67]
                    } else {
                        vec![0x66, 0xf2, 0xf3, 0xf0]
                    } {
                        let mut invalid = vec![prefix];
                        invalid.extend_from_slice(&bytes);
                        invalids.push(invalid);
                    }
                    for invalid in invalids {
                        if native_ps5::aot::translate_rust_memory_region(
                            &invalid,
                            0x100,
                            "unsupported_scalar_move",
                        )
                        .is_ok()
                        {
                            return Err(format!(
                                "Unsupported scalar move accepted: {invalid:02x?}"
                            ));
                        }
                    }
                    let name = format!("scalar_move_{width}_{store}_{variant}_{mode}");
                    assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                        &bytes, 0x100, &name,
                    )?);
                    declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                    table.push_str(&format!(
                        "({name},{width},{store},{dst},{merge},{source},{mode},{}),\n",
                        bytes.len()
                    ));
                    forms += 1;
                }
            }
        }
    }
    let asm = directory.join("scalar-move.S");
    let obj = directory.join("scalar-move.o");
    let source = directory.join("scalar-move-driver.rs");
    let binary = directory.join("scalar-move-driver");
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
    println!("Decoded {forms} scalar move fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.vectors,b.vectors);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==11{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==10{15}else{7}]=addr;}}
fn vector(pattern:usize)->[u8;32]{let mut out=[0;32];let mut random=0xaab129387465bcdeu64^pattern as u64;let specials=[0,1,0x8000000000000000,0x000fffffffffffff,0x0010000000000000,0x7ff0000000000000,0xfff0000000000000,0x7ff0000000000001,0x7ff8deadbeef1234,0xfff876543210abcd,u64::MAX,0x7fc12345,0x7f800001,0xff800000,0x80000000,0x007fffff];for lane in 0..4{random^=random<<13;random^=random>>7;random^=random<<17;let value=if pattern<specials.len(){specials[(pattern+lane)%specials.len()]}else{random};out[lane*8..lane*8+8].copy_from_slice(&value.to_le_bytes());}out}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,width,store,dst,merge,source,mode,len)in cases{let fault=if store==0{1}else{2};
 if mode<9{for pattern in 0..128{let mut c=seed();c.mxcsr=[0,0xffff,0x10000][pattern%3];c.vectors[merge]=vector(pattern);c.vectors[source]=vector(pattern+3);let before=c.clone();let mut expected=before.vectors;expected[dst]=[0;32];expected[dst][..width].copy_from_slice(&before.vectors[source][..width]);expected[dst][width..16].copy_from_slice(&before.vectors[merge][width..16]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected);assert_eq!(c.fuel,19);preserved(&c,&before);checks+=1;}}
 else{for offset in [0usize,1,3,7,15]{for pattern in 0..128{for tail in [0usize,7]{let mut data=vec![0xa5;offset+width+tail];data[offset..offset+width].copy_from_slice(&vector(pattern)[..width]);let mut expected=data.clone();let mut memory=Memory::default();memory.add(0x1_00300001,&mut data,store==1,false).unwrap();let mut c=seed();c.mxcsr=[0,0xffff,0x10000][pattern%3];c.vectors[dst]=vector(pattern+17);c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001+offset as u64);let before=c.clone();let mut vectors=before.vectors;if store==0{vectors[dst]=[0;32];vectors[dst][..width].copy_from_slice(&expected[offset..offset+width]);}else{expected[offset..offset+width].copy_from_slice(&before.vectors[dst][..width]);}unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.fuel,19);assert_eq!(c.vectors,vectors);preserved(&c,&before);drop(memory);assert_eq!(data,expected);checks+=1;}}}
 for length in 0..width{let mut data=vec![0xa5;length];let original=data.clone();let mut memory=Memory::default();if length>0{memory.add(0x1_00300001,&mut data,true,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001);let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(fault,19,0x100));unchanged(&c,&before);drop(memory);assert_eq!(data,original);checks+=1;}
 if store==1{let mut data=vec![0xa5;width];let original=data.clone();let mut memory=Memory::default();memory.add(0x1_00300001,&mut data,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001);let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(2,19,0x100));unchanged(&c,&before);drop(memory);assert_eq!(data,original);checks+=1;}
 }
 let mut c=seed();c.fuel=0;let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&before);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,before.stop,20,0x100));unchanged(&c,&before);checks+=1;}
}
println!("PASS: {checks} native VMOVSS/VMOVSD checks: 8 exact VEX codes and both normalized register opcode directions, merge-source preservation and source aliases/highregs/LIG/WIG, independent bytecopy NaN/zero/subnormal/infinity/random values, memory load zeroing vs register merge, exact4/8 readonly loads/writable stores with sentinels, high/unaligned/RIP/everyshortspan/readonly store atomicity, flags/FP/fuel/preexisting faults");}
"#;
