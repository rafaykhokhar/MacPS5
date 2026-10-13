//! Independent byte-copy/sign-fill oracle for VEX packed signed/zero extension.
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
    for signed in [false, true] {
        for (pair, (input, output, suffix)) in [
            (1usize, 2usize, "bw"),
            (1, 4, "bd"),
            (1, 8, "bq"),
            (2, 4, "wd"),
            (2, 8, "wq"),
            (4, 8, "dq"),
        ]
        .into_iter()
        .enumerate()
        {
            for wide in 0..2 {
                for mode in 0..8 {
                    let (dst, src) = match mode {
                        0 => (2, 1),
                        1 | 7 => (15, 8),
                        2 => (8, 15),
                        3 => (15, 15),
                        4 => (2, 7),
                        5 => (15, 15),
                        6 => (8, 5),
                        _ => unreachable!(),
                    };
                    let memory = (4..=6).contains(&mode);
                    let size = if wide == 0 { 16 } else { 32 };
                    let span = size / output * input;
                    let opcode = if signed { 0x20 } else { 0x30 } + pair as u8;
                    let mut bytes = vec![
                        0xc4,
                        0x42 | if dst < 8 { 0x80 } else { 0 } | if src < 8 { 0x20 } else { 0 },
                        0x79 | wide * 4 | if mode == 7 { 0x80 } else { 0 },
                        opcode,
                        if memory { 0 } else { 0xc0 } | ((dst & 7) << 3) | (src & 7),
                    ];
                    if mode == 6 {
                        bytes.extend_from_slice(&32u32.to_le_bytes());
                    }
                    let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                    let v = if wide == 0 { "xmm" } else { "ymm" };
                    let reg = if wide == 0 { "XMM" } else { "YMM" };
                    let code = format!(
                        "VEX_Vpmov{}x{suffix}_{v}_xmmm{}",
                        if signed { "s" } else { "z" },
                        span * 8
                    );
                    if format!("{:?}", d.code()) != code
                        || d.len() != bytes.len()
                        || d.op_count() != 2
                        || format!("{:?}", d.op0_kind()) != "Register"
                        || format!("{:?}", d.op0_register()) != format!("{reg}{dst}")
                        || format!("{:?}", d.op1_kind())
                            != if memory { "Memory" } else { "Register" }
                        || (!memory && format!("{:?}", d.op1_register()) != format!("XMM{src}"))
                        || (memory
                            && (d.memory_size().size() != span
                                || format!("{:?}", d.memory_base())
                                    != match mode {
                                        4 => "RDI",
                                        5 => "R15",
                                        6 => "RIP",
                                        _ => unreachable!(),
                                    }
                                || format!("{:?}", d.memory_index()) != "None"))
                        || (mode == 6
                            && d.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
                    {
                        return Err(format!(
                            "Bad packed extension {signed}/{pair}/{wide}/{mode}: {d:?}"
                        ));
                    }
                    let mut reserved = bytes.clone();
                    reserved[2] &= !8;
                    if native_ps5::aot::translate_rust_memory_region(
                        &reserved,
                        0x100,
                        "reserved_extension",
                    )
                    .is_ok()
                    {
                        return Err("Reserved packed extension vvvv accepted".into());
                    }
                    if memory {
                        for prefix in [0x64, 0x65, 0x67] {
                            let mut invalid = vec![prefix];
                            invalid.extend_from_slice(&bytes);
                            if native_ps5::aot::translate_rust_memory_region(
                                &invalid,
                                0x100,
                                "guard_extension",
                            )
                            .is_ok()
                            {
                                return Err(format!(
                                    "Unsupported packed extension prefix {prefix:x}"
                                ));
                            }
                        }
                    }
                    let name = format!("packed_extend_{}_{pair}_{wide}_{mode}", u8::from(signed));
                    assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                        &bytes, 0x100, &name,
                    )?);
                    declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                    table.push_str(&format!(
                        "({name},{signed},{input},{output},{size},{dst},{src},{mode},{}),\n",
                        bytes.len()
                    ));
                }
            }
        }
    }
    let asm = directory.join("packed-extend.S");
    let obj = directory.join("packed-extend.o");
    let source = directory.join("packed-extend-driver.rs");
    let binary = directory.join("packed-extend-driver");
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
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==6{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==5{15}else{7}]=addr;}}
fn expected(b:&Cpu,signed:bool,input:usize,output:usize,size:usize,dst:usize,source:&[u8])->[[u8;32];16]{let mut result=b.vectors;let mut data=[0;32];for lane in 0..size/output{let from=lane*input;let to=lane*output;let negative=source[from+input-1]&128!=0;data[to..to+output].fill(if signed&&negative{255}else{0});data[to..to+input].copy_from_slice(&source[from..from+input]);}result[dst]=data;result}
fn source(input:usize,span:usize,pattern:usize,poison:u8)->[u8;32]{let mut bytes=[poison;32];let mask=u64::MAX>>(64-input*8);let sign=1u64<<(input*8-1);let mut random=0x8fedcba123456789u64.wrapping_add(pattern as u64);for lane in 0..span/input{random^=random<<13;random^=random>>7;random^=random<<17;let value=if input==1{(pattern+lane*53)as u64&mask}else{match pattern{0=>0,1=>mask,2=>sign,3=>sign-1,4=>1,5=>if lane%2==0{sign}else{sign-1},6=>mask-lane as u64,7=>lane as u64,_=>random&mask}};bytes[lane*input..(lane+1)*input].copy_from_slice(&value.to_le_bytes()[..input]);}bytes}
fn main(){let cases:&[(Block,bool,usize,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,signed,input,output,size,dst,src,mode,len) in cases{let span=size/output*input;
 if !(4..=6).contains(&mode){for pattern in 0..if input==1{256}else{128}{for poison in [0,255]{let mut c=seed();c.vectors[src]=source(input,span,pattern,poison);let b=c.clone();let e=expected(&b,signed,input,output,size,dst,&b.vectors[src]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e,"signed {signed},input {input},output {output},size {size},mode {mode},pattern {pattern}");assert_eq!(c.fuel,19);preserved(&c,&b);checks+=1;}}}
 else{for offset in [0usize,1,3,7,15]{for pattern in 0..8{let mut c=seed();let values=source(input,span,if input==1{[0,1,127,128,129,254,255,83][pattern]}else{pattern},0xa5);let mut bytes=vec![0xa5;offset+span];bytes[offset..].copy_from_slice(&values[..span]);let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();memory.add(addr,&mut bytes,false,false).unwrap();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr+offset as u64);let b=c.clone();let e=expected(&b,signed,input,output,size,dst,&before[offset..]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e);assert_eq!(c.fuel,19);preserved(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}}
  for length in 0..span{let mut bytes=vec![0xaa;length];let before=bytes.clone();let addr=0x1_00300001;let mut memory=Memory::default();if length>0{memory.add(addr,&mut bytes,false,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(1,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
 }
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native packed extension checks:24 VEX forms, independent byte/signfill reference, everybytevalue and word/dword edge/random lanes, ignoredsourcebytepoison, high/selfaliases/WIG, XMMupperzero, exact2..16byte readonly high/unaligned/RIP input spans, everyshortspan, flags/FP/fuel/priorfaults");}
"#;
