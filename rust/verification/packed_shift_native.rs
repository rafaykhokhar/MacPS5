//! Independent widened integer-lane oracle for immediate VEX packed shifts.
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
    for bits in [16usize, 32, 64] {
        for op in 0..3 {
            if op == 2 && bits == 64 {
                continue;
            }
            for wide in 0..2 {
                for mode in 0..6 {
                    let (dst, src) = match mode {
                        0 => (2, 1),
                        1 => (15, 8),
                        2 => (8, 15),
                        3 => (15, 15),
                        4 => (2, 2),
                        5 => (15, 8),
                        _ => unreachable!(),
                    };
                    let mut counts: Vec<u8> = if mode == 0 {
                        (0..=255).collect()
                    } else {
                        vec![
                            0,
                            1,
                            (bits - 1) as u8,
                            bits as u8,
                            (bits + 1) as u8,
                            31,
                            32,
                            63,
                            64,
                            127,
                            128,
                            255,
                        ]
                    };
                    counts.sort_unstable();
                    counts.dedup();
                    for imm in counts {
                        let opcode = match bits {
                            16 => 0x71,
                            32 => 0x72,
                            64 => 0x73,
                            _ => unreachable!(),
                        };
                        let extension = match op {
                            0 => 6,
                            1 => 2,
                            2 => 4,
                            _ => unreachable!(),
                        };
                        let bytes = [
                            0xc4,
                            0xc1 | if src < 8 { 0x20 } else { 0 },
                            ((!dst & 15) << 3) | 1 | wide * 4 | if mode == 5 { 0x80 } else { 0 },
                            opcode,
                            0xc0 | (extension << 3) | (src & 7),
                            imm,
                        ];
                        let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                        let namepart = format!(
                            "{}{}",
                            match op {
                                0 => "Vpsll",
                                1 => "Vpsrl",
                                2 => "Vpsra",
                                _ => unreachable!(),
                            },
                            match bits {
                                16 => "w",
                                32 => "d",
                                64 => "q",
                                _ => unreachable!(),
                            }
                        );
                        let v = if wide == 0 { "xmm" } else { "ymm" };
                        let reg = if wide == 0 { "XMM" } else { "YMM" };
                        let code = format!("VEX_{namepart}_{v}_{v}_imm8");
                        if format!("{:?}", d.code()) != code
                            || d.len() != bytes.len()
                            || d.op_count() != 3
                            || format!("{:?}", d.op0_kind()) != "Register"
                            || format!("{:?}", d.op1_kind()) != "Register"
                            || format!("{:?}", d.op0_register()) != format!("{reg}{dst}")
                            || format!("{:?}", d.op1_register()) != format!("{reg}{src}")
                            || format!("{:?}", d.op2_kind()) != "Immediate8"
                            || d.immediate8() != imm
                        {
                            return Err(format!(
                                "Bad packed shift {bits}/{op}/{wide}/{mode}/{imm}: {d:?}"
                            ));
                        }
                        if mode == 0 && imm == 1 {
                            let mut memory = bytes;
                            memory[4] &= 0x3f;
                            let legacy = [0x66, 0x0f, opcode, 0xc0 | (extension << 3) | 1, imm];
                            let evex = [
                                0x62,
                                0xf1,
                                0x7d,
                                0x08,
                                opcode,
                                0xc0 | (extension << 3) | 1,
                                imm,
                            ];
                            let variable_opcode = match op {
                                0 => match bits {
                                    16 => 0xf1,
                                    32 => 0xf2,
                                    64 => 0xf3,
                                    _ => unreachable!(),
                                },
                                1 => match bits {
                                    16 => 0xd1,
                                    32 => 0xd2,
                                    64 => 0xd3,
                                    _ => unreachable!(),
                                },
                                2 => {
                                    if bits == 16 {
                                        0xe1
                                    } else {
                                        0xe2
                                    }
                                }
                                _ => unreachable!(),
                            };
                            let variable = [0xc4, 0xe1, 0x69 | wide * 4, variable_opcode, 0xd1];
                            for invalid in [&memory[..], &legacy[..], &evex[..], &variable[..]] {
                                if native_ps5::aot::translate_rust_memory_region(
                                    invalid,
                                    0x100,
                                    "unsupported_packed_shift",
                                )
                                .is_ok()
                                {
                                    return Err(format!(
                                        "Unsupported packed shift accepted: {invalid:02x?}"
                                    ));
                                }
                            }
                        }
                        let name = format!("packed_shift_{bits}_{op}_{wide}_{mode}_{imm}");
                        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                            &bytes, 0x100, &name,
                        )?);
                        declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                        table.push_str(&format!(
                            "({name},{bits},{op},{},{dst},{src},{imm}),\n",
                            if wide == 0 { 16 } else { 32 }
                        ));
                        forms += 1;
                    }
                }
            }
        }
    }
    let asm = directory.join("packed-shift.S");
    let obj = directory.join("packed-shift.o");
    let source = directory.join("packed-shift-driver.rs");
    let binary = directory.join("packed-shift-driver");
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
    println!("Decoded {forms} immediate packed-shift fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::legacy_cpu::Cpu;
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.vectors,b.vectors);}
fn expected(b:&Cpu,bits:usize,op:usize,size:usize,dst:usize,src:usize,count:usize)->[[u8;32];16]{let mut result=b.vectors;let mut out=[0;32];let width=bits/8;let modulus=1u128<<bits;
 for lane in 0..size/width{let at=lane*width;let mut raw=[0;8];raw[..width].copy_from_slice(&b.vectors[src][at..at+width]);let value=u64::from_le_bytes(raw)as u128;
  let shifted=if op==2{let signed=if value>=modulus/2{value as i128-modulus as i128}else{value as i128};let divisor=1i128<<count.min(bits);signed.div_euclid(divisor).rem_euclid(modulus as i128)as u128}else if count>=bits{0}else if op==0{(value*(1u128<<count))%modulus}else{value/(1u128<<count)};
  out[at..at+width].copy_from_slice(&shifted.to_le_bytes()[..width]);
 }result[dst]=out;result}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,bits,op,size,dst,src,count) in cases{
 for pattern in 0..12{let mut c=seed();let width=bits/8;let mask=u64::MAX>>(64-bits);let sign=1u64<<(bits-1);let mut random=0x8fedcba123456789u64.wrapping_add(pattern as u64);
  for lane in 0..32/width{random^=random<<13;random^=random>>7;random^=random<<17;let value=match pattern{0=>0,1=>mask,2=>sign,3=>sign-1,4=>1,5=>if lane%2==0{sign}else{mask},6=>1u64<<((lane*7)%bits),7=>(lane as u64*0x1234567)&mask,_=>random&mask};c.vectors[src][lane*width..(lane+1)*width].copy_from_slice(&value.to_le_bytes()[..width]);}
  let b=c.clone();let e=expected(&b,bits,op,size,dst,src,count);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,e,"bits {bits},op {op},size {size},dst {dst},src {src},count {count},pattern {pattern}");assert_eq!(c.fuel,19);preserved(&c,&b);checks+=1;
 }
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native immediate packed-shift checks:16 exact VEX forms, all256 counts, widened per-lane arithmetic reference, sign/edge/random distinct lanes, high/self aliases, WIG=1, oversized counts, XMM upperzero, flags/FP/fuel/preexisting faults");}
"#;
