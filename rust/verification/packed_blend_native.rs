//! Independent lane-selection oracle for VEX VPBLENDW and VPBLENDD.
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
                let size = if wide == 0 { 16 } else { 32 };
                let map = 3;
                let immediates: Vec<u8> = if mode == 0 || mode == 8 {
                    (0..=255).collect()
                } else {
                    vec![0, 0x55, 0xaa, 255]
                };
                for immediate in immediates {
                    let opcode = if op == 0 { 0x0e } else { 0x02 };
                    let mut bytes = vec![
                        0xc4,
                        map | 0x40
                            | if dst < 8 { 0x80 } else { 0 }
                            | if second < 8 { 0x20 } else { 0 },
                        ((!first & 15) << 3)
                            | 1
                            | wide * 4
                            | if op == 0 && mode == 11 { 0x80 } else { 0 },
                        opcode,
                        if memory { 0 } else { 0xc0 } | ((dst & 7) << 3) | (second & 7),
                    ];
                    if mode == 10 {
                        bytes.extend_from_slice(&32u32.to_le_bytes());
                    }
                    bytes.push(immediate);
                    let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                    let v = if wide == 0 { "xmm" } else { "ymm" };
                    let reg = if wide == 0 { "XMM" } else { "YMM" };
                    let code = format!(
                        "VEX_Vpblend{}_{v}_{v}_{v}m{}_imm8",
                        if op == 0 { "w" } else { "d" },
                        size * 8
                    );
                    let source_op = 2;
                    if format!("{:?}", d.code()) != code
                        || d.len() != bytes.len()
                        || d.op_count() != 4
                        || format!("{:?}", d.op0_kind()) != "Register"
                        || (format!("{:?}", d.op1_kind()) != "Register")
                        || format!("{:?}", d.op0_register()) != format!("{reg}{dst}")
                        || (format!("{:?}", d.op1_register()) != format!("{reg}{first}"))
                        || (format!("{:?}", d.op3_kind()) != "Immediate8"
                            || d.immediate8() != immediate)
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
                        return Err(format!("Bad blend fixture {op}/{wide}/{mode}: {d:?}"));
                    }
                    if mode == 0 {
                        for bad_prefix in [0x66, 0xf2, 0xf3, 0xf0] {
                            let mut invalid = vec![bad_prefix];
                            invalid.extend_from_slice(&bytes);
                            if native_ps5::aot::translate_rust_memory_region(
                                &invalid,
                                0x100,
                                "bad_blend_prefix",
                            )
                            .is_ok()
                            {
                                return Err(format!("Forbidden prefix accepted: {invalid:02x?}"));
                            }
                        }
                        if op == 1 {
                            let mut bad_w = bytes.clone();
                            bad_w[2] |= 128;
                            if native_ps5::aot::translate_rust_memory_region(
                                &bad_w,
                                0x100,
                                "bad_blend_w",
                            )
                            .is_ok()
                            {
                                return Err("VPBLENDD W1 unexpectedly accepted".into());
                            }
                        }
                        let mut legacy = vec![0x66, 0x0f, 0x3a];
                        legacy.extend_from_slice(&[opcode, 0xc1]);
                        legacy.push(0);
                        let mut evex = vec![0x62, 0xf0 | map, 0x75, 0x08, opcode, 0xc2];
                        evex.push(0);
                        for invalid in [&legacy[..], &evex[..]] {
                            if native_ps5::aot::translate_rust_memory_region(
                                invalid,
                                0x100,
                                "unsupported_packed_blend",
                            )
                            .is_ok()
                            {
                                return Err(format!(
                                    "Unsupported blend form accepted: {invalid:02x?}"
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
                                "guard_blend",
                            )
                            .is_ok()
                            {
                                return Err(format!("Unsupported blend prefix {prefix:x}"));
                            }
                        }
                    }
                    let name = format!("packed_blend_{op}_{wide}_{mode}_{immediate}");
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
    let asm = directory.join("packed-blend.S");
    let obj = directory.join("packed-blend.o");
    let source = directory.join("packed-blend-driver.rs");
    let binary = directory.join("packed-blend-driver");
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
    println!("Decoded {forms} packed blend fixtures");
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
fn expected(b:&Cpu,op:usize,immediate:u8,size:usize,dst:usize,first:usize,second:&[u8])->[[u8;32];16]{let mut result=b.vectors;let mut output=[0;32];let width=if op==0{2}else{4};for i in 0..size{let select_bit=if op==0{(i/width)%8}else{i/width};output[i]=if (immediate>>select_bit)%2!=0{second[i]}else{b.vectors[first][i]};}result[dst]=output;result}
fn vectors(c:&mut Cpu,first:usize,second:usize,pattern:usize){let mut lhs=[0;32];let mut rhs=[0;32];let mut random=0x871623456789abcd_u64 ^ pattern as u64;for i in 0..32{random^=random<<13;random^=random>>7;random^=random<<17;let(a,b)=match pattern{0=>(0,255),1=>(255,0),2=>((i*7+3)as u8,(i*11+131)as u8),3=>if i<16{(0xaa,0x55)}else{(0x33,0xcc)},_=>(random as u8,(random>>39)as u8)};lhs[i]=a;rhs[i]=b;}c.vectors[first]=lhs;c.vectors[second]=rhs;}
fn main(){let cases:&[(Block,usize,u8,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,op,immediate,size,dst,first,second,mode,len) in cases{
 if !(8..=10).contains(&mode){for pattern in 0..12{let mut c=seed();vectors(&mut c,first,second,pattern);let b=c.clone();let e=expected(&b,op,immediate,size,dst,first,&b.vectors[second]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e,"op {op},size {size},mode {mode},pattern {pattern}");assert_eq!(c.fuel,19);preserved(&c,&b);checks+=1;}}
 else{for offset in [0usize,1,3,7,15]{for pattern in 0..12{let mut c=seed();let temp=if first==0{1}else{0};vectors(&mut c,first,temp,pattern);let mut bytes=vec![0xa5;offset+size];bytes[offset..].copy_from_slice(&c.vectors[temp][..size]);let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();memory.add(addr,&mut bytes,false,false).unwrap();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr+offset as u64);let b=c.clone();let e=expected(&b,op,immediate,size,dst,first,&before[offset..]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e);assert_eq!(c.fuel,19);preserved(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}}
  for length in 0..size{let mut bytes=vec![0xaa;length];let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();if length>0{memory.add(addr,&mut bytes,false,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(1,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
 }
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VPBLENDW/VPBLENDD checks: 4 exact VEX forms, independent lane selection, every immediate, word mask repeats across128 halves and dword upper mask bits, distinct/random source bytes, aliases/highregs/wordWIG, XMM upperzero, full readonly high/unaligned/RIP loads even zero mask, every shortspan, flags/FP/fuel/preexisting faults");}
"#;
