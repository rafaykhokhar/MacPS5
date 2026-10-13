//! Independent binary-power oracle for VEX signed64 VCVTTSD2SI.
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
    let mut forms = 0;
    for l in 0..2 {
        for mode in 0..6 {
            let (dst, src) = match mode {
                0 => (0, 1),
                1 => (15, 8),
                2 => (9, 15),
                3 => (0, 7),
                4 => (15, 15),
                5 => (8, 5),
                _ => unreachable!(),
            };
            let memory = mode >= 3;
            let mut bytes = vec![
                0xc4,
                1 | 0x40 | if dst < 8 { 0x80 } else { 0 } | if src < 8 { 0x20 } else { 0 },
                0xfb | l * 4,
                0x2c,
                if memory { 0 } else { 0xc0 } | ((dst & 7) << 3) | (src & 7),
            ];
            if mode == 5 {
                bytes.extend_from_slice(&32u32.to_le_bytes());
            }
            let d = native_ps5::decode::instruction(&bytes, 0x100)?;
            let dest = if dst == 0 {
                "RAX".to_owned()
            } else {
                format!("R{dst}")
            };
            if format!("{:?}", d.code()) != "VEX_Vcvttsd2si_r64_xmmm64"
                || d.len() != bytes.len()
                || d.op_count() != 2
                || format!("{:?}", d.op0_register()) != dest
                || format!("{:?}", d.op1_kind()) != if memory { "Memory" } else { "Register" }
                || (!memory && format!("{:?}", d.op1_register()) != format!("XMM{src}"))
                || (memory
                    && (d.memory_size().size() != 8
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
                return Err(format!("Bad truncate64 fixture {l}/{mode}: {d:?}"));
            }
            let mut reserved = bytes.clone();
            reserved[2] &= !8;
            let mut invalids = vec![
                reserved,
                vec![0xf2, 0x48, 0x0f, 0x2c, 0xc1],
                vec![0x62, 0xf1, 0xff, 0x08, 0x2c, 0xc1],
            ];
            for prefix in if memory {
                vec![0x66, 0xf2, 0xf3, 0xf0, 0x64, 0x65, 0x67]
            } else {
                vec![0x66, 0xf2, 0xf3, 0xf0]
            } {
                let mut v = vec![prefix];
                v.extend_from_slice(&bytes);
                invalids.push(v);
            }
            for invalid in invalids {
                if native_ps5::aot::translate_rust_memory_region(&invalid, 0x100, "bad_truncate64")
                    .is_ok()
                {
                    return Err(format!("Unsupported truncate64 accepted: {invalid:02x?}"));
                }
            }
            let name = format!("truncate64_{l}_{mode}");
            assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                &bytes, 0x100, &name,
            )?);
            declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
            table.push_str(&format!("({name},{dst},{src},{mode},{}),\n", bytes.len()));
            forms += 1;
        }
    }
    let asm = directory.join("truncate64.S");
    let obj = directory.join("truncate64.o");
    let source = directory.join("truncate64-driver.rs");
    let binary = directory.join("truncate64-driver");
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
    println!("Decoded {forms} signed64 truncate fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x1f80,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.vectors,b.vectors);assert_eq!(c.flags,b.flags);assert_eq!((c.x87_control,c.x87_status),(b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.registers,b.registers);assert_eq!(c.mxcsr,b.mxcsr);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==5{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==4{15}else{7}]=addr;}}
// Sum individual binary powers into a u128 integer and record all discarded
// negative powers. This never calls the product conversion or host FP casts.
fn oracle(bits:u64,control:u32)->(u64,u32){let exponent=((bits>>52)%2048)as i32;let fraction=bits% (1u64<<52);let negative=bits>>63!=0;if exponent==2047{return(1<<63,1);}if exponent==0&&control&64!=0{return(0,0);}let significand=if exponent==0{fraction}else{fraction+(1u64<<52)};let scale=if exponent==0{-1074}else{exponent-1023-52};let mut integer=0u128;let mut discarded=false;let mut overflow=false;for k in 0..53{if (significand>>k)%2!=0{let power=scale+k;if power<0{discarded=true;}else if power>=64{overflow=true;}else{integer+=1u128<<power;}}}let limit=if negative{1u128<<63}else{(1u128<<63)-1};if overflow||integer>limit{return(1<<63,1);}let value=if negative{(-(integer as i128))as u64}else{integer as u64};(value,if discarded{32}else{0})}
fn verify(c:&Cpu,b:&Cpu,dst:usize,bits:u64){let(value,status)=oracle(bits,b.mxcsr);let trap=status & !(b.mxcsr>>7) !=0;assert_eq!(c.mxcsr,b.mxcsr|status,"bits {bits:016x} control {:x}",b.mxcsr);assert_eq!(c.fault,if trap{15}else{0});if trap{assert_eq!(c.rip,b.rip);}let mut regs=b.registers;if !trap{regs[dst]=value;}assert_eq!(c.registers,regs,"bits {bits:016x} control {:x}",b.mxcsr);assert_eq!(c.fuel,19);preserved(c,b);}
fn inputs(full:bool)->Vec<u64>{let mut v=vec![0,1,(1<<52)-1,1<<52,0x3fd0000000000000,0x3fefffffffffffff,0x3ff0000000000000,0x3ff8000000000000,0x4004000000000000,0x432fffffffffffff,0x4330000000000000,0x43dfffffffffffff,0x43e0000000000000,0x43e0000000000001,0x43efffffffffffff,0x7fefffffffffffff,0x7ff0000000000000,0x7ff0000000000001,0x7ff8000000000000,0x7fffffffffffffff];if full{let mut random=0x935acd321789654fu64;for e in 0..2048u64{random^=random<<13;random^=random>>7;random^=random<<17;v.push(e<<52);v.push((e<<52)|(random% (1<<52)));}}let negative:Vec<u64>=v.iter().map(|b|b|(1<<63)).collect();v.extend(negative);v}
fn controls()->Vec<u32>{let mut v=vec![];for rc in 0..4{for daz in [0,64]{for ftz in [0,0x8000]{for masks in 0..4{for sticky in [0,0x3f]{v.push((0x1f80 & !0x1080)|((masks&1)*128)|((masks>>1)*4096)|(rc<<13)|daz|ftz|sticky);}}}}}v}
fn main(){let cases:&[(Block,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;let controls=controls();for &(block,dst,src,mode,len) in cases{
 for bits in inputs(mode==0){for &control in &controls{let mut c=seed();c.mxcsr=control;c.vectors[src][..8].copy_from_slice(&bits.to_le_bytes());let mut data=bits.to_le_bytes();let mut memory=Memory::default();if mode>=3{memory.add(0x1_00300001,&mut data,false,false).unwrap();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001);}let b=c.clone();unsafe{block(&mut c)};verify(&c,&b,dst,bits);drop(memory);assert_eq!(data,bits.to_le_bytes());checks+=1;}}
 if mode>=3{for offset in [0usize,1,3,7,15]{for bits in inputs(false){let mut data=vec![0xa5;offset+8];data[offset..].copy_from_slice(&bits.to_le_bytes());let original=data.clone();let mut memory=Memory::default();memory.add(0x1_00300001,&mut data,false,false).unwrap();let mut c=seed();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001+offset as u64);let b=c.clone();unsafe{block(&mut c)};verify(&c,&b,dst,bits);drop(memory);assert_eq!(data,original);checks+=1;}}
 for length in 0..8{for control in [0u32,0x3f,0x1f80,0x10000]{let mut data=vec![0xff;length];let original=data.clone();let mut memory=Memory::default();if length>0{memory.add(0x1_00300001,&mut data,false,false).unwrap();}let mut c=seed();c.mxcsr=control;c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(1,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(data,original);checks+=1;}}}
 for bits in [0x3ff0000000000000u64,0x7ff8000000000001]{let mut c=seed();c.mxcsr=0x11f80;c.vectors[src][..8].copy_from_slice(&bits.to_le_bytes());let mut data=bits.to_le_bytes();let mut memory=Memory::default();if mode>=3{memory.add(0x1_00300001,&mut data,false,false).unwrap();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001);}let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(14,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(data,bits.to_le_bytes());checks+=1;}
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13,15]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
 }println!("PASS: {checks} native signed64 VCVTTSD2SI checks: independent binary-power oracle, every exponent/random significands, both signs and i64 boundary neighbors, NaNs/infinity/zeros/subnormals, all RC/DAZ/FTZ/IM/PM/sticky controls, LIG/high registers, exact readonly high/unaligned/RIP memory and every shortspan, invalid/precision fault15 and memoryfault precedence, preserved flags/vector/x87/fuel");}
"#;
