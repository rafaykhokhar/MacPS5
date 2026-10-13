//! Independent full-256-bit source-selection oracle for VPERMD and VPERMQ.
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
    for op in 0..2 {
        for wide in 1..2 {
            for mode in 0..12 {
                let (dst, first, second) = match mode {
                    0 => (0, 1, 2),
                    1 | 11 => (15, 8, 9),
                    2 => (1, 1, 2),
                    3 => (2, 1, 2),
                    4 => (15, 15, 15),
                    5 => (0, 1, 1),
                    6 => (8, 15, 2),
                    7 => (2, 1, 15),
                    8 => (2, 1, 7),
                    9 => (15, 15, 15),
                    10 => (8, 2, 5),
                    _ => unreachable!(),
                };
                let memory = (8..=10).contains(&mode);
                let size = 32;
                let map = if op == 0 { 3 } else { 2 };
                let immediates: Vec<u8> = if op == 0 {
                    (0..=255).collect()
                } else {
                    vec![0]
                };
                for immediate in immediates {
                    let opcode = if op == 0 { 0x00 } else { 0x36 };
                    let mut bytes = vec![
                        0xc4,
                        map | 0x40
                            | if dst < 8 { 0x80 } else { 0 }
                            | if second < 8 { 0x20 } else { 0 },
                        (if op == 0 { 0x78 } else { (!first & 15) << 3 })
                            | 1
                            | wide * 4
                            | if op == 0 { 0x80 } else { 0 },
                        opcode,
                        if memory { 0 } else { 0xc0 } | ((dst & 7) << 3) | (second & 7),
                    ];
                    if mode == 10 {
                        bytes.extend_from_slice(&32u32.to_le_bytes());
                    }
                    if op == 0 {
                        bytes.push(immediate);
                    }
                    let d = native_ps5::decode::instruction(&bytes, 0x100)?;

                    let reg = "YMM";
                    let code = if op == 0 {
                        "VEX_Vpermq_ymm_ymmm256_imm8"
                    } else {
                        "VEX_Vpermd_ymm_ymm_ymmm256"
                    };
                    let source_op = if op == 0 { 1 } else { 2 };
                    if format!("{:?}", d.code()) != code
                        || d.len() != bytes.len()
                        || d.op_count() != 3
                        || format!("{:?}", d.op0_kind()) != "Register"
                        || (op == 1 && format!("{:?}", d.op1_kind()) != "Register")
                        || format!("{:?}", d.op0_register()) != format!("{reg}{dst}")
                        || (op == 1 && format!("{:?}", d.op1_register()) != format!("{reg}{first}"))
                        || (op == 0
                            && (format!("{:?}", d.op2_kind()) != "Immediate8"
                                || d.immediate8() != immediate))
                        || format!("{:?}", d.op_kind(source_op))
                            != if memory { "Memory" } else { "Register" }
                        || (!memory
                            && format!("{:?}", d.op_register(source_op))
                                != format!("{reg}{second}"))
                        || (memory
                            && (d.memory_size().size() != size
                                || format!("{:?}", d.memory_base())
                                    != match mode {
                                        8 => "RDI",
                                        9 => "R15",
                                        10 => "RIP",
                                        _ => unreachable!(),
                                    }
                                || format!("{:?}", d.memory_index()) != "None"))
                        || (mode == 10
                            && d.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
                    {
                        return Err(format!("Bad permute fixture {op}/{wide}/{mode}: {d:?}"));
                    }
                    if mode == 0 && immediate == 0 {
                        for bit in [4, 128] {
                            let mut invalid = bytes.clone();
                            invalid[2] ^= bit;
                            if native_ps5::aot::translate_rust_memory_region(
                                &invalid,
                                0x100,
                                "bad_permute_size",
                            )
                            .is_ok()
                            {
                                return Err(format!(
                                    "Unsupported W/L permutation accepted: {invalid:02x?}"
                                ));
                            }
                        }
                        for bad_prefix in [0x66, 0xf2, 0xf3, 0xf0] {
                            let mut invalid = vec![bad_prefix];
                            invalid.extend_from_slice(&bytes);
                            if native_ps5::aot::translate_rust_memory_region(
                                &invalid,
                                0x100,
                                "bad_permute_prefix",
                            )
                            .is_ok()
                            {
                                return Err(format!("Forbidden prefix accepted: {invalid:02x?}"));
                            }
                        }
                        if op == 0 {
                            let mut reserved = bytes.clone();
                            reserved[2] &= !8;
                            if native_ps5::aot::translate_rust_memory_region(
                                &reserved,
                                0x100,
                                "reserved_permute_vvvv",
                            )
                            .is_ok()
                            {
                                return Err("Reserved VPERMQ vvvv accepted".into());
                            }
                        }
                        let mut legacy = if op == 0 {
                            vec![0x66, 0x0f, 0x3a]
                        } else {
                            vec![0x66, 0x0f, 0x38]
                        };
                        legacy.extend_from_slice(&[opcode, 0xc1]);
                        if op == 0 {
                            legacy.push(0);
                        }
                        let mut evex = vec![
                            0x62,
                            0xf0 | map,
                            if op == 0 { 0xfd } else { 0x75 },
                            0x28,
                            opcode,
                            0xc2,
                        ];
                        if op == 0 {
                            evex.push(0);
                        }
                        for invalid in [&legacy[..], &evex[..]] {
                            if native_ps5::aot::translate_rust_memory_region(
                                invalid,
                                0x100,
                                "unsupported_packed_permute",
                            )
                            .is_ok()
                            {
                                return Err(format!(
                                    "Unsupported permute form accepted: {invalid:02x?}"
                                ));
                            }
                        }
                    }
                    if memory {
                        for prefix in [0x64, 0x65, 0x67] {
                            let mut invalid = vec![prefix];
                            invalid.extend_from_slice(&bytes);
                            if native_ps5::aot::translate_rust_memory_region(
                                &invalid,
                                0x100,
                                "guard_permute",
                            )
                            .is_ok()
                            {
                                return Err(format!("Unsupported permute prefix {prefix:x}"));
                            }
                        }
                    }
                    let name = format!("packed_permute_{op}_{wide}_{mode}_{immediate}");
                    assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                        &bytes, 0x100, &name,
                    )?);
                    declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                    table.push_str(&format!(
                        "({name},{op},{immediate},{size},{dst},{first},{second},{mode},{}),\n",
                        bytes.len()
                    ));
                    forms += 1;
                }
            }
        }
    }
    let asm = directory.join("packed-permute.S");
    let obj = directory.join("packed-permute.o");
    let source = directory.join("packed-permute-driver.rs");
    let binary = directory.join("packed-permute-driver");
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
    println!("Decoded {forms} packed permute fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.vectors,b.vectors);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==10{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==9{15}else{7}]=addr;}}
fn expected(b:&Cpu,op:usize,immediate:u8,size:usize,dst:usize,first:usize,second:&[u8])->[[u8;32];16]{assert_eq!(size,32);let mut result=b.vectors;let mut output=[0;32];let width=if op==0{8}else{4};for lane in 0..32/width{let index=if op==0{usize::from((immediate>>(2*lane))%4)}else{let at=lane*4;(u32::from_le_bytes(b.vectors[first][at..at+4].try_into().unwrap())%8)as usize};for byte in 0..width{output[lane*width+byte]=second[index*width+byte];}}result[dst]=output;result}
fn vectors(c:&mut Cpu,first:usize,second:usize,pattern:usize){let mut indices=[0;32];let mut source=[0;32];let mut random=0x871623456789abcd_u64 ^ pattern as u64;for lane in 0..8{random^=random<<13;random^=random>>7;random^=random<<17;let index=match pattern{0..=31=>(pattern%8)as u32|match pattern/8{0=>0,1=>0xfffffff8,2=>0xaaaaaaa8,_=>0x55555550},32..=60=>(lane as u32)|(1u32<<(pattern-29)),61=>(7-lane)as u32,62=>((lane+3)%8)as u32,63=>((lane*3)%8)as u32,64|65=>3,_=>random as u32};indices[lane*4..lane*4+4].copy_from_slice(&index.to_le_bytes());for byte in 0..4{source[lane*4+byte]=if (pattern==64||pattern==65)&&lane!=3{if pattern==64{0}else{255}}else{(lane*29+byte*7+3)as u8};}}c.vectors[first]=indices;c.vectors[second]=source;}
fn main(){let cases:&[(Block,usize,u8,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,op,immediate,size,dst,first,second,mode,len) in cases{
 if !(8..=10).contains(&mode){for pattern in 0..if op==0{4}else{192}{let mut c=seed();if op==0{for i in 0..32{c.vectors[second][i]=(i*7+pattern*43)as u8;}}else{vectors(&mut c,first,second,pattern);}let b=c.clone();let e=expected(&b,op,immediate,size,dst,first,&b.vectors[second]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e,"op {op},size {size},mode {mode},pattern {pattern}");assert_eq!(c.fuel,19);preserved(&c,&b);checks+=1;}}
 else{for offset in [0usize,1,3,7,15]{for pattern in 0..if op==0{4}else{192}{let mut c=seed();let temp=if first==0{1}else{0};if op==0{for i in 0..32{c.vectors[temp][i]=(i*7+pattern*43)as u8;}}else{vectors(&mut c,first,temp,pattern);}let mut bytes=vec![0xa5;offset+size];bytes[offset..].copy_from_slice(&c.vectors[temp][..size]);let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();memory.add(addr,&mut bytes,false,false).unwrap();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr+offset as u64);let b=c.clone();let e=expected(&b,op,immediate,size,dst,first,&before[offset..]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e);assert_eq!(c.fuel,19);preserved(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}}
  for length in 0..size{let mut bytes=vec![0xaa;length];let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();if length>0{memory.add(addr,&mut bytes,false,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(1,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
 }
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VPERMD/VPERMQ checks: 2 exact VEX forms, full256 source-selection oracle, every quadword immediate and dword selector, ignored index highbits, mixed/reverse/rotate/broadcast/random indices and unused source poison, aliases/highregs, full readonly high/unaligned/RIP loads, every shortspan, flags/FP/fuel/preexisting faults");}
"#;
