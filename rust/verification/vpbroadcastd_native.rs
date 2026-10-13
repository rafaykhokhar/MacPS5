//! AMD APM Vol4 rev3.24 pp702–703: low32 broadcast, XMM upper clearing; no flags/FP effects.
//! https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf
//! Independent byte-array oracle, not emitter instruction mirroring.
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
    let mut declarations = String::from("unsafe extern \"C\" {\n");
    let mut table = String::from("fn register_cases()->Vec<(Block,usize,usize,usize)>{vec![");
    for wide in 0..2 {
        for dst in 0..16 {
            for src in 0..16 {
                let name = format!("broadcast_{wide}_{dst}_{src}");
                let vex =
                    0xe2 ^ (if dst >= 8 { 0x80 } else { 0 }) ^ (if src >= 8 { 0x20 } else { 0 });
                let bytes = [
                    0xc4,
                    vex,
                    0x79 | (wide * 4) as u8,
                    0x58,
                    0xc0 | ((dst & 7) << 3) as u8 | (src & 7) as u8,
                ];
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                table.push_str(&format!(
                    "({name},{dst},{src},{}),",
                    if wide == 0 { 16 } else { 32 }
                ));
            }
        }
    }
    declarations.push_str("}\n");
    table.push_str("]}\n");
    for (name, bytes) in [
        ("mem128", &[0xc4, 0xe2, 0x79, 0x58, 0x07][..]),
        ("mem256", &[0xc4, 0x62, 0x7d, 0x58, 0x3f][..]),
        (
            "rip128",
            &[0xc4, 0xe2, 0x79, 0x58, 0x35, 0xbb, 0x06, 0x16, 0][..],
        ),
        (
            "rip256",
            &[0xc4, 0xe2, 0x7d, 0x58, 0x3d, 0xb2, 0x06, 0x16, 0][..],
        ),
    ] {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0x100, name,
        )?);
    }
    let asm = directory.join("vpbroadcastd.S");
    let obj = directory.join("vpbroadcastd.o");
    let source = directory.join("vpbroadcastd-driver.rs");
    let binary = directory.join("vpbroadcastd-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(&source, format!("{DRIVER}\n{declarations}{table}"))
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
type Block=unsafe extern "C" fn(*mut Cpu);
unsafe extern "C"{fn mem128(c:*mut Cpu);fn mem256(c:*mut Cpu);fn rip128(c:*mut Cpu);fn rip256(c:*mut Cpu);}
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+1)as u8;}}c.x87.push_raw(c.x87_control,&mut c.x87_status,[0;10]).unwrap();c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!(c.x87,b.x87);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));}
fn expected(b:&Cpu,dst:usize,width:usize,value:[u8;4])->[[u8;32];16]{let mut v=b.vectors;v[dst]=[0;32];for lane in v[dst][..width].chunks_exact_mut(4){lane.copy_from_slice(&value);}v}
fn main(){let mut checks=0;let cases=register_cases();let mut random=0x5a3b9c7d812346efu64;
 for n in 0..128{random^=random<<13;random^=random>>7;random^=random<<17;let value=match n{0=>0,1=>u32::MAX,2=>0x80000000,3=>0x7f800001,4=>0x01020304,_=>random as u32}.to_le_bytes();for &(block,dst,src,width)in &cases{let mut c=seed();c.vectors[src][..4].copy_from_slice(&value);let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected(&b,dst,width,value));preserved(&c,&b);assert_eq!(c.fuel,19);checks+=1;}}
 let memory_cases:[(Block,usize,usize,Option<u64>);4]=[(mem128,0,16,None),(mem256,15,32,None),(rip128,6,16,Some(0x1607c4)),(rip256,7,32,Some(0x1607bb))];
 for offset in [0,1,3,7,15,28]{for &(block,dst,width,rip)in &memory_cases{let mut bytes:Vec<u8>=(0..32).map(|i|(i*17+3)as u8).collect();let before=bytes.clone();let addr=0x1_00300001u64;let mut memory=Memory::default();memory.add(addr,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[7]=addr+offset;if let Some(target)=rip{c.bias=addr+offset-target;}let b=c.clone();let value=before[offset as usize..offset as usize+4].try_into().unwrap();unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected(&b,dst,width,value));preserved(&c,&b);assert_eq!(c.fuel,19);drop(memory);assert_eq!(bytes,before);checks+=1;}}
 for length in 0..4{for &(block,_,_,rip)in &memory_cases{let mut bytes=vec![0xaa;length];let mut memory=Memory::default();let addr=0x1_00300001;if length>0{memory.add(addr,&mut bytes,false,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[7]=addr;if let Some(target)=rip{c.bias=addr-target;}let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,1);assert_eq!(c.rip,b.rip);assert_eq!(c.vectors,b.vectors);preserved(&c,&b);drop(memory);assert_eq!(bytes,vec![0xaa;length]);checks+=1;}}
 for block in [mem128 as Block,mem256]{let mut bytes=[0xaa;3];let mut memory=Memory::default();memory.add(u64::MAX-3,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[7]=u64::MAX-3;let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,1);assert_eq!(c.vectors,b.vectors);preserved(&c,&b);checks+=1;}
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{(cases[0].0)(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));assert_eq!(c.vectors,b.vectors);preserved(&c,&b);checks+=1;
 println!("PASS: {checks} native VPBROADCASTD checks: both widths, every XMM source/destination pairing, independent lane-byte reference, dirty uppers/alias clearing, pinned RIP memory encodings, exact four-byte reads, full64/unaligned/readonly bounds, atomic faults, unchanged flags/GPR/FP and fuel stop");
}
"#;
