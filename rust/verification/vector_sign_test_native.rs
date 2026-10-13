//! Independent sign-bit set oracle for VEX VTESTPS and VTESTPD.
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
    for width in [4usize, 8] {
        for wide in 0..2 {
            for mode in 0..9 {
                let (first, second) = match mode {
                    0 => (1, 2),
                    1 | 8 => (15, 8),
                    2 => (8, 15),
                    3 => (15, 15),
                    4 => (1, 1),
                    5 => (2, 7),
                    6 => (15, 15),
                    7 => (8, 5),
                    _ => unreachable!(),
                };
                let memory = (5..=7).contains(&mode);
                let size = if wide == 0 { 16 } else { 32 };
                let mut bytes = vec![
                    0xc4,
                    0x42 | if first < 8 { 0x80 } else { 0 } | if second < 8 { 0x20 } else { 0 },
                    0x79 | wide * 4,
                    if width == 4 { 0x0e } else { 0x0f },
                    if memory { 0 } else { 0xc0 } | ((first & 7) << 3) | (second & 7),
                ];
                if mode == 7 {
                    bytes.extend_from_slice(&32u32.to_le_bytes());
                }
                let d = native_ps5::decode::instruction(&bytes, 0x100)?;
                let v = if wide == 0 { "xmm" } else { "ymm" };
                let reg = if wide == 0 { "XMM" } else { "YMM" };
                let code = format!(
                    "VEX_Vtest{}_{v}_{v}m{}",
                    if width == 4 { "ps" } else { "pd" },
                    size * 8
                );
                if format!("{:?}", d.code()) != code
                    || d.len() != bytes.len()
                    || d.op_count() != 2
                    || format!("{:?}", d.op0_kind()) != "Register"
                    || format!("{:?}", d.op0_register()) != format!("{reg}{first}")
                    || format!("{:?}", d.op1_kind()) != if memory { "Memory" } else { "Register" }
                    || (!memory && format!("{:?}", d.op1_register()) != format!("{reg}{second}"))
                    || (memory
                        && (d.memory_size().size() != size
                            || format!("{:?}", d.memory_base())
                                != match mode {
                                    5 => "RDI",
                                    6 => "R15",
                                    7 => "RIP",
                                    _ => unreachable!(),
                                }
                            || format!("{:?}", d.memory_index()) != "None"))
                    || (mode == 7 && d.ip_rel_memory_address() != 0x100 + bytes.len() as u64 + 32)
                {
                    return Err(format!("Bad VTESTPS/PD fixture {wide}/{mode}: {d:?}"));
                }
                let mut bad_w = bytes.clone();
                bad_w[2] |= 128;
                let mut invalids = vec![
                    bad_w,
                    vec![0x66, 0x0f, 0x38, bytes[3], 0xc1],
                    vec![0x62, 0xf2, 0x7d, 0x08, bytes[3], 0xc1],
                ];
                for prefix in [0x66, 0xf2, 0xf3, 0xf0] {
                    let mut invalid = vec![prefix];
                    invalid.extend_from_slice(&bytes);
                    invalids.push(invalid);
                }
                for invalid in invalids {
                    if native_ps5::aot::translate_rust_memory_region(
                        &invalid,
                        0x100,
                        "unsupported_sign_test",
                    )
                    .is_ok()
                    {
                        return Err(format!("Unsupported sign test accepted: {invalid:02x?}"));
                    }
                }
                let mut reserved = bytes.clone();
                reserved[2] &= !8;
                if native_ps5::aot::translate_rust_memory_region(&reserved, 0x100, "reserved_test")
                    .is_ok()
                {
                    return Err("Reserved VTESTPS/PD vvvv accepted".into());
                }
                if memory {
                    for prefix in [0x64, 0x65, 0x67] {
                        let mut invalid = vec![prefix];
                        invalid.extend_from_slice(&bytes);
                        if native_ps5::aot::translate_rust_memory_region(
                            &invalid,
                            0x100,
                            "guard_test",
                        )
                        .is_ok()
                        {
                            return Err(format!("Unsupported VTESTPS/PD prefix {prefix:x}"));
                        }
                    }
                }
                let name = format!("vector_sign_test_{width}_{wide}_{mode}");
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                forms += 1;
                table.push_str(&format!(
                    "({name},{width},{size},{first},{second},{mode},{}),\n",
                    bytes.len()
                ));
            }
        }
    }
    let asm = directory.join("vector-sign-test.S");
    let obj = directory.join("vector-sign-test.o");
    let source = directory.join("vector-sign-test-driver.rs");
    let binary = directory.join("vector-sign-test-driver");
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
    println!("Decoded {forms} vector sign-test fixtures");
    run(&mut Command::new(binary))
}
const DRIVER: &str = r#"
use native_ps5::{legacy_cpu::Cpu,guest_memory::Memory};
unsafe extern "C"{DECLARATIONS}
type Block=unsafe extern "C" fn(*mut Cpu);
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+3)as u8;}}c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.vectors,b.vectors);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));assert_eq!(c.x87,b.x87);assert_eq!((c.bias,c.memory),(b.bias,b.memory));}
fn unchanged(c:&Cpu,b:&Cpu){preserved(c,b);assert_eq!(c.flags,b.flags);}
fn address(c:&mut Cpu,mode:usize,len:usize,addr:u64){if mode==7{c.bias=addr-(0x100+len as u64+32);}else{c.registers[if mode==6{15}else{7}]=addr;}}
fn expected(old:u64,width:usize,a:&[u8],b:&[u8])->u64{let left:std::collections::BTreeSet<usize>=(0..a.len()/width).filter(|&lane|a[(lane+1)*width-1]>=128).collect();let right:std::collections::BTreeSet<usize>=(0..b.len()/width).filter(|&lane|b[(lane+1)*width-1]>=128).collect();let shared=left.intersection(&right).next().is_some();let outside=right.difference(&left).next().is_some();(old&!0x8d5)|if shared{0}else{64}|if outside{0}else{1}}
fn inputs(size:usize,width:usize,pattern:usize,poison:u8)->([u8;32],[u8;32]){let mut a=[poison;32];let mut b=[!poison;32];let count=1usize<<(size/width);let pairs=count*count;if pattern<pairs{for lane in 0..size/width{a[(lane+1)*width-1]=(poison&127)|((((pattern/count)>>lane)%2)as u8*128);b[(lane+1)*width-1]=(!poison&127)|((((pattern%count)>>lane)%2)as u8*128);}}else if pattern<pairs+size*16{a[..size].fill(0);b[..size].fill(0);let bit=(pattern-pairs)/2;b[bit/8]=1<<(bit%8);if pattern%2==0{a[..size].copy_from_slice(&b[..size]);}}else{let mut random=0x8fedcba123456789u64.wrapping_add(pattern as u64);for i in 0..size{random^=random<<13;random^=random>>7;random^=random<<17;a[i]=random as u8;b[i]=(random>>27)as u8;}}(a,b)}
fn patterns(size:usize,width:usize,full:bool)->Vec<usize>{let pairs=1usize<<(2*size/width);if full{(0..pairs+size*16+128).collect()}else{let mut p=vec![0,1,pairs-1,pairs/2,pairs/3];p.extend(pairs..pairs+size*16+128);p}}
fn main(){let cases:&[(Block,usize,usize,usize,usize,usize,usize)]=&[TABLE];let mut checks=0;
for &(block,width,size,first,second,mode,len)in cases{
 if !(5..=7).contains(&mode){for pattern in patterns(size,width,mode==0){for(poison,flags)in [(0u8,0u64),(255,u64::MAX),(0x55,0xa123456789abcdef)]{let mut c=seed();let(a,z)=inputs(size,width,pattern,poison);c.vectors[first]=a;c.vectors[second]=z;c.flags=flags;let b=c.clone();let e=expected(flags,width,&b.vectors[first][..size],&b.vectors[second][..size]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.flags,e);assert_eq!(c.fuel,19);preserved(&c,&b);checks+=1;}}}
 else{for offset in [0usize,1,3,7,15]{for pattern in patterns(size,width,mode==5&&offset==0){for(poison,flags)in [(0u8,0u64),(255,u64::MAX),(0x55,0xa123456789abcdef)]{let mut c=seed();let(a,z)=inputs(size,width,pattern,poison);c.vectors[first]=a;c.flags=flags;let mut data=vec![0xa5;offset+size];data[offset..].copy_from_slice(&z[..size]);let original=data.clone();let mut memory=Memory::default();memory.add(0x1_00300001,&mut data,false,false).unwrap();c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001+offset as u64);let b=c.clone();let e=expected(flags,width,&b.vectors[first][..size],&original[offset..]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.flags,e);assert_eq!(c.fuel,19);preserved(&c,&b);drop(memory);assert_eq!(data,original);checks+=1;}}}
 for length in 0..size{for flags in [0u64,u64::MAX,0xa123456789abcdef]{let mut data=vec![0xaa;length];let original=data.clone();let mut memory=Memory::default();if length>0{memory.add(0x1_00300001,&mut data,false,false).unwrap();}let mut c=seed();c.flags=flags;c.memory=(&mut memory as *mut Memory<'_>)as u64;address(&mut c,mode,len,0x1_00300001);let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.fuel,c.rip),(1,19,0x100));unchanged(&c,&b);drop(memory);assert_eq!(data,original);checks+=1;}}}
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));unchanged(&c,&b);checks+=1;
 for fault in [1,2,13]{let mut c=seed();c.fault=fault;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(fault,b.stop,20,0x100));unchanged(&c,&b);checks+=1;}
}
println!("PASS: {checks} native VTESTPS/PD checks:4exactVEXcodes, independent signbit-set CF/ZF oracle, exhaustive signmaskpairs/register+memory, nonsignpoison/everybit/random, asymmetric sources andaliases/highregs, clearedOF/SF/AF/PF andpreservedotherflags, readonlyfullspan high/unaligned/RIP/allshortspans, unchangedvectors/FP/fuel/priorfaults");}
"#;
