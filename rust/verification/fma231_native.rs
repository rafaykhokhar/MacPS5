//! Independent exact-integer and pinned fused-result oracle for VFMADD231SD.
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
    for op in 0..1 {
        for wide in 0..2 {
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
                let size = 8;
                let map = 2;
                let opcode = 0xb9;
                let mut bytes = vec![
                    0xc4,
                    map | 0x40 | if dst < 8 { 0x80 } else { 0 } | if second < 8 { 0x20 } else { 0 },
                    ((!first & 15) << 3) | 0x81 | wide * 4,
                    opcode,
                    if memory { 0 } else { 0xc0 } | ((dst & 7) << 3) | (second & 7),
                ];
                if mode == 10 {
                    bytes.extend_from_slice(&32u32.to_le_bytes());
                }
                let d = native_ps5::decode::instruction(&bytes, 0x100)?;

                let reg = "XMM";
                let code = "VEX_Vfmadd231sd_xmm_xmm_xmmm64";
                if format!("{:?}", d.code()) != code
                    || d.len() != bytes.len()
                    || d.op_count() != 3
                    || format!("{:?}", d.op0_kind()) != "Register"
                    || format!("{:?}", d.op1_kind()) != "Register"
                    || format!("{:?}", d.op0_register()) != format!("{reg}{dst}")
                    || format!("{:?}", d.op1_register()) != format!("{reg}{first}")
                    || format!("{:?}", d.op2_kind()) != if memory { "Memory" } else { "Register" }
                    || (!memory && format!("{:?}", d.op2_register()) != format!("{reg}{second}"))
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
                    || (mode == 10 && d.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
                {
                    return Err(format!("Bad FMA231 fixture {op}/{wide}/{mode}: {d:?}"));
                }
                if mode == 0 {
                    for (at, value) in [(2, bytes[2] & 0x7f), (3, 0x99)] {
                        let mut invalid = bytes.clone();
                        invalid[at] = value;
                        if native_ps5::aot::translate_rust_memory_region(
                            &invalid,
                            0x100,
                            "unsupported_fma_form",
                        )
                        .is_ok()
                        {
                            return Err(format!("Unsupported FMA form accepted: {invalid:02x?}"));
                        }
                    }
                    for bad_prefix in [0x66, 0xf2, 0xf3, 0xf0] {
                        let mut invalid = vec![bad_prefix];
                        invalid.extend_from_slice(&bytes);
                        if native_ps5::aot::translate_rust_memory_region(
                            &invalid,
                            0x100,
                            "bad_FMA231_prefix",
                        )
                        .is_ok()
                        {
                            return Err(format!("Forbidden prefix accepted: {invalid:02x?}"));
                        }
                    }
                    let mut legacy = vec![0x66, 0x0f, 0x38];
                    legacy.extend_from_slice(&[opcode, 0xc1]);
                    let evex = [0x62, 0xf0 | map, 0xf5, 0x08, opcode, 0xc2];
                    for invalid in [&legacy[..], &evex[..]] {
                        if native_ps5::aot::translate_rust_memory_region(
                            invalid,
                            0x100,
                            "unsupported_fma231",
                        )
                        .is_ok()
                        {
                            return Err(format!(
                                "Unsupported FMA231 form accepted: {invalid:02x?}"
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
                            "guard_FMA231",
                        )
                        .is_ok()
                        {
                            return Err(format!("Unsupported FMA231 prefix {prefix:x}"));
                        }
                    }
                }
                let name = format!("fma231_{op}_{wide}_{mode}");
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                table.push_str(&format!(
                    "({name},{dst},{first},{second},{mode},{}),\n",
                    bytes.len()
                ));
                forms += 1;
            }
        }
    }
    let asm = directory.join("fma231.S");
    let obj = directory.join("fma231.o");
    let source = directory.join("fma231-driver.rs");
    let binary = directory.join("fma231-driver");
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
    println!("Decoded {forms} scalar FMA231 fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x1f80,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.x87_control,c.x87_status),(b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.vectors,b.vectors);assert_eq!(c.mxcsr,b.mxcsr);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==10{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==9{15}else{7}]=addr;}}
fn bits(c:&Cpu,reg:usize)->u64{u64::from_le_bytes(c.vectors[reg][..8].try_into().unwrap())}
fn check(c:&Cpu,b:&Cpu,dst:usize,result:u64,status:u32){let fault=if b.mxcsr&!0xffff!=0{14}else if status&!(b.mxcsr>>7)!=0{15}else{0};assert_eq!(c.fault,fault);assert_eq!(c.mxcsr,if fault==14{b.mxcsr}else{b.mxcsr|status});let mut expected=b.vectors;if fault==0{expected[dst][..8].copy_from_slice(&result.to_le_bytes());expected[dst][16..].fill(0);}else{assert_eq!(c.rip,b.rip);}assert_eq!(c.vectors,expected);assert_eq!(c.fuel,19);preserved(c,b);}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,dst,first,second,mode,len)in cases{let mem=(8..=10).contains(&mode);
 for pattern in 0..128i64{for rc in 0..4{let mut c=seed();c.mxcsr|=rc<<13;let(a,b,z)=(pattern-65,pattern*3-7,39-pattern*2);for(reg,value)in [(dst,a),(first,b)]{c.vectors[reg][..8].copy_from_slice(&(value as f64).to_bits().to_le_bytes());}if !mem{c.vectors[second][..8].copy_from_slice(&(z as f64).to_bits().to_le_bytes());}
 let mut data=(z as f64).to_bits().to_le_bytes();let mut memory=Memory::default();if mem{memory.add(0x1_00300001,&mut data,false,false).unwrap();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001);}
 let before=c.clone();let old=f64::from_bits(bits(&before,dst))as i64;let left=f64::from_bits(bits(&before,first))as i64;let right=if mem{z}else{f64::from_bits(bits(&before,second))as i64};let integer=left*right+old;let result=if integer==0&&rc==1{1<<63}else{(integer as f64).to_bits()};unsafe{block(&mut c)};check(&c,&before,dst,result,0);drop(memory);assert_eq!(data,(z as f64).to_bits().to_le_bytes());checks+=1;}}
 if dst!=first && (mem||dst!=second&&first!=second){for &(old,left,right,control,result,status) in SPECIAL {for sticky in [0,4,0x3f]{let mut c=seed();c.mxcsr=control|sticky;c.vectors[dst][..8].copy_from_slice(&old.to_le_bytes());c.vectors[first][..8].copy_from_slice(&left.to_le_bytes());if !mem{c.vectors[second][..8].copy_from_slice(&right.to_le_bytes());}let mut data=right.to_le_bytes();let mut memory=Memory::default();if mem{memory.add(0x1_00300001,&mut data,false,false).unwrap();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001);}let before=c.clone();unsafe{block(&mut c)};check(&c,&before,dst,result,status);drop(memory);assert_eq!(data,right.to_le_bytes());checks+=1;}}}
 if mem{for offset in [0usize,1,3,7,15]{let mut data=vec![0xa5;offset+8];data[offset..].copy_from_slice(&3f64.to_bits().to_le_bytes());let original=data.clone();let mut memory=Memory::default();memory.add(0x1_00300001,&mut data,false,false).unwrap();let mut c=seed();c.vectors[dst][..8].copy_from_slice(&2f64.to_bits().to_le_bytes());c.vectors[first][..8].copy_from_slice(&5f64.to_bits().to_le_bytes());c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001+offset as u64);let before=c.clone();let expected=if dst==first{20f64}else{17f64};unsafe{block(&mut c)};check(&c,&before,dst,expected.to_bits(),0);drop(memory);assert_eq!(data,original);checks+=1;}
 for length in 0..8{for control in [0u32,0x1f80,0x10000]{let mut data=vec![0xff;length];let original=data.clone();let mut memory=Memory::default();if length>0{memory.add(0x1_00300001,&mut data,false,false).unwrap();}let mut c=seed();c.mxcsr=control;c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001);let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(1,19,0x100));unchanged(&c,&before);drop(memory);assert_eq!(data,original);checks+=1;}}}
 let mut c=seed();c.fuel=0;let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&before);checks+=1;
 for fault in [1,2,13,14,15]{let mut c=seed();c.fault=fault;let before=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,before.stop,20,0x100));unchanged(&c,&before);checks+=1;}
}
println!("PASS: {checks} native VFMADD231SD checks: independent exact integer arithmetic and pinned fused/rounding/exception vectors, NaN priority, aliases/highregs/LIG, preserved destination high64 and cleared upper128, readonly exact8 high/unaligned/RIP and shortspan guards, flags/GPR/x87/fuel and exception atomicity");}
// (old destination/addend, explicit first multiplicand, final multiplicand,
// MXCSR, rounded result, newly raised status). Constants are independent of
// the implementation, including cancellation that separate MUL+ADD gets wrong.
const SPECIAL:&[(u64,u64,u64,u32,u64,u32)]=&[
 // Intel Vol1 14.5.1/Table14-17: explicit first, final, old destination.
 (0x7ff8111111111111,0x7ff8222222222222,0x7ff8333333333333,0x1f80,0x7ff8222222222222,0),
 (0x7ff8111111111111,0x7ff8333333333333,0x7ff8222222222222,0x1f80,0x7ff8333333333333,0),
 (0x7ff8222222222222,0x7ff8111111111111,0x7ff8333333333333,0x1f80,0x7ff8111111111111,0),
 (0x7ff8222222222222,0x7ff8333333333333,0x7ff8111111111111,0x1f80,0x7ff8333333333333,0),
 (0x7ff8333333333333,0x7ff8111111111111,0x7ff8222222222222,0x1f80,0x7ff8111111111111,0),
 (0x7ff8333333333333,0x7ff8222222222222,0x7ff8111111111111,0x1f80,0x7ff8222222222222,0),
 (0x7ff8111111111111,0x3ff0000000000000,0x7ff8333333333333,0x1f80,0x7ff8333333333333,0),
 (0x7ff8111111111111,0x3ff0000000000000,0x3ff0000000000000,0x1f80,0x7ff8111111111111,0),
 (0x3ff0000000000000,0x7ff8222222222222,0x3ff0000000000000,0x1f80,0x7ff8222222222222,0),
 (0x3ff0000000000000,0x3ff0000000000000,0x7ff8333333333333,0x1f80,0x7ff8333333333333,0),
 (0x7ff0111111111111,0x7ff8222222222222,0x7ff8333333333333,0x1f80,0x7ff8222222222222,1),
 (0x7ff8111111111111,0x7ff0222222222222,0x7ff8333333333333,0x1f80,0x7ff8222222222222,1),
 (0x7ff8111111111111,0x7ff8222222222222,0x7ff0333333333333,0x1f80,0x7ff8222222222222,1),
 (0x7ff8111111111111,0x7ff8222222222222,0x7ff0333333333333,0x1f00,0,1),
 (0x7ff8111111111111,0,0x7ff0000000000000,0x1f80,0x7ff8111111111111,0),
 (0xbff0000000000000,0x3ff0000000000001,0x3feffffffffffffe,0x1f80,0xb970000000000000,0),
 (0xbff0000000000000,0x3ff0000000000000,0x3ff0000000000000,0x3f80,0x8000000000000000,0),
 (0xbff0000000000000,0x3ff0000000000000,0x3ff0000000000000,0x1f80,0,0),
 (0x3ca0000000000000,0x3ff0000000000000,0x3ff0000000000000,0x1f80,0x3ff0000000000000,32),
 (0x3ca0000000000000,0x3ff0000000000000,0x3ff0000000000000,0x3f80,0x3ff0000000000000,32),
 (0x3ca0000000000000,0x3ff0000000000000,0x3ff0000000000000,0x5f80,0x3ff0000000000001,32),
 (0x3ca0000000000000,0x3ff0000000000000,0x3ff0000000000000,0x7f80,0x3ff0000000000000,32),
 (0x3ca0000000000000,0x3ff0000000000000,0x3ff0000000000000,0x0f80,0,32),
 (0x3ff0000000000000,0,0x7ff0000000000000,0x1f80,0xfff8000000000000,1),
 (0x3ff0000000000000,0,0x7ff0000000000000,0x1f00,0,1),
 (0xfff0000000000000,0x7ff0000000000000,0x3ff0000000000000,0x1f80,0xfff8000000000000,1),
 (0,1,0x3ff0000000000000,0x1f80,1,2),
 (0,1,0x3ff0000000000000,0x1fc0,0,0),
 (0,1,0x3ff0000000000000,0x1e80,0,2),
 (0,0x0010000000000000,0x3fe0000000000000,0x1f80,0x0008000000000000,0),
 (0,0x0010000000000000,0x3fe0000000000000,0x9f80,0,48),
 (0,0x0010000000000000,0x3fe0000000000000,0x8f80,0,48),
 (0,0x0010000000000000,0x3c90000000000000,0x1f80,0,48),
 (0,0x0010000000000000,0x3c90000000000000,0x5f80,1,48),
 (0,0x7fefffffffffffff,0x4000000000000000,0x1f80,0x7ff0000000000000,40),
 (0,0x7fefffffffffffff,0x4000000000000000,0x7f80,0x7fefffffffffffff,40),
 // Intel Vol1 Table D-18: an unmasked overflow suppresses new precision status.
 (0,0x7fefffffffffffff,0x4000000000000000,0x1b80,0,8),
 (0,0x3ff0000000000000,0x3ff0000000000000,0x11f80,0,0),
];
"#;
