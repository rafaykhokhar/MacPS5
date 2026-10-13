//! Independent byte-copy and alignment oracle for VEX VMOVAPD and VMOVUPD.
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
    for aligned in 0..2 {
        for wide in 0..2 {
            for store in 0..2 {
                for mode in 0..9 {
                    let (reg, rm) = match mode {
                        0 => (0, 1),
                        1 => (15, 8),
                        2 => (2, 15),
                        3 => (15, 15),
                        4 => (2, 2),
                        5 => (8, 9),
                        6 => (2, 7),
                        7 => (15, 15),
                        8 => (8, 5),
                        _ => unreachable!(),
                    };
                    let memory = mode >= 6;
                    let size = if wide == 0 { 16 } else { 32 };
                    let opcode = if aligned == 1 { 0x28 } else { 0x10 } + store;
                    let mut bytes = vec![
                        0xc4,
                        0x41 | if reg < 8 { 0x80 } else { 0 } | if rm < 8 { 0x20 } else { 0 },
                        0x79 | wide * 4 | if mode == 5 { 128 } else { 0 },
                        opcode,
                        if memory { 0 } else { 0xc0 } | ((reg & 7) << 3) | (rm & 7),
                    ];
                    if mode == 8 {
                        bytes.extend_from_slice(&32u32.to_le_bytes());
                    }
                    let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                    let v = if wide == 0 { "xmm" } else { "ymm" };
                    let regname = if wide == 0 { "XMM" } else { "YMM" };
                    let mnemonic = if aligned == 1 { "apd" } else { "upd" };
                    let code = if store == 0 {
                        format!("VEX_Vmov{mnemonic}_{v}_{v}m{}", size * 8)
                    } else {
                        format!("VEX_Vmov{mnemonic}_{v}m{}_{v}", size * 8)
                    };
                    let register_op = if store == 0 { 0 } else { 1 };
                    let rm_op = if store == 0 { 1 } else { 0 };
                    if format!("{:?}", d.code()) != code
                        || d.len() != bytes.len()
                        || d.op_count() != 2
                        || format!("{:?}", d.op_kind(register_op)) != "Register"
                        || format!("{:?}", d.op_register(register_op)) != format!("{regname}{reg}")
                        || format!("{:?}", d.op_kind(rm_op))
                            != if memory { "Memory" } else { "Register" }
                        || (!memory
                            && format!("{:?}", d.op_register(rm_op)) != format!("{regname}{rm}"))
                        || (memory
                            && (d.memory_size().size() != size
                                || format!("{:?}", d.memory_base())
                                    != match mode {
                                        6 => "RDI",
                                        7 => "R15",
                                        8 => "RIP",
                                        _ => unreachable!(),
                                    }
                                || format!("{:?}", d.memory_index()) != "None"))
                        || (mode == 8
                            && d.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
                    {
                        return Err(format!(
                            "Bad move fixture {aligned}/{wide}/{store}/{mode}: {d:?}"
                        ));
                    }
                    let mut reserved = bytes.clone();
                    reserved[2] &= !8;
                    let mut invalids = vec![
                        reserved,
                        vec![0x66, 0x0f, opcode, 0xc1],
                        vec![0x62, 0xf1, 0xfd, 0x08, opcode, 0xc1],
                    ];
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
                            "unsupported_double_move",
                        )
                        .is_ok()
                        {
                            return Err(format!("Unsupported move accepted: {invalid:02x?}"));
                        }
                    }
                    let name = format!("double_move_{aligned}_{wide}_{store}_{mode}");
                    assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                        &bytes, 0x100, &name,
                    )?);
                    declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                    table.push_str(&format!(
                        "({name},{aligned},{size},{store},{reg},{rm},{mode},{}),\n",
                        bytes.len()
                    ));
                    forms += 1;
                }
            }
        }
    }
    let asm = directory.join("packed-double-move.S");
    let obj = directory.join("packed-double-move.o");
    let source = directory.join("packed-double-move-driver.rs");
    let binary = directory.join("packed-double-move-driver");
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
    println!("Decoded {forms} packed-double move fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.vectors,b.vectors);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==8{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==7{15}else{7}]=addr;}}
fn vector(pattern:usize)->[u8;32]{let mut out=[0;32];let mut random=0xaab129387465bcdeu64^pattern as u64;let specials=[0,1,0x8000000000000000,0x000fffffffffffff,0x0010000000000000,0x7ff0000000000000,0xfff0000000000000,0x7ff0000000000001,0x7ff8deadbeef1234,0xfff876543210abcd,u64::MAX];for lane in 0..4{random^=random<<13;random^=random>>7;random^=random<<17;let value=if pattern<specials.len(){specials[(pattern+lane)%specials.len()]}else{random};out[lane*8..lane*8+8].copy_from_slice(&value.to_le_bytes());}out}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,aligned,size,store,reg,rm,mode,len)in cases{let fault=if store==0{1}else{2};
 if mode<6{let(dst,src)=if store==0{(reg,rm)}else{(rm,reg)};for pattern in 0..128{let mut c=seed();c.vectors[src]=vector(pattern);let before=c.clone();let mut expected=before.vectors;expected[dst]=[0;32];expected[dst][..size].copy_from_slice(&before.vectors[src][..size]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected);assert_eq!(c.fuel,19);preserved(&c,&before);checks+=1;}}
 else{let offsets=if aligned==1{vec![0,size,64,128]}else{vec![0,1,3,7,15,31]};for offset in offsets{for pattern in 0..128{for tail in [0usize,7]{let mut data=vec![0xa5;offset+size+tail];data[offset..offset+size].copy_from_slice(&vector(pattern)[..size]);let mut expected=data.clone();let mut memory=Memory::default();memory.add(0x1_00300000,&mut data,store==1,false).unwrap();let mut c=seed();c.vectors[reg]=vector(pattern+17);c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300000+offset as u64);let before=c.clone();let mut vectors=before.vectors;if store==0{vectors[reg]=[0;32];vectors[reg][..size].copy_from_slice(&expected[offset..offset+size]);}else{expected[offset..offset+size].copy_from_slice(&before.vectors[reg][..size]);}unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.fuel,19);assert_eq!(c.vectors,vectors);preserved(&c,&before);drop(memory);assert_eq!(data,expected);checks+=1;}}}
 for length in 0..size{let mut data=vec![0xa5;length];let original=data.clone();let mut memory=Memory::default();if length>0{memory.add(0x1_00300000,&mut data,true,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300000);let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(fault,19,0x100));unchanged(&c,&before);drop(memory);assert_eq!(data,original);checks+=1;}
 if aligned==1{for offset in 1..size{let mut data=vec![0xa5;size+offset];let original=data.clone();let mut memory=Memory::default();memory.add(0x1_00300000,&mut data,true,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300000+offset as u64);let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(fault,19,0x100));unchanged(&c,&before);drop(memory);assert_eq!(data,original);checks+=1;}}
 if store==1{let mut data=vec![0xa5;size];let original=data.clone();let mut memory=Memory::default();memory.add(0x1_00300000,&mut data,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300000);let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(2,19,0x100));unchanged(&c,&before);drop(memory);assert_eq!(data,original);checks+=1;}
 }
 let mut c=seed();c.fuel=0;let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&before);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,before.stop,20,0x100));unchanged(&c,&before);checks+=1;}
}
println!("PASS: {checks} native VMOVAPD/VMOVUPD checks: 8 exact VEX load/store forms, both register opcode directions, aliases/highregs/WIG, independent byte copies NaN/zero/subnormal/infinity/random lanes, upper clearing, APD16/32 alignment faults and UPD unaligned copies, full readonly loads/writable stores with sentinels, high/RIP memory/everyshortspan/readonly store atomicity, flags/FP/fuel/preexisting faults");}
"#;
