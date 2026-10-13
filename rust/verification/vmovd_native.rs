//! Independent low32-byte-copy expectations for VEX VMOVD r/m32-to-XMM.
//! Intel SDM MOVD/MOVQ: VEX destination bits above31 are zeroed; no FP operation.
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
    let cases: [(&str, &[u8]); 5] = [
        ("observed", &[0xc5, 0xf9, 0x6e, 0xc1]), // xmm0,ecx; pinned0x13094d
        ("high", &[0xc4, 0x41, 0x79, 0x6e, 0xff]), // xmm15,r15d
        ("memory", &[0xc5, 0xf9, 0x6e, 0x07]),   // xmm0,[rdi]
        ("memory_high", &[0xc5, 0x79, 0x6e, 0x3f]), // xmm15,[rdi]
        ("rip", &[0xc5, 0xf9, 0x6e, 0x05, 0x28, 0, 0, 0]), // xmm0,[rip+40]
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes,
            0x100,
            &format!("vmovd_{name}"),
        )?);
    }
    for bytes in [
        &[0x66, 0x0f, 0x6e, 0xc1][..],
        &[0xc5, 0xfd, 0x6e, 0xc1][..],
    ] {
        if native_ps5::aot::translate_rust_memory_region(bytes, 0, "unsupported_vmovd").is_ok() {
            return Err("Unimplemented VMOVD variant accepted".into());
        }
    }
    let asm = directory.join("vmovd.S");
    let obj = directory.join("vmovd.o");
    let source = directory.join("vmovd-driver.rs");
    let binary = directory.join("vmovd-driver");
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
unsafe extern "C"{fn vmovd_observed(c:*mut Cpu);fn vmovd_high(c:*mut Cpu);fn vmovd_memory(c:*mut Cpu);fn vmovd_memory_high(c:*mut Cpu);fn vmovd_rip(c:*mut Cpu);}
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+1)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));}
fn expected(b:&Cpu,dst:usize,value:u32)->[[u8;32];16]{let mut v=b.vectors;v[dst]=[0;32];v[dst][..4].copy_from_slice(&value.to_le_bytes());v}
fn main(){let mut checks=0;
 let mut random=0x5a3b9c7d812346efu64;
 for n in 0..4096{random^=random<<13;random^=random>>7;random^=random<<17;let value=match n{0=>0,1=>u64::MAX,2=>0x80000000,3=>0xffffffff00000000,_=>random};for(block,dst,src)in[(vmovd_observed as unsafe extern "C" fn(*mut Cpu),0,1),(vmovd_high,15,15)]{let mut c=seed();c.registers[src]=value;let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected(&b,dst,value as u32));preserved(&c,&b);assert_eq!(c.fuel,19);checks+=1;}}
 for offset in [0,1,3,7,15,28]{for(block,dst,rip)in[(vmovd_memory as unsafe extern "C" fn(*mut Cpu),0,false),(vmovd_memory_high,15,false),(vmovd_rip,0,true)]{let mut bytes:Vec<u8>=(0..32).map(|i|(i*17+3)as u8).collect();let before=bytes.clone();let addr=0x1_00300001u64;let mut memory=Memory::default();memory.add(addr,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[7]=addr+offset;if rip{c.bias=addr+offset-0x130;}let b=c.clone();let value=u32::from_le_bytes(before[offset as usize..offset as usize+4].try_into().unwrap());unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected(&b,dst,value));preserved(&c,&b);assert_eq!(c.fuel,19);drop(memory);assert_eq!(bytes,before);checks+=1;}}
 for length in 0..4{for block in [vmovd_memory as unsafe extern "C" fn(*mut Cpu),vmovd_memory_high]{let mut bytes=vec![0xaa;length];let mut memory=Memory::default();let addr=0x1_00300001;if length>0{memory.add(addr,&mut bytes,false,false).unwrap();}let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[7]=addr;let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,1);assert_eq!(c.rip,b.rip);assert_eq!(c.vectors,b.vectors);preserved(&c,&b);drop(memory);assert_eq!(bytes,vec![0xaa;length]);checks+=1;}}
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{vmovd_observed(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));assert_eq!(c.vectors,b.vectors);preserved(&c,&b);checks+=1;
 println!("PASS: {checks} native VMOVD low32 bit copies, exact pinned encoding, r15d/xmm15, randomized sources, zero bits255:32, full64 unaligned/RIP-relative memory, four-byte bounds, failed-load atomicity, fuel guard, unchanged GPR/flags/FP controls");
}
"#;
