//! VEX128 VPXOR independent byte-wise oracle, including exact pinned encodings.
//! Intel instruction reference: bits127:0 = src1 XOR src2; upperYMM zero; no flags.
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
    let cases: [(&str, &[u8]); 11] = [
        ("distinct", &[0xc5, 0xf1, 0xef, 0xc2]),
        ("dst_first", &[0xc5, 0xf9, 0xef, 0xc1]),
        ("dst_second", &[0xc5, 0xf1, 0xef, 0xc0]),
        ("same_sources", &[0xc5, 0xf1, 0xef, 0xc1]),
        ("self0", &[0xc5, 0xf9, 0xef, 0xc0]),
        ("self1", &[0xc5, 0xf1, 0xef, 0xc9]),
        ("high", &[0xc4, 0x41, 0x09, 0xef, 0xfd]),
        ("rbp40", &[0xc5, 0xf9, 0xef, 0x45, 0xd8]),
        ("rbp72", &[0xc5, 0xf9, 0xef, 0x45, 0xb8]),
        ("rdx504", &[0xc5, 0xf9, 0xef, 0x82, 0xf8, 1, 0, 0]),
        ("memory_high", &[0xc5, 0x61, 0xef, 0x3f]),
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes,
            0x100,
            &format!("vpxor_{name}"),
        )?);
    }
    for bytes in [&[0xc5, 0xfd, 0xef, 0xc0][..], &[0x66, 0x0f, 0xef, 0xc0][..]] {
        if native_ps5::aot::translate_rust_memory_region(bytes, 0, "unsupported_vpxor").is_ok() {
            return Err("Unimplemented VPXOR width/legacy accepted".into());
        }
    }
    let asm = directory.join("vpxor.S");
    let obj = directory.join("vpxor.o");
    let source = directory.join("vpxor-driver.rs");
    let binary = directory.join("vpxor-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(&source, DRIVER).map_err(|e| e.to_string())?;
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
unsafe extern "C"{fn vpxor_distinct(c:*mut Cpu);fn vpxor_dst_first(c:*mut Cpu);fn vpxor_dst_second(c:*mut Cpu);fn vpxor_same_sources(c:*mut Cpu);fn vpxor_self0(c:*mut Cpu);fn vpxor_self1(c:*mut Cpu);fn vpxor_high(c:*mut Cpu);fn vpxor_rbp40(c:*mut Cpu);fn vpxor_rbp72(c:*mut Cpu);fn vpxor_rdx504(c:*mut Cpu);fn vpxor_memory_high(c:*mut Cpu);}
fn seed(random:&mut u64)->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for v in &mut c.vectors{for b in v{*random^=*random<<13;*random^=*random>>7;*random^=*random<<17;*b=*random as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));}
fn expected(b:&Cpu,dst:usize,first:usize,second:&[u8])->[[u8;32];16]{let mut v=b.vectors;v[dst]=[0;32];for i in 0..16{v[dst][i]=b.vectors[first][i]^second[i];}v}
fn main(){let mut checks=0;let mut random=0x1234567abcdefu64;
 for (block,dst,first,second) in [(vpxor_distinct as unsafe extern "C" fn(*mut Cpu),0,1,2),(vpxor_dst_first,0,0,1),(vpxor_dst_second,0,1,0),(vpxor_same_sources,0,1,1),(vpxor_self0,0,0,0),(vpxor_self1,1,1,1),(vpxor_high,15,14,13)] {for _ in 0..1024{let mut c=seed(&mut random);let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected(&b,dst,first,&b.vectors[second]));assert_eq!((c.fuel,c.stop),(19,2));preserved(&c,&b);checks+=1;}}
 for (block,dst,first,base,disp) in [(vpxor_rbp40 as unsafe extern "C" fn(*mut Cpu),0,0,5,-40i64),(vpxor_rbp72,0,0,5,-72),(vpxor_rdx504,0,0,2,504),(vpxor_memory_high,15,3,7,0)]{
 for offset in [0,1,7,15,31,48]{let mut data:Vec<u8>=(0..64).map(|i|(i*17+3)as u8).collect();let original=data.clone();let addr=0x1_00300001u64;let mut memory=Memory::default();memory.add(addr,&mut data,false,false).unwrap();let mut c=seed(&mut random);c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[base]=(addr+offset).wrapping_add_signed(-disp);let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected(&b,dst,first,&original[offset as usize..offset as usize+16]));assert_eq!((c.fuel,c.stop),(19,2));preserved(&c,&b);drop(memory);assert_eq!(data,original);checks+=1;}
 // Every insufficient size tests whole-span validation before changing any lane,
 // including aliased src1/destination and high register destination.
 for length in 0..16{let addr=0x1_00300001u64;let mut data=vec![0xaa;length];let mut memory=Memory::default();if length>0{memory.add(addr,&mut data,false,false).unwrap();}let mut c=seed(&mut random);c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[base]=addr.wrapping_add_signed(-disp);let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,1);assert_eq!(c.rip,b.rip);assert_eq!(c.vectors,b.vectors);preserved(&c,&b);drop(memory);assert_eq!(data,vec![0xaa;length]);checks+=1;}
 }
 let mut c=seed(&mut random);c.fuel=0;let b=c.clone();unsafe{vpxor_self0(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));assert_eq!(c.vectors,b.vectors);preserved(&c,&b);checks+=1;
 println!("PASS: {checks} native VEX128 VPXOR byte-oracle cases, all register alias classes/high registers, pinned selfzero/RBPnegative/RDX504 encodings, upperYMMzero, full64 unaligned readonly memory, full-span fault atomicity, fuel and unchanged GPR/flags/FP");
}
"#;
