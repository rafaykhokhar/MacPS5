//! Independent byte-copy oracle for all four VEX VMOVQ forms and reverse VMOVD.
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
    for variant in 0..7 {
        let family = if variant < 5 { variant } else { variant - 3 };
        for mode in 0..6 {
            let vector = if mode == 1 || mode == 4 { 15 } else { 2 };
            let operand = match mode {
                0 => 1,
                1 | 4 => 15,
                2 => 2,
                3 => 7,
                5 => 5,
                _ => unreachable!(),
            };
            let memory = mode >= 3;
            let reverse = family == 1 || family == 3 || family == 4;
            let width = if family == 4 { 4 } else { 8 };
            let (opcode, pp, mut w, code) = match family {
                0 => (0x6e, 1, 0x80, "VEX_Vmovq_xmm_rm64"),
                1 => (0x7e, 1, 0x80, "VEX_Vmovq_rm64_xmm"),
                2 => (0x7e, 2, 0, "VEX_Vmovq_xmm_xmmm64"),
                3 => (0xd6, 1, 0, "VEX_Vmovq_xmmm64_xmm"),
                4 => (0x7e, 1, 0, "VEX_Vmovd_rm32_xmm"),
                _ => unreachable!(),
            };
            if variant >= 5 {
                w = 0x80;
            }
            let mut bytes = vec![
                0xc4,
                0x41 | if vector < 8 { 0x80 } else { 0 } | if operand < 8 { 0x20 } else { 0 },
                0x78 | pp | w,
                opcode,
                if memory { 0 } else { 0xc0 } | ((vector & 7) << 3) | (operand & 7),
            ];
            if mode == 5 {
                bytes.extend_from_slice(&32u32.to_le_bytes());
            }
            let d = native_ps5::decode::instruction(&bytes, 0x100)?;
            let vop = if reverse { 1 } else { 0 };
            let other = 1 - vop;
            let operand_name = if memory {
                String::new()
            } else if family == 2 || family == 3 {
                format!("XMM{operand}")
            } else if width == 4 {
                match operand {
                    1 => "ECX".into(),
                    2 => "EDX".into(),
                    15 => "R15D".into(),
                    _ => unreachable!(),
                }
            } else {
                match operand {
                    1 => "RCX".into(),
                    2 => "RDX".into(),
                    15 => "R15".into(),
                    _ => {
                        if memory {
                            String::new()
                        } else {
                            unreachable!()
                        }
                    }
                }
            };
            if format!("{:?}", d.code()) != code
                || d.len() != bytes.len()
                || d.op_count() != 2
                || format!("{:?}", d.op_kind(vop)) != "Register"
                || format!("{:?}", d.op_register(vop)) != format!("XMM{vector}")
                || format!("{:?}", d.op_kind(other)) != if memory { "Memory" } else { "Register" }
                || (!memory && format!("{:?}", d.op_register(other)) != operand_name)
                || (memory
                    && (d.memory_size().size() != width
                        || format!("{:?}", d.memory_base())
                            != match mode {
                                3 => "RDI",
                                4 => "R15",
                                5 => "RIP",
                                _ => unreachable!(),
                            }
                        || format!("{:?}", d.memory_index()) != "None"))
                || (mode == 5 && d.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
            {
                return Err(format!("Bad VMOV fixture {family}/{mode}: {d:?}"));
            }
            let mut invalid = bytes.clone();
            invalid[2] |= 4;
            if native_ps5::aot::translate_rust_memory_region(&invalid, 0x100, "invalid_vmov_l")
                .is_ok()
            {
                return Err(format!("Reserved VEX L=1 accepted for {variant}/{mode}"));
            }
            let name = format!("vmovq_{variant}_{mode}");
            assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                &bytes, 0x100, &name,
            )?);
            declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
            let direction = if memory {
                if reverse { 4 } else { 3 }
            } else {
                match family {
                    0 => 0,
                    1 | 4 => 1,
                    2 | 3 => 2,
                    _ => unreachable!(),
                }
            };
            let (dst, src) = if reverse {
                (operand, vector)
            } else {
                (vector, operand)
            };
            table.push_str(&format!(
                "({name},{direction},{dst},{src},{width},{mode},{}),\n",
                bytes.len()
            ));
        }
    }
    let asm = directory.join("vmovq.S");
    let obj = directory.join("vmovq.o");
    let source = directory.join("vmovq-driver.rs");
    let binary = directory.join("vmovq-driver");
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
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C" { DECLARATIONS }
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed(value:u64)->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};for(i,r)in c.registers.iter_mut().enumerate(){*r=value.rotate_left(i as u32*3);}for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(value.rotate_right((i%8*8)as u32)as u8).wrapping_add((r*37+i*11)as u8);}}c}
fn controls(c:&Cpu,b:&Cpu){assert_eq!(c.flags,b.flags);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.vectors,b.vectors);controls(c,b);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==5{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==4{15}else{7}]=addr;}}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;let mut random=0x8746abcde3456123u64;
for &(block,direction,dst,src,width,mode,len) in cases{
 for n in 0..2048{random^=random<<13;random^=random>>7;random^=random<<17;let value=match n{0=>0,1=>u64::MAX,2=>0x8000000000000000,3=>0xffffffff00000000,_=>random};let mut c=seed(value);
  let mut bytes:Vec<u8>=(0..64).map(|i|value.rotate_right((i%8*8)as u32)as u8).collect();let before=bytes.clone();let offset=[0usize,1,3,7,15,56][n%6];let addr=0x1_00300001;
  let mut memory=Memory::default();memory.add(addr,&mut bytes,direction==4,false).unwrap();c.memory=(&mut memory as *mut Memory<'_>)as u64;if direction>=3{address(&mut c,mode,len,addr+offset as u64);}let b=c.clone();let mut regs=b.registers;let mut vectors=b.vectors;let mut expected=before.clone();
  match direction{
   0=>{vectors[dst]=[0;32];vectors[dst][..8].copy_from_slice(&b.registers[src].to_le_bytes());},
   1=>{let mut v=[0;8];v[..width].copy_from_slice(&b.vectors[src][..width]);regs[dst]=u64::from_le_bytes(v);},
   2=>{vectors[dst]=[0;32];vectors[dst][..8].copy_from_slice(&b.vectors[src][..8]);},
   3=>{vectors[dst]=[0;32];vectors[dst][..width].copy_from_slice(&before[offset..offset+width]);},
   4=>expected[offset..offset+width].copy_from_slice(&b.vectors[src][..width]),_=>unreachable!()}
  unsafe{block(&mut c)};assert_eq!(c.fault,0,"direction {direction}, mode {mode}");assert_eq!(c.registers,regs);assert_eq!(c.vectors,vectors);assert_eq!(c.fuel,19);controls(&c,&b);drop(memory);assert_eq!(bytes,expected);checks+=1;
 }
 if direction>=3{for length in 0..width{let mut bytes=vec![0xaa;length];let before=bytes.clone();let mut memory=Memory::default();let addr=0x1_00300001;if length>0{memory.add(addr,&mut bytes,true,false).unwrap();}let mut c=seed(0xfedcba9876543210);c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(if direction==3{1}else{2},19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
  if direction==4{let mut bytes=vec![0xaa;32];let before=bytes.clone();let mut memory=Memory::default();let addr=0x1_00300001;memory.add(addr,&mut bytes,false,false).unwrap();let mut c=seed(0xfedcba9876543210);c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,addr+1);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(2,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(bytes,before);checks+=1;}
 }
 let mut c=seed(0xfedcba9876543210);c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed(0xfedcba9876543210);c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VMOVQ/reverse VMOVD checks across 42 decoded forms including WIG=1: independent byte copies, all directions, high registers, aliases, vector upper zeroing, GPR zero extension, high unaligned/RIP memory, short spans/read-only stores, atomic faults, flags/FP/fuel/preexisting faults");}
"#;
