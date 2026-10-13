//! Independent byte-merge and per-lane sign-bit oracle for register vector moves.
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
    for op in 0..4 {
        for wide in 0..if op < 2 { 1 } else { 2 } {
            for w in 0..2 {
                for mode in 0..16u8 {
                    let dst = mode;
                    let mut first = (mode + 1) % 16;
                    let mut second = if op < 2 { (mode + 8) % 16 } else { 15 - mode };
                    if op < 2 {
                        match mode {
                            0 => first = dst,
                            1 => second = dst,
                            2 => {
                                first = dst;
                                second = dst
                            }
                            3 => second = first,
                            _ => {}
                        }
                    }
                    let size = if wide == 0 { 16 } else { 32 };
                    let pp = if op == 3 { 1 } else { 0 };
                    let opcode = match op {
                        0 => 0x16,
                        1 => 0x12,
                        _ => 0x50,
                    };
                    let bytes = vec![
                        0xc4,
                        0x41 | if dst < 8 { 0x80 } else { 0 } | if second < 8 { 0x20 } else { 0 },
                        (if op < 2 { (!first & 15) << 3 } else { 0x78 }) | pp | wide * 4 | w * 128,
                        opcode,
                        0xc0 | ((dst & 7) << 3) | (second & 7),
                    ];
                    let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                    let code = if op < 2 {
                        format!(
                            "VEX_Vmov{}_xmm_xmm_xmm",
                            if op == 0 { "lhps" } else { "hlps" }
                        )
                    } else {
                        format!(
                            "VEX_Vmovmsk{}_r{}_{}",
                            if op == 2 { "ps" } else { "pd" },
                            if w == 0 { 32 } else { 64 },
                            if wide == 0 { "xmm" } else { "ymm" }
                        )
                    };
                    let gpr32 = [
                        "EAX", "ECX", "EDX", "EBX", "ESP", "EBP", "ESI", "EDI", "R8D", "R9D",
                        "R10D", "R11D", "R12D", "R13D", "R14D", "R15D",
                    ];
                    let gpr64 = [
                        "RAX", "RCX", "RDX", "RBX", "RSP", "RBP", "RSI", "RDI", "R8", "R9", "R10",
                        "R11", "R12", "R13", "R14", "R15",
                    ];
                    let mut valid = format!("{:?}", d.code()) == code
                        && d.len() == bytes.len()
                        && d.op_count() == if op < 2 { 3 } else { 2 };
                    if op < 2 {
                        valid &= format!("{:?}", d.op0_register()) == format!("XMM{dst}")
                            && format!("{:?}", d.op1_register()) == format!("XMM{first}")
                            && format!("{:?}", d.op2_register()) == format!("XMM{second}");
                    } else {
                        valid &= format!("{:?}", d.op0_register())
                            == if w == 0 {
                                gpr32[dst as usize]
                            } else {
                                gpr64[dst as usize]
                            }
                            && format!("{:?}", d.op1_register())
                                == format!("{}{second}", if wide == 0 { "XMM" } else { "YMM" });
                    }
                    if !valid {
                        return Err(format!(
                            "Bad register vector fixture {op}/{wide}/{w}/{mode}: {d:?}"
                        ));
                    }
                    let mut memory = bytes.clone();
                    memory[4] &= 0x3f;
                    let mut legacy = if pp == 1 {
                        vec![0x66, 0x0f]
                    } else {
                        vec![0x0f]
                    };
                    legacy.extend_from_slice(&[opcode, 0xc1]);
                    let mut invalids = vec![
                        memory,
                        legacy,
                        vec![0x62, 0xf1, 0x7c | pp, 0x08, opcode, 0xc1],
                    ];
                    if op < 2 {
                        let mut bad_l = bytes.clone();
                        bad_l[2] |= 4;
                        invalids.push(bad_l);
                    } else {
                        let mut reserved = bytes.clone();
                        reserved[2] &= !8;
                        invalids.push(reserved);
                    }
                    for prefix in [0x66, 0xf2, 0xf3, 0xf0] {
                        let mut bad = vec![prefix];
                        bad.extend_from_slice(&bytes);
                        invalids.push(bad);
                    }
                    for invalid in invalids {
                        if native_ps5::aot::translate_rust_memory_region(
                            &invalid,
                            0x100,
                            "bad_register_vector",
                        )
                        .is_ok()
                        {
                            return Err(format!(
                                "Unsupported register vector form accepted: {invalid:02x?}"
                            ));
                        }
                    }
                    let name = format!("vector_register_{op}_{wide}_{w}_{mode}");
                    assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                        &bytes, 0x100, &name,
                    )?);
                    declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                    table.push_str(&format!("({name},{op},{size},{dst},{first},{second}),\n"));
                    forms += 1;
                }
            }
        }
    }
    let asm = directory.join("vector-register.S");
    let obj = directory.join("vector-register.o");
    let source = directory.join("vector-register-driver.rs");
    let binary = directory.join("vector-register-driver");
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
    println!("Decoded {forms} vector register fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::legacy_cpu::Cpu;
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.vectors,b.vectors);assert_eq!(c.registers,b.registers);}
fn vector(pattern:usize)->[u8;32]{let mut out=[0;32];let mut random=0xaab129387465bcdeu64^pattern as u64;let specials=[0,1,0x8000000000000000,0x000fffffffffffff,0x0010000000000000,0x7ff0000000000000,0xfff0000000000000,0x7ff0000000000001,0x7ff8deadbeef1234,0xfff876543210abcd,u64::MAX];for lane in 0..4{random^=random<<13;random^=random>>7;random^=random<<17;let value=if pattern<specials.len(){specials[(pattern+lane)%specials.len()]}else{random};out[lane*8..lane*8+8].copy_from_slice(&value.to_le_bytes());}out}
fn check(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.fault,0);assert_eq!(c.fuel,19);}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,op,size,dst,first,second)in cases{
 if op<2{for pattern in 0..128{let mut c=seed();c.mxcsr=[0,0xffff,0x10000][pattern%3];c.vectors[first]=vector(pattern);c.vectors[second]=vector(pattern+3);let b=c.clone();let mut expected=b.vectors;expected[dst]=[0;32];if op==0{expected[dst][..8].copy_from_slice(&b.vectors[first][..8]);expected[dst][8..16].copy_from_slice(&b.vectors[second][..8]);}else{expected[dst][..8].copy_from_slice(&b.vectors[second][8..16]);expected[dst][8..16].copy_from_slice(&b.vectors[first][8..16]);}unsafe{block(&mut c)};check(&c,&b);assert_eq!(c.registers,b.registers);assert_eq!(c.vectors,expected);checks+=1;}}
 else{let width=if op==2{4}else{8};let lanes=size/width;for mask in 0..1u64<<lanes{for poison in [0u8,0x7f,0xff]{let mut c=seed();c.vectors[second]=[poison;32];for lane in 0..lanes{let at=(lane+1)*width-1;c.vectors[second][at]=(poison&0x7f)|(((mask>>lane)%2)as u8*128);}let b=c.clone();let mut regs=b.registers;regs[dst]=mask;unsafe{block(&mut c)};check(&c,&b);assert_eq!(c.registers,regs);assert_eq!(c.vectors,b.vectors);checks+=1;}}
 for bit in 0..256{let mut c=seed();c.vectors[second]=[0;32];c.vectors[second][bit/8]=1<<(bit%8);let b=c.clone();let mut mask=0;for lane in 0..lanes{if b.vectors[second][(lane+1)*width-1]>=128{mask+=1u64<<lane;}}let mut regs=b.registers;regs[dst]=mask;unsafe{block(&mut c)};check(&c,&b);assert_eq!(c.registers,regs);assert_eq!(c.vectors,b.vectors);checks+=1;}
 for pattern in 0..128{let mut c=seed();c.vectors[second]=vector(pattern);c.mxcsr=[0,0xffff,0x10000][pattern%3];let b=c.clone();let mut mask=0;for lane in 0..lanes{if b.vectors[second][(lane+1)*width-1]>=128{mask+=1u64<<lane;}}let mut regs=b.registers;regs[dst]=mask;unsafe{block(&mut c)};check(&c,&b);assert_eq!(c.registers,regs);assert_eq!(c.vectors,b.vectors);checks+=1;}}
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VMOVLHPS/HLPS and VMOVMSKPS/PD checks:10exactVEXcodes, independent byte merge/signbit oracle, rawFP/random/alias/high vectors, exhaustive masks/nonsignpoison/all256bitpositions, all16GPRdestinations includingRSP andfull64zeroextension, WIGhalfmove andW0/W1maskforms, guestflags/FP/fuel/priorfault");}
"#;
