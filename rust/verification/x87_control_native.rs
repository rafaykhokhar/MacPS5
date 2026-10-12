//! Native control-word transport only, no x87 arithmetic or guest trap delivery.
use std::{path::Path, process::Command};
fn run(command: &mut Command) -> Result<(), String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut assembly = String::new();
    for (name, bytes) in [
        ("load_cw", &[0xd9, 0x2f][..]),
        ("store_cw", &[0xd9, 0x3f]),
        ("waiting_store", &[0x9b, 0xd9, 0x3f]),
        ("wait_x87", &[0x9b]),
        ("load_simd", &[0x0f, 0xae, 0x17]),
    ] {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0xc3f2e, name,
        )?);
    }
    let asm = directory.join("controls.S");
    let obj = directory.join("controls.o");
    let source = directory.join("driver.rs");
    let binary = directory.join("driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(&source, DRIVER).map_err(|e| e.to_string())?;
    run(Command::new("xcrun")
        .args(["clang", "-arch", "arm64", "-c"])
        .arg(&asm)
        .arg("-o")
        .arg(&obj))?;
    let profile = archive.parent().ok_or("Archive parent missing")?;
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
    run(&mut Command::new(binary))?;
    inheritance(archive, &directory.join("inheritance"))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C" {fn load_cw(c:*mut Cpu);fn store_cw(c:*mut Cpu);fn waiting_store(c:*mut Cpu);fn wait_x87(c:*mut Cpu);fn load_simd(c:*mut Cpu);}
fn main(){
 for pc in [0u16,0x200,0x300]{for rc in 0..4{for ic in [0u16,0x1000]{
  let value=0x7f|pc|(rc<<10)|ic;let mut data=value.to_le_bytes();let mut memory=Memory::default();memory.add(0x300000,&mut data,true,false).unwrap();
  let mut cpu=Cpu{rip:0xc3f2e,fuel:20,mxcsr:0xa040,x87_status:0x20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;unsafe{load_cw(&mut cpu)};assert_eq!(cpu.fault,0);assert_eq!(cpu.x87_control,value);assert_eq!(cpu.x87_status,0x20);assert_eq!(cpu.mxcsr,0xa040);
  cpu.rip=0xc3f2e;unsafe{store_cw(&mut cpu)};assert_eq!(memory.load(0x300000,2).unwrap(),value as u64);
 }}}
 for length in [1,2]{for writable in [false,true]{
  let mut data=vec![0x5a;length];let mut memory=Memory::default();memory.add(0x300000,&mut data,writable,false).unwrap();
  let mut cpu=Cpu{rip:0xc3f2e,fuel:20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;unsafe{store_cw(&mut cpu)};
  assert_eq!(cpu.fault,if length==2&&writable{0}else{2});if cpu.fault!=0{assert_eq!(memory.read(0x300000,length).unwrap(),vec![0x5a;length]);}
 }}
 let mut data=[0x5a;4];let mut memory=Memory::default();memory.add(0x300000,&mut data,true,false).unwrap();
 for pending in [false,true]{
  let mut cpu=Cpu{rip:0xc3f2e,fuel:20,x87_control:0x037e,x87_status:u16::from(pending),memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;
  unsafe{store_cw(&mut cpu)};assert_eq!(cpu.fault,0); // no-wait stores despite pending
  cpu.rip=0xc3f2e;unsafe{waiting_store(&mut cpu)};assert_eq!(cpu.fault,if pending{18}else{0});
 }
 let mut cpu=Cpu{rip:0xc3f2e,fuel:20,x87_control:0x037e,x87_status:1,..Cpu::default()};unsafe{wait_x87(&mut cpu)};assert_eq!(cpu.fault,18);
 for value in [0u16,0x013f,0xffff,0x03ff]{memory.store(0x300000,value as u64,2).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;unsafe{load_cw(&mut cpu)};assert_eq!(cpu.fault,19);assert_eq!(cpu.x87_control,0x037f);}
 let mut partial=[0x7fu8];let mut short=Memory::default();short.add(0x400000,&mut partial,false,false).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,memory:(&mut short as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x400000;unsafe{load_cw(&mut cpu)};assert_eq!(cpu.fault,1);assert_eq!(cpu.x87_control,0x037f);
 // An existing pending exception wins before FLDCW's malformed memory input.
 let mut cpu=Cpu{rip:0xc3f2e,fuel:20,x87_control:0x037e,x87_status:1,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=u64::MAX;unsafe{load_cw(&mut cpu)};assert_eq!(cpu.fault,18);assert_eq!(cpu.x87_control,0x037e);
 memory.store(0x300000,0x1f80,4).unwrap();let mut cpu=Cpu{rip:0xc3f2e,fuel:20,x87_control:0x0b7f,x87_status:0x20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;unsafe{load_simd(&mut cpu)};assert_eq!((cpu.x87_control,cpu.x87_status),(0x0b7f,0x20));assert_eq!(cpu.mxcsr,0x1f80);
 println!("PASS: native x87 control transport PC/RC/IC, no-wait/wait pending boundary, memory guard atomicity and independent MXCSR; no arithmetic/delivery claim.");
}
"#;

fn inheritance(archive: &Path, directory: &Path) -> Result<(), String> {
    use native_ps5::{
        guest_memory::DataRegion,
        loader::Image,
        module_executor::{NativeExecutor, Service, memory_fixture},
        scheduler::{Scheduler, Step},
    };
    use std::collections::BTreeSet;
    const DATA: u64 = 0x31000000;
    fn control(code: &mut Vec<u8>, offset: u64, store: bool) {
        code.extend([0x48, 0xbf]);
        code.extend((DATA + offset).to_le_bytes());
        code.extend([0xd9, if store { 0x3f } else { 0x2f }]);
    }
    fn call(code: &mut Vec<u8>, address: u64) {
        code.extend([0x48, 0xb8]);
        code.extend(address.to_le_bytes());
        code.extend([0xff, 0xd0]);
    }
    let mut file = memory_fixture();
    file[96..104].copy_from_slice(&0x300u64.to_le_bytes());
    file[104..112].copy_from_slice(&0x300u64.to_le_bytes());
    let mut parent = vec![0x48, 0x83, 0xec, 16];
    control(&mut parent, 0, false);
    parent.extend([0x48, 0x89, 0xe7, 0x31, 0xf6, 0x48, 0xba]);
    parent.extend(0x100200u64.to_le_bytes());
    parent.extend([0x31, 0xc9]);
    call(&mut parent, 0x780010);
    parent.extend([0x48, 0x8b, 0x3c, 0x24, 0x31, 0xf6]);
    call(&mut parent, 0x780020);
    control(&mut parent, 8, true);
    parent.extend([0x31, 0xc0, 0x48, 0x83, 0xc4, 16, 0xc3]);
    let mut child = Vec::new();
    control(&mut child, 4, true);
    control(&mut child, 2, false);
    control(&mut child, 6, true);
    child.extend([0x31, 0xc0, 0xc3]);
    assert!(parent.len() <= 0x100 && child.len() <= 0x100);
    file[0x100..0x100 + parent.len()].copy_from_slice(&parent);
    file[0x200..0x200 + child.len()].copy_from_slice(&child);
    let mut native = NativeExecutor::new(0x10000000, 2 * 1024 * 1024, 10000, 10000)?;
    native.register_service(0x780010, Service::ThreadCreate)?;
    native.register_service(0x780020, Service::ThreadJoin)?;
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            directory,
        )?;
        native.set_automatic_translation(true);
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut scheduler = Scheduler::new(2, 1000, 10000)?;
    scheduler.enqueue(&mut native, 2, 0x100100, 0)?;
    let mut data = [0u8; 10];
    data[..2].copy_from_slice(&0x0b7fu16.to_le_bytes());
    data[2..4].copy_from_slice(&0x077fu16.to_le_bytes());
    let mut finished = false;
    for _ in 0..1000 {
        if matches!(
            scheduler.step(
                &mut native,
                &mut image,
                &mut [],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true
                }]
            )?,
            Step::Finished
        ) {
            finished = true;
            break;
        }
    }
    assert!(finished);
    assert_eq!(scheduler.result(2), Some(0));
    assert_eq!(&data[4..6], &data[..2]);
    assert_eq!(&data[6..8], &data[2..4]);
    assert_eq!(&data[8..10], &data[..2]);
    println!(
        "PASS: actual guest pthread_create inherits x87 control; child FLDCW remains isolated after join."
    );
    Ok(())
}
