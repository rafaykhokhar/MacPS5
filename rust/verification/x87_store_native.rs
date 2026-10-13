//! Native exact FSTP m80 raw store/pop and guarded memory checks.
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
    let assembly =
        native_ps5::aot::translate_rust_memory_region(&[0xdb, 0x3f], 0x1772da, "fstp80")?;
    let asm = directory.join("x87-store.S");
    let obj = directory.join("x87-store.o");
    let source = directory.join("x87-store-driver.rs");
    let binary = directory.join("x87-store-driver");
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
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory,x87_state::{top,Tag,INDEFINITE}};
unsafe extern "C" { fn fstp80(cpu:*mut Cpu); }
const ADDRESS:u64=0x1_00300001;
fn raw(sig:u64,exp:u16)->[u8;10]{let mut v=[0;10];v[..8].copy_from_slice(&sig.to_le_bytes());v[8..].copy_from_slice(&exp.to_le_bytes());v}
fn seed(cw:u16,t:usize,value:Option<[u8;10]>)->Cpu{
 let mut c=Cpu{rip:0x1772da,fuel:20,flags:0xad7,mxcsr:0x5fa5,x87_control:cw,x87_status:(((t+1)&7)as u16)<<11,..Cpu::default()};
 c.registers=[0xfedcba9876543210;16];c.registers[7]=ADDRESS;
 for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+1)as u8;}}
 if let Some(v)=value{c.x87.push_raw(cw,&mut c.x87_status,v).unwrap();}else{c.x87_status=(t as u16)<<11;}
 // C1 starts set: successful FSTP/empty-stack fault clears it.
 c.x87_status|=0x4700;c
}
fn unaffected(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!(c.vectors,b.vectors);assert_eq!(c.mxcsr,b.mxcsr);assert_eq!(c.x87_control,b.x87_control);assert_eq!(c.fuel,19);}
fn main(){let mut count=0;
 let values=[raw(0,0),raw(0,0x8000),raw(1<<63,0x7fff),raw(1<<63,0xffff),raw(0xc000000000000123,0x7fff),raw(0xc000000000000123,0xffff),raw(0x8000000000000001,0x3fff),raw(u64::MAX,0x7ffe),raw(1<<63,1)];
 for t in 0..8{for pc in [0,2,3]{for rc in 0..4{let cw=0x007f|(pc<<8)|(rc<<10);for value in values{
  let mut bytes=[0xa5;12];let mut memory=Memory::default();memory.add(ADDRESS-1,&mut bytes,true,false).unwrap();
  let mut c=seed(cw,t,Some(value));c.memory=(&mut memory as *mut Memory<'_>)as u64;let b=c.clone();unsafe{fstp80(&mut c)};
  assert_eq!(c.fault,0);assert_eq!(top(c.x87_status),(t+1)&7);assert_eq!(c.x87.physical_tag(t).unwrap(),Tag::Empty);assert_eq!(c.x87_status,(b.x87_status&!0x3a00)|((((t+1)&7)as u16)<<11));unaffected(&c,&b);
  drop(memory);assert_eq!(&bytes[1..11],&value);assert_eq!((bytes[0],bytes[11]),(0xa5,0xa5));count+=1;
 }}}}
 // Every insufficient span and read-only span preserves all x87 state and bytes.
 for length in 0..=10{for writable in [false,true]{if length==10&&writable{continue;}let mut bytes=vec![0xa5;length];let mut memory=Memory::default();if length>0{memory.add(ADDRESS,&mut bytes,writable,false).unwrap();}
 let mut c=seed(0x37f,3,Some(raw(0x8000000000000001,0x3fff)));c.memory=(&mut memory as *mut Memory<'_>)as u64;let b=c.clone();unsafe{fstp80(&mut c)};assert_eq!((c.fault,c.rip),(2,b.rip));assert_eq!(c.x87,b.x87);assert_eq!(c.x87_status,b.x87_status);unaffected(&c,&b);drop(memory);assert_eq!(bytes,vec![0xa5;length]);count+=1;}}
 for t in 0..8{for masked in [false,true]{let mut bytes=[0xa5;10];let mut memory=Memory::default();memory.add(ADDRESS,&mut bytes,true,false).unwrap();let mut c=seed(if masked{0x37f}else{0x37e},t,None);c.memory=(&mut memory as *mut Memory<'_>)as u64;let b=c.clone();unsafe{fstp80(&mut c)};
 if masked{assert_eq!(c.fault,0);assert_eq!(top(c.x87_status),(t+1)&7);assert_eq!(c.x87_status&0x2c1,0x41);}else{assert_eq!((c.fault,c.rip),(18,b.rip));assert_eq!(c.x87,b.x87);assert_eq!(top(c.x87_status),t);assert_eq!(c.x87_status&0x82c1,0x80c1);}unaffected(&c,&b);drop(memory);assert_eq!(bytes,if masked{INDEFINITE}else{[0xa5;10]});count+=1;}}
 // Waiting instruction detects pending unmasked exceptions without store/pop.
 for exception in 0..6{let mut bytes=[0xa5;10];let mut memory=Memory::default();memory.add(ADDRESS,&mut bytes,true,false).unwrap();let mut c=seed(0x37f,6,Some(raw(1<<63,0x3fff)));c.x87_control&=!(1<<exception);c.x87_status|=1<<exception;c.memory=(&mut memory as *mut Memory<'_>)as u64;let b=c.clone();unsafe{fstp80(&mut c)};assert_eq!((c.fault,c.rip),(18,b.rip));assert_eq!(c.x87,b.x87);assert_eq!(c.x87_status,b.x87_status);unaffected(&c,&b);drop(memory);assert_eq!(bytes,[0xa5;10]);count+=1;}
 println!("PASS: {count} native exact FSTP m80 checks: all TOP/PC/RC, raw extended precision/range, signed zero/inf/qNaN, unaligned guarded stores, empty and pending exceptions; special operand audit remains separate");
}
"#;
