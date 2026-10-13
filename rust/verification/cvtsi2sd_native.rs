//! Native signed64 conversion compared with an independent binary64-lattice
//! binary search using u128 exact fixed-point distances, not host FP conversion.
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
    let cases: [(&str, &[u8]); 6] = [
        ("observed", &[0xc4, 0xe1, 0x83, 0x2a, 0xc0]),
        ("alias", &[0xc4, 0xe1, 0xfb, 0x2a, 0xc0]),
        ("high", &[0xc4, 0xc1, 0x83, 0x2a, 0xe1]),
        ("memory", &[0xc4, 0xe1, 0x83, 0x2a, 0x27]),
        ("memory_alias", &[0xc4, 0xe1, 0xdb, 0x2a, 0x27]),
        ("destination_high", &[0xc4, 0x41, 0xfb, 0x2a, 0xff]),
    ];
    let mut assembly = String::new();
    for (name, bytes) in cases {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes,
            0x1772da,
            &format!("cvtsi_{name}"),
        )?);
    }
    let asm = directory.join("cvtsi.S");
    let obj = directory.join("cvtsi.o");
    let source = directory.join("cvtsi-driver.rs");
    let binary = directory.join("cvtsi-driver");
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
unsafe extern "C"{fn cvtsi_observed(c:*mut Cpu);fn cvtsi_alias(c:*mut Cpu);fn cvtsi_high(c:*mut Cpu);fn cvtsi_memory(c:*mut Cpu);fn cvtsi_memory_alias(c:*mut Cpu);fn cvtsi_destination_high(c:*mut Cpu);}
// Exact binary64 magnitude multiplied by2^52; searched interval[1,2^63]
// makes every candidate an exact u128 integer. This reference uses no FP cast,
// translator helper or significand truncation/rounding algorithm.
fn scaled(bits:u64)->u128{let exponent=((bits>>52)&0x7ff)-1023;(((bits&((1<<52)-1))|(1<<52))as u128)<<exponent}
fn reference(value:i64,rc:u32)->(u64,u32){if value==0{return(0,0);}let sign=if value<0{1u64<<63}else{0};let target=(value.unsigned_abs()as u128)<<52;let(mut lo,mut hi)=(0x3ff0000000000000u64,0x43e0000000000000u64);while lo<hi{let middle=lo+(hi-lo).div_ceil(2);if scaled(middle)<=target{lo=middle;}else{hi=middle-1;}}if scaled(lo)==target{return(sign|lo,0);}let upper=lo+1;let choose_upper=match rc{0=>{let down=target-scaled(lo);let up=scaled(upper)-target;up<down||(up==down&&lo&1!=0)},1=>value<0,2=>value>0,_=>false};(sign|if choose_upper{upper}else{lo},32)}
fn seed(control:u32)->Cpu{let mut c=Cpu{rip:0x1772da,fuel:20,flags:0xad7,mxcsr:control,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+1)as u8;}}c}
fn unchanged(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!((c.x87_control,c.x87_status),(b.x87_control,b.x87_status));}
fn main(){let mut checks=0;let mut values=vec![0,1,-1,i64::MIN,i64::MAX];for power in 52..=62{for delta in -3..=3{values.push((1i64<<power)+delta);values.push(-((1i64<<power)+delta));}}let mut random=0x1234567abcdefu64;for _ in 0..256{random^=random<<13;random^=random>>7;random^=random<<17;values.push(random as i64);}
 for(block,dst,merge,source,memory_form)in[(cvtsi_observed as unsafe extern "C" fn(*mut Cpu),0,15,0,false),(cvtsi_alias,0,0,0,false),(cvtsi_high,4,15,9,false),(cvtsi_memory,4,15,7,true),(cvtsi_memory_alias,4,4,7,true),(cvtsi_destination_high,15,0,15,false)]{
 for &value in &values{for rc in 0..4{let(bits,status)=reference(value,rc);for extra in [0,0x40,0x8000,0x8040]{for unmask in [false,true]{let mut control=0x1f80|(rc<<13)|extra|5;if unmask{control&=!0x1000;}let fault=status!=0&&unmask;let mut data=value.to_le_bytes();let mut memory=Memory::default();memory.add(0x1_00300001,&mut data,false,false).unwrap();let mut c=seed(control);c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[source]=if memory_form{0x1_00300001}else{value as u64};let b=c.clone();let mut expected=c.vectors;if !fault{expected[dst]=[0;32];expected[dst][..8].copy_from_slice(&bits.to_le_bytes());expected[dst][8..16].copy_from_slice(&b.vectors[merge][8..16]);}unsafe{block(&mut c)};assert_eq!(c.vectors,expected,"value={value} rc={rc} dst={dst} merge={merge}");assert_eq!(c.mxcsr,control|status);assert_eq!(c.fault,if fault{15}else{0});assert_eq!(c.fuel,19);if fault{assert_eq!(c.rip,b.rip);}unchanged(&c,&b);drop(memory);assert_eq!(data,value.to_le_bytes());checks+=1;}}}}
 // Reserved control never commits lanes; preexisting sticky precision does not
 // re-raise for an exact integer, even with every exception unmasked.
 for control in [0x10000,0x10020,0x20]{let mut data=1i64.to_le_bytes();let mut memory=Memory::default();memory.add(0x1_00300001,&mut data,false,false).unwrap();let mut c=seed(control);c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[source]=if memory_form{0x1_00300001}else{1};let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.mxcsr,control);if control&0x10000!=0{assert_eq!(c.fault,14);assert_eq!(c.rip,b.rip);assert_eq!(c.vectors,b.vectors);}else{assert_eq!(c.fault,0);assert_eq!(&c.vectors[dst][..8],&0x3ff0000000000000u64.to_le_bytes());assert_eq!(&c.vectors[dst][8..16],&b.vectors[merge][8..16]);assert_eq!(&c.vectors[dst][16..],&[0;16]);}unchanged(&c,&b);checks+=1;}
 if memory_form{for length in 0..8{let mut bytes=vec![0xaa;length];let mut memory=Memory::default();if length>0{memory.add(0x1_00300001,&mut bytes,false,false).unwrap();}let mut c=seed(0x1f85);c.memory=(&mut memory as *mut Memory<'_>)as u64;c.registers[7]=0x1_00300001;let b=c.clone();unsafe{block(&mut c)};assert_eq!((c.fault,c.mxcsr,c.rip),(1,b.mxcsr,b.rip));assert_eq!(c.vectors,b.vectors);unchanged(&c,&b);drop(memory);assert_eq!(bytes,vec![0xaa;length]);checks+=1;}}
 }
 println!("PASS: {checks} native signed64 VCVTSI2SD independent exact binary64-lattice comparisons; all RC, DAZ/FTZ inert, precision sticky/unmasked no-commit, extreme/tie/random inputs, merge aliases/high registers, full64 unaligned memory and fault atomicity");
}
"#;
