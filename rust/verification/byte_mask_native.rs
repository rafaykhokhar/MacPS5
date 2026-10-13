//! Independent byte-sign extraction oracle for VEX VPMOVMSKB.
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
    for wide in 0..2 {
        for w in 0..2 {
            for dst in 0..16u8 {
                let source = 15 - dst;
                let size = if wide == 0 { 16 } else { 32 };
                let bytes = vec![
                    0xc4,
                    0x41 | if dst < 8 { 128 } else { 0 } | if source < 8 { 32 } else { 0 },
                    0x79 | wide * 4 | w * 128,
                    0xd7,
                    0xc0 | ((dst & 7) << 3) | (source & 7),
                ];
                let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                let code = format!(
                    "VEX_Vpmovmskb_r{}_{}",
                    if w == 0 { 32 } else { 64 },
                    if wide == 0 { "xmm" } else { "ymm" }
                );
                let gpr32 = [
                    "EAX", "ECX", "EDX", "EBX", "ESP", "EBP", "ESI", "EDI", "R8D", "R9D", "R10D",
                    "R11D", "R12D", "R13D", "R14D", "R15D",
                ];
                let gpr64 = [
                    "RAX", "RCX", "RDX", "RBX", "RSP", "RBP", "RSI", "RDI", "R8", "R9", "R10",
                    "R11", "R12", "R13", "R14", "R15",
                ];
                if format!("{:?}", d.code()) != code
                    || d.len() != bytes.len()
                    || d.op_count() != 2
                    || format!("{:?}", d.op0_register())
                        != if w == 0 {
                            gpr32[dst as usize]
                        } else {
                            gpr64[dst as usize]
                        }
                    || format!("{:?}", d.op1_register())
                        != format!("{}{source}", if wide == 0 { "XMM" } else { "YMM" })
                {
                    return Err(format!("Bad byte mask {wide}/{w}/{dst}: {d:?}"));
                }
                let mut memory = bytes.clone();
                memory[4] &= 0x3f;
                let mut reserved = bytes.clone();
                reserved[2] &= !8;
                let mut invalids = vec![
                    memory,
                    reserved,
                    vec![0x66, 0x0f, 0xd7, 0xc1],
                    vec![0x62, 0xf1, 0x7d, 0x08, 0xd7, 0xc1],
                ];
                for prefix in [0x66, 0xf2, 0xf3, 0xf0] {
                    let mut bad = vec![prefix];
                    bad.extend_from_slice(&bytes);
                    invalids.push(bad);
                }
                for invalid in invalids {
                    if native_ps5::aot::translate_rust_memory_region(
                        &invalid,
                        0x100,
                        "bad_byte_mask",
                    )
                    .is_ok()
                    {
                        return Err(format!("Unsupported byte mask accepted: {invalid:02x?}"));
                    }
                }
                let name = format!("byte_mask_{wide}_{w}_{dst}");
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                table.push_str(&format!("({name},{size},{dst},{source}),\n"));
                forms += 1;
            }
        }
    }
    let asm = directory.join("byte-mask.S");
    let obj = directory.join("byte-mask.o");
    let source = directory.join("byte-mask-driver.rs");
    let binary = directory.join("byte-mask-driver");
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
    println!("Decoded {forms} byte mask fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::legacy_cpu::Cpu;
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.vectors,b.vectors);assert_eq!(c.registers,b.registers);}
fn masks(size:usize,full:bool)->Vec<u64>{let limit=if size==16{0xffff}else{0xffffffff};let mut masks=vec![0,limit,0xaaaaaaaau64&limit,0x55555555&limit,0xffff,0xffff0000&limit,0x80000000&limit];for bit in 0..size{masks.push(1u64<<bit);masks.push(limit^(1u64<<bit));}if full{for low in 0..=65535u16{let high=low.rotate_left(7)^0xa59c;masks.push(u64::from(low)|if size==32{u64::from(high)<<16}else{0});}}let mut random=0x831725764bada557u64;for _ in 0..256{random^=random<<13;random^=random>>7;random^=random<<17;masks.push(random&limit);}masks}
fn main(){let cases:&[(Block,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,size,dst,source)in cases{for mask in masks(size,dst==0){for poison in [0u8,0x55,0x7f]{let mut c=seed();c.mxcsr=if poison==0{0}else if poison==0x55{0xffff}else{0x10000};c.vectors[source]=[0x80|poison;32];for lane in 0..size{c.vectors[source][lane]=poison+(((mask>>lane)%2)as u8*128);}let b=c.clone();let mut regs=b.registers;regs[dst]=mask;unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.fuel,19);assert_eq!(c.registers,regs);assert_eq!(c.vectors,b.vectors);preserved(&c,&b);checks+=1;}}
for bit in 0..256{let mut c=seed();c.vectors[source]=[0;32];c.vectors[source][bit/8]=1<<(bit%8);let b=c.clone();let expected=if bit%8==7&&bit/8<size{1u64<<(bit/8)}else{0};let mut regs=b.registers;regs[dst]=expected;unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.fuel,19);assert_eq!(c.registers,regs);assert_eq!(c.vectors,b.vectors);preserved(&c,&b);checks+=1;}
let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VPMOVMSKB checks:4exactVEXcodes, byte-sign oracle, all65536XMM masks andYMMlow16patterns withvariedupper16, full/alternating/bit31/walking/random masks andnonsignpoison, all16GPRdestinations/allvectorregs, r32/r64fullzeroextension, preservedvectors/flags/FP/fuel/priorfault");}
"#;
