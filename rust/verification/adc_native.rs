//! ARM64 ADC checks against an independent widened-integer reference.
//! Intel SDM Vol.2A ADC: dest := dest + source + CF; six arithmetic flags.
//! https://cdrdv2-public.intel.com/812383/253666-sdm-vol-2a.pdf
use std::{path::Path, process::Command};
fn run(command: &mut Command) -> Result<(), String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    // kind0 reg/reg;1 memory source;2 memory destination;3 immediate;
    // 4 same-register;5 AH destination;6 AH alias;7 actual RDI immediate;
    // 8 actual CMP RDI,1 + ADC RDI,0 allocator pair.
    let cases: &[(&str, &[u8], usize, u8, u64)] = &[
        ("adc8", &[0x10, 0xd0], 8, 0, 0),
        ("adc16", &[0x66, 0x11, 0xd0], 16, 0, 0),
        ("adc32", &[0x11, 0xd0], 32, 0, 0),
        ("adc64", &[0x48, 0x11, 0xd0], 64, 0, 0),
        ("adc8load", &[0x12, 0x07], 8, 1, 0),
        ("adc16load", &[0x66, 0x13, 0x07], 16, 1, 0),
        ("adc32load", &[0x13, 0x07], 32, 1, 0),
        ("adc64load", &[0x48, 0x13, 0x07], 64, 1, 0),
        ("adc8store", &[0x10, 0x17], 8, 2, 0),
        ("adc16store", &[0x66, 0x11, 0x17], 16, 2, 0),
        ("adc32store", &[0x11, 0x17], 32, 2, 0),
        ("adc64store", &[0x48, 0x11, 0x17], 64, 2, 0),
        ("adc8imm", &[0x14, 0x80], 8, 3, 0x80),
        ("adc16imm", &[0x66, 0x15, 0, 0x80], 16, 3, 0x8000),
        ("adc32imm", &[0x15, 0, 0, 0, 0x80], 32, 3, 0x80000000),
        (
            "adc64imm",
            &[0x48, 0x15, 0, 0, 0, 0x80],
            64,
            3,
            0xffffffff80000000,
        ),
        ("adc16imm8", &[0x66, 0x83, 0xd0, 0xff], 16, 3, u64::MAX),
        ("adc32imm8", &[0x83, 0xd0, 0xff], 32, 3, u64::MAX),
        ("adc64imm8", &[0x48, 0x83, 0xd0, 0xff], 64, 3, u64::MAX),
        ("adc8alias", &[0x10, 0xc0], 8, 4, 0),
        ("adc16alias", &[0x66, 0x11, 0xc0], 16, 4, 0),
        ("adc32alias", &[0x11, 0xc0], 32, 4, 0),
        ("adc64alias", &[0x48, 0x11, 0xc0], 64, 4, 0),
        ("adcah", &[0x10, 0xd4], 8, 5, 0),
        ("adcahalias", &[0x10, 0xe4], 8, 6, 0),
        ("adcalloc", &[0x48, 0x83, 0xd7, 0], 64, 7, 0),
        (
            "adcallocpair",
            &[0x48, 0x83, 0xff, 1, 0x48, 0x83, 0xd7, 0],
            64,
            8,
            0,
        ),
    ];
    let mut assembly = String::new();
    let mut declarations = String::new();
    let mut entries = String::new();
    for &(name, bytes, width, kind, immediate) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0x18d0a, name,
        )?);
        declarations.push_str(&format!("fn {name}(cpu:*mut Cpu);"));
        entries.push_str(&format!("({width},{kind},{immediate}u64,{name} as Block),"));
    }
    assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
        &[0x48, 0x11, 0xd0, 0x48, 0x11, 0xcb],
        0x18d0a,
        "adcchain",
    )?);
    declarations.push_str("fn adcchain(cpu:*mut Cpu);");
    let asm = directory.join("adc.S");
    let obj = directory.join("adc.o");
    let source = directory.join("adc-driver.rs");
    let binary = directory.join("adc-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(
        &source,
        DRIVER
            .replace("DECLARATIONS", &declarations)
            .replace("CASES", &entries),
    )
    .map_err(|e| e.to_string())?;
    run(Command::new("xcrun")
        .args(["clang", "-arch", "arm64", "-c"])
        .arg(&asm)
        .arg("-o")
        .arg(&obj))?;
    let profile = archive.parent().ok_or("Missing archive directory")?;
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
unsafe extern "C" { DECLARATIONS }
fn reference(a:u64,b:u64,old:u64,width:usize)->(u64,u64){
 let modulus=1u128<<width;let mask=(modulus-1)as u64;let a=a&mask;let b=b&mask;let carry=old&1;
 let sum=a as u128+b as u128+carry as u128;let value=(sum%modulus)as u64;
 let signed=|v:u64| if v as u128>=modulus/2{v as i128-modulus as i128}else{v as i128};
 let exact=signed(a)+signed(b)+carry as i128;
 let mut flags=(old&!0x8d5)|2;
 if sum>=modulus{flags|=1;}if (a&15)+(b&15)+carry>=16{flags|=16;}
 if value==0{flags|=64;}if value as u128>=modulus/2{flags|=128;}
 if (value as u8).count_ones()%2==0{flags|=4;}
 if exact < -(modulus as i128/2)||exact>=modulus as i128/2{flags|=0x800;}
 (value,flags)
}
fn main(){
 let cases:&[(usize,u8,u64,Block)]=&[CASES]; let mut count=0;let mut seed=0xab396d10c459e6f7u64;
 for &(width,kind,imm,block) in cases{
  let mask=u64::MAX>>(64-width);let sign=1u64<<(width-1);let edges=[0,1,15,16,sign-1,sign,sign+1,mask];
  let mut inputs=Vec::new();for a in edges{for b in edges{inputs.push((a,b));}}
  for _ in 0..1024{seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;let a=seed;seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;inputs.push((a,seed));}
  for (a,b) in inputs { for carry in [0,1] {
   let a=a&mask;let b=b&mask;let mut bytes=if kind==2{a.to_le_bytes()}else{b.to_le_bytes()};
   let mut memory=Memory::default();memory.add(0x300000,&mut bytes,true,false).unwrap();
   let mut cpu=Cpu{rip:0x18d0a,fuel:20,flags:0xa5a50ed6|carry,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};
   cpu.registers=[0x5a5a5a5a5a5a5a5a;16];cpu.registers[0]=(0xa55aa55aa55aa55a&!mask)|a;cpu.registers[2]=b;cpu.registers[7]=0x300000;
   if kind==5||kind==6{cpu.registers[0]=0xa55aa55aa55a005a|(a<<8);}
   if kind>=7{cpu.registers[7]=a;}
   let before=cpu.clone();let input=if kind==3{imm}else if kind==4||kind==6{a}else if kind>=7{0}else{b};
   let prior=if kind==8{(cpu.flags&!1)|u64::from(a<1)}else{cpu.flags};
   let (value,flags)=reference(a,input,prior,width);let mut expected=before.registers;
   if kind!=2{let index=if kind>=7{7}else{0};expected[index]=if kind==5||kind==6{(expected[index]&!0xff00)|(value<<8)}else if width==8||width==16{(expected[index]&!mask)|value}else{value};}
   unsafe{block(&mut cpu)};assert_eq!(cpu.fault,0,"width{width} kind{kind}");assert_eq!(cpu.registers,expected,"width{width} kind{kind} a{a:x} b{input:x} carry{carry}");assert_eq!(cpu.flags,flags,"flags width{width} kind{kind} a{a:x} b{input:x} carry{carry}");
   assert_eq!(cpu.fuel,20-if kind==8{2}else{1});assert_eq!(cpu.vectors,before.vectors);assert_eq!((cpu.mxcsr,cpu.x87_control,cpu.x87_status),(before.mxcsr,before.x87_control,before.x87_status));
   if kind==2{assert_eq!(memory.load(0x300000,(width/8) as u64).unwrap(),value);}count+=1;
  }}
  if kind==1||kind==2 { for writable in [false,true] {
   let len=if writable{width/8-1}else{width/8};let mut bytes=vec![0x55;len];let old=bytes.clone();let mut memory=Memory::default();if len!=0{memory.add(0x300000,&mut bytes,writable,false).unwrap();}
   let mut cpu=Cpu{rip:0x18d0a,fuel:20,flags:0xed7,memory:(&mut memory as *mut Memory<'_>)as u64,..Cpu::default()};cpu.registers[7]=0x300000;let before=cpu.clone();
   unsafe{block(&mut cpu)};if writable||kind==2{assert_eq!(cpu.fault,if !writable&&kind==2{2}else{1});assert_eq!(cpu.registers,before.registers);assert_eq!(cpu.flags,before.flags);assert_eq!(cpu.rip,before.rip);}else{assert_eq!(cpu.fault,0);}drop(memory);assert_eq!(bytes,old);
  }}
 }
 for a in [0,1,u64::MAX,1u64<<63]{for b in [0,1,u64::MAX,1u64<<63]{for carry in [0,1]{
  let mut cpu=Cpu{rip:0x18d0a,fuel:20,flags:0xed6|carry,..Cpu::default()};cpu.registers[0]=a;cpu.registers[2]=b;cpu.registers[3]=b;cpu.registers[1]=a;
  let (lo,f)=reference(a,b,cpu.flags,64);let (hi,f)=reference(b,a,f,64);unsafe{adcchain(&mut cpu)};assert_eq!((cpu.registers[0],cpu.registers[3],cpu.flags,cpu.fault),(lo,hi,f,0));assert_eq!(cpu.fuel,18);
 }}}
 println!("PASS: ARM64 ADC {count} independent edge/random cases,8/16/32/64 register/memory/immediates,CF0/1,six flags,AH/aliases,carry chain,actual SDL allocator pair and guard atomicity");
}
"#;
