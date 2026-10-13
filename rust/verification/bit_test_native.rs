//! Independent native bit-test reference: signed bit-string addressing and
//! bit mutation use Rust integer operations, not translated helper results.
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
    let mut cases = String::new();
    for width in [32, 64] {
        for (operation, mnemonic, opcode, extension) in
            [(0, "Bt", 0xa3, 4), (1, "Bts", 0xab, 5), (2, "Btr", 0xb3, 6)]
        {
            for mode in [0, 1, 2, 3, 4, 7, 8] {
                let mut bytes = Vec::new();
                if width == 64 {
                    bytes.push(0x48);
                }
                bytes.push(0x0f);
                if mode == 2 || mode == 4 || mode == 8 {
                    bytes.extend_from_slice(&[
                        0xba,
                        (if mode == 2 {
                            0xc0
                        } else if mode == 8 {
                            5
                        } else {
                            7
                        }) | (extension << 3),
                    ]);
                    if mode == 8 {
                        bytes.extend_from_slice(&32u32.to_le_bytes());
                    }
                    bytes.push(255);
                } else {
                    bytes.extend_from_slice(&[
                        opcode,
                        match mode {
                            0 => 0xc8,
                            1 => 0xc0,
                            3 => 0x0f,
                            7 => 0x0d,
                            _ => unreachable!(),
                        },
                    ]);
                    if mode == 7 {
                        bytes.extend_from_slice(&32u32.to_le_bytes());
                    }
                }
                let decoded = native_ps5::decode::instruction(&bytes, 0x100)?;
                if format!("{:?}", decoded.mnemonic()) != mnemonic || decoded.len() != bytes.len() {
                    return Err(format!(
                        "Bit-test fixture encoding mismatch: {bytes:02x?} -> {decoded:?}"
                    ));
                }
                let register = mode < 3;
                let immediate = matches!(mode, 2 | 4 | 8);
                let expected_rax = if width == 32 { "EAX" } else { "RAX" };
                let expected_index = if mode == 1 {
                    expected_rax
                } else if width == 32 {
                    "ECX"
                } else {
                    "RCX"
                };
                if decoded.op_count() != 2
                    || format!("{:?}", decoded.op0_kind())
                        != if register { "Register" } else { "Memory" }
                    || format!("{:?}", decoded.op1_kind())
                        != if immediate { "Immediate8" } else { "Register" }
                    || (register
                        && (format!("{:?}", decoded.op0_register()) != expected_rax
                            || decoded.op0_register().size() * 8 != width))
                    || (!immediate
                        && (format!("{:?}", decoded.op1_register()) != expected_index
                            || decoded.op1_register().size() * 8 != width))
                    || (immediate && decoded.immediate8() != 255)
                    || (!register
                        && (decoded.memory_size().size() * 8 != width
                            || format!("{:?}", decoded.memory_base())
                                != if mode >= 7 { "RIP" } else { "RDI" }
                            || format!("{:?}", decoded.memory_index()) != "None"))
                    || (mode >= 7
                        && decoded.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
                {
                    return Err(format!(
                        "Bit-test operand fixture mismatch: {bytes:02x?} -> {decoded:?}"
                    ));
                }
                let name = format!("bit_{operation}_{width}_{mode}");
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(cpu:*mut Cpu);\n"));
                cases.push_str(&format!("({name} as Block,{width},{operation},{mode}),\n"));
            }
        }
    }
    // Exact encodings recorded in CPU_INVENTORY; this verifies their semantics,
    // not execution of the surrounding payload or provenance of an ELF file.
    for (name, bytes, operation, mode) in [
        ("bit_observed_bt", &[0x44, 0x0f, 0xa3, 0xf8][..], 0, 5),
        ("bit_observed_btr", &[0x0f, 0xb3, 0xc1][..], 2, 6),
    ] {
        let decoded = native_ps5::decode::instruction(bytes, 0x100)?;
        if decoded.len() != bytes.len()
            || format!("{:?}", decoded.mnemonic()) != if operation == 0 { "Bt" } else { "Btr" }
        {
            return Err("Observed bit-test fixture encoding mismatch".into());
        }
        if decoded.op_count() != 2
            || format!("{:?}", decoded.op0_kind()) != "Register"
            || format!("{:?}", decoded.op1_kind()) != "Register"
            || decoded.op0_register().size() != 4
            || decoded.op1_register().size() != 4
            || format!("{:?}", decoded.op0_register()) != if mode == 5 { "EAX" } else { "ECX" }
            || format!("{:?}", decoded.op1_register()) != if mode == 5 { "R15D" } else { "EAX" }
        {
            return Err("Observed bit-test operand mismatch".into());
        }
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0x100, name,
        )?);
        declarations.push_str(&format!("fn {name}(cpu:*mut Cpu);\n"));
        cases.push_str(&format!("({name} as Block,32,{operation},{mode}),\n"));
    }
    let driver = DRIVER
        .replace("DECLARATIONS", &declarations)
        .replace("CASES", &cases);
    let asm = directory.join("bit-test.S");
    let obj = directory.join("bit-test.o");
    let source = directory.join("bit-test-driver.rs");
    let binary = directory.join("bit-test-driver");
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
use native_ps5::{legacy_cpu::Cpu, guest_memory::Memory};
unsafe extern "C" { DECLARATIONS }
type Block = unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu {
    let mut c=Cpu{rip:0x100,fuel:10,flags:0xad7,mxcsr:0x3fa0,..Cpu::default()};
    c.registers=[0xfedcba9876543210;16]; c.vectors=[[0xa5;32];16]; c
}
fn preserve(c:&Cpu,b:&Cpu) {
    assert_eq!(c.vectors,b.vectors); assert_eq!(c.x87,b.x87);
    assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));
}
fn reference(value:u64,index:u64,width:u32,operation:u32)->(u64,u64) {
    let bit=index % u64::from(width); let mask=1u64<<bit;
    let carry=u64::from(value&mask!=0);
    let output=match operation {0=>value,1=>value|mask,2=>value&!mask,_=>unreachable!()};
    (if width==32 && operation!=0 {output & 0xffff_ffff} else {output},carry)
}
fn rip_address(width:u32,mode:u32)->u64 {0x100+32+if mode==7 {7} else {8}+u64::from(width==64)}
fn success(c:&Cpu,b:&Cpu,carry:u64) {
    assert_eq!(c.fault,0); assert_eq!(c.fuel,9);
    // AMD marks OF/SF/ZF/AF/PF undefined. Their preservation tests the
    // project's deterministic policy, not a hardware equivalence claim.
    assert_eq!(c.flags,(b.flags&!1)|carry); preserve(c,b);
}
fn fault(c:&Cpu,b:&Cpu,code:u64) {
    assert_eq!(c.fault,code); assert_eq!(c.fuel,9); assert_eq!(c.rip,b.rip);
    assert_eq!(c.registers,b.registers); assert_eq!(c.flags,b.flags); preserve(c,b);
}
fn main() {
    let cases:&[(Block,u32,u32,u32)]=&[CASES];
    let mut checks=0usize; let mut random=0x6a21e98745bcd30fu64;
    for &(block,width,operation,mode) in cases {
        if mode<3 || mode==5 || mode==6 {
            for n in 0..1024 {
                random^=random<<13;random^=random>>7;random^=random<<17;
                let value=match n {0=>0,1=>u64::MAX,2=>0xffff_ffff_0000_0000,3..=66=>1u64<<(n-3),_=>random};
                let index=match n {0=>0,1=>u64::MAX,_=>random.rotate_left(17)};
                for flags in [2,0xad7] {
                    let dst=if mode==6 {1} else {0};let src=if mode==5 {15} else if mode==6 {0} else {1};
                    let mut c=seed();c.flags=flags;c.registers[dst]=value;c.registers[src]=index;
                    let b=c.clone();let actual_index=match mode {1=>value,2=>255,_=>index};
                    let (output,carry)=reference(value,actual_index,width,operation);
                    unsafe{block(&mut c)};success(&c,&b,carry);
                    let mut expected=b.registers;expected[dst]=output;assert_eq!(c.registers,expected);checks+=1;
                }
            }
        } else {
            let width_bytes=(width/8) as usize;
            for index in [-129i64,-65,-64,-33,-32,-1,0,1,31,32,63,64,65,129] {
                for offset in [0usize,1,3] {
                    for flags in [2,0xad7] {
                        let mut bytes:Vec<u8>=(0..160).map(|i| (i*17+3) as u8).collect();
                        let original=bytes.clone();let address=0x1_0030_0001u64;
                        let base=80+offset;
                        let (word,index)=if mode==4 || mode==8 {(base,255u64)} else {
                            let signed=if width==32 {index as i32 as i64} else {index};
                            ((base as i64+signed.div_euclid(i64::from(width))*width_bytes as i64) as usize,signed as u64)
                        };
                        let mut raw=[0u8;8];raw[..width_bytes].copy_from_slice(&bytes[word..word+width_bytes]);
                        let (output,carry)=reference(u64::from_le_bytes(raw),index,width,operation);
                        let mut expected=original.clone();
                        if operation!=0 {expected[word..word+width_bytes].copy_from_slice(&output.to_le_bytes()[..width_bytes]);}
                        let mut memory=Memory::default();memory.add(address,&mut bytes,operation!=0,false).unwrap();
                        let mut c=seed();c.flags=flags;c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[7]=address+base as u64;if mode>=7 {c.bias=address+base as u64-rip_address(width,mode);}
                        // For 32-bit indices upper register bits must be ignored.
                        c.registers[1]=if width==32 {0x7654_3210_0000_0000 | index as u32 as u64} else {index as u64};
                        let b=c.clone();unsafe{block(&mut c)};success(&c,&b,carry);assert_eq!(c.registers,b.registers);
                        drop(memory);assert_eq!(bytes,expected);checks+=1;
                    }
                }
            }
            for size in 0..width_bytes {
                let mut bytes=vec![0xa5;size];let original=bytes.clone();let address=0x1_0030_0001;
                let mut memory=Memory::default();if size!=0 {memory.add(address,&mut bytes,true,false).unwrap();}
                let mut c=seed();c.registers[7]=address;c.registers[1]=0;if mode>=7 {c.bias=address-rip_address(width,mode);}c.memory=(&mut memory as *mut Memory<'_>)as u64;
                let b=c.clone();unsafe{block(&mut c)};fault(&c,&b,1);drop(memory);assert_eq!(bytes,original);checks+=1;
            }
            if operation!=0 {
                let mut bytes=[0xa5;8];let mut memory=Memory::default();let address=0x1_0030_0001;
                memory.add(address,&mut bytes,false,false).unwrap();let mut c=seed();c.flags=2;c.registers[7]=address;c.registers[1]=0;if mode>=7 {c.bias=address-rip_address(width,mode);}c.memory=(&mut memory as *mut Memory<'_>)as u64;
                let b=c.clone();unsafe{block(&mut c)};fault(&c,&b,2);drop(memory);assert_eq!(bytes,[0xa5;8]);checks+=1;
            }
        }
        let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};
        assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);preserve(&c,&b);checks+=1;
    }
    println!("PASS: {checks} native BT/BTS/BTR32/64 checks: decoded forms, independent bit reference, register aliases, upper32 behavior, signed memory indices, masked immediates, CF and preserved flags, unaligned full64 and biased RIP-relative readonly loads, guarded stores and short-span atomic faults, fuel");
}
"#;
