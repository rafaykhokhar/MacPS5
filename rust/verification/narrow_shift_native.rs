//! Native SHL/SHR/SAR/ROL/ROR verification, AMD APM Vol3 rev3.31.
//! https://kib.kiev.ua/x86docs/AMD/AMD64/24594_APM_v3-r3.31.pdf
//! Independent repeated single-bit arithmetic reference; undefined flag values
//! are checked only as the translator's documented deterministic policy.
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
    for width in [8usize, 16, 32, 64] {
        for (operation, mnemonic, extension) in [
            (0, "Shl", 4),
            (1, "Shr", 5),
            (2, "Sar", 7),
            (3, "Rol", 0),
            (4, "Ror", 1),
        ] {
            let mut forms = vec![(0, 256), (4, 256)];
            if width == 8 {
                forms.extend([(1, 256), (2, 256), (3, 256), (5, 256)]);
            }
            if width <= 16 {
                for count in [0, 1, 5, 7, 8, 15, 16, 31, 32, 63, 255, 257] {
                    forms.extend([(0, count), (4, count)]);
                }
            }
            for (mode, count) in forms {
                let mut bytes = Vec::new();
                if width == 16 {
                    bytes.push(0x66);
                }
                if width == 64 {
                    bytes.push(0x48);
                } else if mode == 1 || mode == 5 {
                    bytes.push(0x40);
                }
                bytes.push(match (width == 8, count) {
                    (true, 256) => 0xd2,
                    (false, 256) => 0xd3,
                    (true, 257) => 0xd0,
                    (false, 257) => 0xd1,
                    (true, _) => 0xc0,
                    (false, _) => 0xc1,
                });
                let operand = match mode {
                    0 => 0xc0,
                    1 => 0xc7,
                    2 => 0xc4,
                    3 => 0xc1,
                    4 => 0x07,
                    5 => 0xc6,
                    _ => unreachable!(),
                };
                bytes.push(operand | extension << 3);
                if count < 256 {
                    bytes.push(count as u8);
                }
                let decoded = native_ps5::decode::instruction(&bytes, 0x100)?;
                let register = match (mode, width) {
                    (0, 8) => "AL",
                    (0, 16) => "AX",
                    (0, 32) => "EAX",
                    (0, 64) => "RAX",
                    (1, _) => "DIL",
                    (2, _) => "AH",
                    (3, _) => "CL",
                    (4, _) => "None",
                    (5, _) => "SIL",
                    _ => unreachable!(),
                };
                if format!("{:?}", decoded.mnemonic()) != mnemonic
                    || decoded.len() != bytes.len()
                    || decoded.op_count() != 2
                    || format!("{:?}", decoded.op0_kind())
                        != if mode == 4 { "Memory" } else { "Register" }
                    || (mode != 4
                        && (format!("{:?}", decoded.op0_register()) != register
                            || decoded.op0_register().size() * 8 != width))
                    || (mode == 4
                        && (decoded.memory_size().size() * 8 != width
                            || format!("{:?}", decoded.memory_base()) != "RDI"))
                    || (count == 256
                        && (format!("{:?}", decoded.op1_kind()) != "Register"
                            || format!("{:?}", decoded.op1_register()) != "CL"))
                    || (count != 256
                        && (format!("{:?}", decoded.op1_kind()) != "Immediate8"
                            || decoded.immediate8() != if count == 257 { 1 } else { count as u8 }))
                {
                    return Err(format!(
                        "Shift fixture encoding mismatch {bytes:02x?}: {decoded:?}"
                    ));
                }
                let name = format!("shift_{width}_{operation}_{mode}_{count}");
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                table.push_str(&format!(
                    "({name} as Block,{width},{operation},{mode},{count}),\n"
                ));
            }
        }
    }
    // Forms outside the active addressing profile must be explicit boundaries.
    for bytes in [
        &[0x67, 0xc0, 0x24, 0x25, 0, 0, 0, 0, 1][..], // absolute address32 SHL byte
        &[0x64, 0xd2, 0x27][..],                      // FS memory
        &[0xf0, 0xd2, 0x27][..],                      // LOCK shift
    ] {
        if native_ps5::aot::translate_rust_memory_region(bytes, 0x100, "unsupported_shift").is_ok()
        {
            return Err(format!("Unsupported shift accepted {bytes:02x?}"));
        }
    }
    let driver = DRIVER
        .replace("DECLARATIONS", &declarations)
        .replace("CASES", &table);
    let asm = directory.join("narrow-shift.S");
    let obj = directory.join("narrow-shift.o");
    let source = directory.join("narrow-shift-driver.rs");
    let binary = directory.join("narrow-shift-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(&source, driver).map_err(|e| e.to_string())?;
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
unsafe extern "C" {DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu {let mut c=Cpu{rip:0x100,fuel:10,flags:0xad7,mxcsr:0x3fa0,..Cpu::default()};c.registers=[0xfedcba9876543210;16];c.vectors=[[0xa5;32];16];c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.vectors,b.vectors);assert_eq!(c.x87,b.x87);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));}
fn reference(input:u64,raw:u32,width:u32,op:u32,old:u64)->(u64,u64) {
 let mask=if width==64 {u64::MAX}else{(1u64<<width)-1};let sign=1u64<<(width-1);
 let count=raw & if width==64 {63}else{31};let mut value=input&mask;let original=value;
 if count==0 {return (value,old);}
 let mut carry=false;
 for _ in 0..count {
  if op==0 || op==3 {carry=value&sign!=0;value=value.wrapping_mul(2)&mask;if op==3 && carry {value|=1;}}
  else {carry=value&1!=0;let high=value&sign;value/=2;if op==2 {value|=high;}if op==4 && carry {value|=sign;}}
 }
 let mut flags=(old&!1)|u64::from(carry);
 if op<3 {
  flags&=!0xc4;if value==0 {flags|=0x40;}if value&sign!=0 {flags|=0x80;}
  let mut parity=false;for bit in 0..8 {parity^=value&(1<<bit)!=0;}if !parity {flags|=4;}
 }
 if count==1 {
  let overflow=match op {0|3=>(value&sign!=0)!=carry,1=>original&sign!=0,2=>false,_=>(value&sign!=0)!=(value&(sign>>1)!=0)};
  flags=(flags&!0x800)|if overflow{0x800}else{0};
 }
 (value,flags)
}
fn main(){
 let cases:&[(Block,u32,u32,u32,u32)]=&[CASES];let mut checks=0;let mut random=0x173cfd2ea593b481u64;
 for &(block,width,op,mode,encoded) in cases {
  let counts:Vec<u32>=if encoded==256 {(0..256).collect()}else{vec![if encoded==257{1}else{encoded}]};
  for count in counts {
   for n in 0..16 {
    random^=random<<13;random^=random>>7;random^=random<<17;
    let value=match n {0=>0,1=>u64::MAX,2=>1,3=>1u64<<(width-1),4=>(1u64<<(width-1))-1,5=>0x55aa55aa55aa55aa,_=>random};
    for flags in [2,0xad7] {
     let mut c=seed();c.flags=flags;c.registers[1]=(c.registers[1]&!255)|u64::from(count);
     let mask=if width==64{u64::MAX}else{(1u64<<width)-1};
     if mode!=4 {
      let dst=match mode {1=>7,3=>1,5=>6,_=>0};let shift=if mode==2{8}else{0};
      if mode!=3 {c.registers[dst]=(c.registers[dst]&!(mask<<shift))|((value&mask)<<shift);}
      let b=c.clone();let input=(b.registers[dst]>>shift)&mask;
      let (out,expectedflags)=reference(input,count,width,op,flags);
      unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel),(0,9));assert_eq!(c.flags,expectedflags,"width{width} op{op} mode{mode} count{count}");
      let mut regs=b.registers;regs[dst]=if width==32 {out}else{(regs[dst]&!(mask<<shift))|(out<<shift)};
      assert_eq!(c.registers,regs,"width{width} op{op} mode{mode} count{count}");preserved(&c,&b);checks+=1;
     }else{
      let length=(width/8)as usize;let mut bytes=[0xa5;24];let offset=1;bytes[offset..offset+length].copy_from_slice(&value.to_le_bytes()[..length]);let original=bytes;
      let address=0x1_00300001;let mut memory=Memory::default();memory.add(address,&mut bytes,true,false).unwrap();
      c.registers[7]=address+offset as u64;c.memory=(&mut memory as *mut Memory<'_>)as u64;let b=c.clone();
      let(out,expectedflags)=reference(value,count,width,op,flags);unsafe{block(&mut c)};
      assert_eq!((c.fault,c.fuel),(0,9));assert_eq!(c.flags,expectedflags,"memory width{width} op{op} count{count}");assert_eq!(c.registers,b.registers);preserved(&c,&b);
      drop(memory);let mut expected=original;expected[offset..offset+length].copy_from_slice(&out.to_le_bytes()[..length]);assert_eq!(bytes,expected);checks+=1;
     }
    }
   }
  }
  if mode==4 {
   for count in [0u32,1,8,16,32,255] {
    for (size,writable,faultcode) in [(0usize,true,1),((width/8-1)as usize,true,1),((width/8)as usize,false,2)] {
     let mut bytes=vec![0xa5;size];let original=bytes.clone();let address=0x1_00300001;
     let mut memory=Memory::default();if size!=0{memory.add(address,&mut bytes,writable,false).unwrap();}
     let mut c=seed();c.registers[1]=u64::from(count);c.registers[7]=address;c.memory=(&mut memory as *mut Memory<'_>)as u64;let b=c.clone();
     unsafe{block(&mut c)};assert_eq!(c.fault,faultcode,"memory guard width{width} op{op} encoded{encoded} count{count}");assert_eq!(c.fuel,9);assert_eq!(c.rip,b.rip);assert_eq!(c.flags,b.flags);assert_eq!(c.registers,b.registers);preserved(&c,&b);drop(memory);assert_eq!(bytes,original);checks+=1;
    }
   }
  }
  let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));assert_eq!(c.flags,b.flags);assert_eq!(c.registers,b.registers);preserved(&c,&b);checks+=1;
 }
 println!("PASS: {checks} native narrow shift/rotate checks: all256 CL counts, immediate/one forms, AH/DIL/SIL/CL aliases, iterative reference, partial writes, defined flags and explicit undefined preservation policy, full64 unaligned memory, zero-count memory access, atomic load/store faults, FP/fuel, 32/64 regression");
}
"#;
