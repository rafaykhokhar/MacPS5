//! Legacy MOVDQU byte-copy oracle; independent expected lanes and unchanged state.
use std::{path::Path, process::Command};
fn run(command: &mut Command) -> Result<(), String> {
    let result = command.output().map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned());
    }
    print!("{}", String::from_utf8_lossy(&result.stdout));
    Ok(())
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let cases: [(&str, &[u8]); 8] = [
        ("load", &[0xf3, 0x0f, 0x6f, 0x07]),
        ("store", &[0xf3, 0x0f, 0x7f, 0x07]),
        ("reg", &[0xf3, 0x0f, 0x6f, 0xc1]),
        ("reg_reverse", &[0xf3, 0x0f, 0x7f, 0xc1]),
        ("selfcopy", &[0xf3, 0x0f, 0x6f, 0xc0]),
        ("high", &[0xf3, 0x44, 0x0f, 0x6f, 0x3f]),
        ("abort_load", &[0xf3, 0x0f, 0x6f, 0x83, 0xd0, 0, 0, 0]),
        ("vex_load", &[0xc5, 0xfa, 0x6f, 0x07]),
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes,
            0x357,
            &format!("movdqu_{name}"),
        )?);
    }
    let asm = directory.join("movdqu.S");
    let obj = directory.join("movdqu.o");
    let source = directory.join("movdqu-driver.rs");
    let binary = directory.join("movdqu-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(&source, DRIVER).map_err(|e| e.to_string())?;
    run(Command::new("xcrun")
        .args(["clang", "-arch", "arm64", "-c"])
        .arg(&asm)
        .arg("-o")
        .arg(&obj))?;
    let profile = archive.parent().ok_or("Archive missing parent")?;
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
unsafe extern "C" {
 fn movdqu_load(c:*mut Cpu); fn movdqu_store(c:*mut Cpu); fn movdqu_reg(c:*mut Cpu);
 fn movdqu_reg_reverse(c:*mut Cpu); fn movdqu_selfcopy(c:*mut Cpu); fn movdqu_high(c:*mut Cpu);
 fn movdqu_abort_load(c:*mut Cpu); fn movdqu_vex_load(c:*mut Cpu);
}
fn seed() -> Cpu {
 let mut c=Cpu{rip:0x357,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};
 c.registers=[0x12345678;16];
 for (r,v) in c.vectors.iter_mut().enumerate(){for (i,b) in v.iter_mut().enumerate(){*b=(r*37+i*11)as u8;}}
 c
}
fn unchanged(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));}
fn main(){
 for (block,dst,src) in [(movdqu_reg as unsafe extern "C" fn(*mut Cpu),0,1),(movdqu_reg_reverse,1,0),(movdqu_selfcopy,0,0)]{
  let mut c=seed();let before=c.clone();let mut expected=c.vectors;expected[dst][..16].copy_from_slice(&before.vectors[src][..16]);
  unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected);unchanged(&c,&before);
 }
 for offset in [0,1,7,15,31]{
  for (block,dst,vex) in [(movdqu_load as unsafe extern "C" fn(*mut Cpu),0,false),(movdqu_high,15,false),(movdqu_abort_load,0,false),(movdqu_vex_load,0,true)]{
   let mut data:Vec<u8>=(0..64).map(|i|(i*19+3)as u8).collect();let original=data.clone();let mut mem=Memory::default();mem.add(0x300000,&mut data,false,false).unwrap();
   let mut c=seed();c.memory=(&mut mem as *mut Memory<'_>)as u64;c.registers[7]=0x300000+offset;c.registers[3]=0x300000+offset-0xd0;let before=c.clone();let mut expected=c.vectors;
   expected[dst][..16].copy_from_slice(&original[offset as usize..offset as usize+16]);if vex{expected[dst][16..].fill(0);}
   unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected);unchanged(&c,&before);drop(mem);assert_eq!(data,original);
  }
  let mut data=vec![0xa5;64];let mut expected=data.clone();let mut mem=Memory::default();mem.add(0x300000,&mut data,true,false).unwrap();let mut c=seed();c.memory=(&mut mem as *mut Memory<'_>)as u64;c.registers[7]=0x300000+offset;let before=c.clone();expected[offset as usize..offset as usize+16].copy_from_slice(&c.vectors[0][..16]);unsafe{movdqu_store(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,before.vectors);unchanged(&c,&before);drop(mem);assert_eq!(data,expected);
 }
 for length in [0,1,8,15]{for (block,fault) in [(movdqu_load as unsafe extern "C" fn(*mut Cpu),1),(movdqu_store,2),(movdqu_high,1)]{
  let mut data=vec![0xa5;length];let original=data.clone();let mut mem=Memory::default();if length>0{mem.add(0x300001,&mut data,true,false).unwrap();}
  let mut c=seed();c.memory=(&mut mem as *mut Memory<'_>)as u64;c.registers[7]=0x300001;let before=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,fault);assert_eq!(c.rip,before.rip);assert_eq!(c.vectors,before.vectors);unchanged(&c,&before);drop(mem);assert_eq!(data,original);
 }}
 let mut data=[0xa5;16];let mut mem=Memory::default();mem.add(0x300001,&mut data,false,false).unwrap();let mut c=seed();c.memory=(&mut mem as *mut Memory<'_>)as u64;c.registers[7]=0x300001;let before=c.clone();unsafe{movdqu_store(&mut c)};assert_eq!(c.fault,2);assert_eq!(c.vectors,before.vectors);unchanged(&c,&before);drop(mem);assert_eq!(data,[0xa5;16]);
 println!("PASS: native legacy MOVDQU register/load/store, unaligned byte copies, xmm15/selfalias, exact abort displacement form, upperYMM preservation vs VEX128 zeroing, guarded load/store atomicity, unchanged flags/FP/GPRs");
}
"#;
