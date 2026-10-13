//! AMD APM Vol4 rev3.24 pp316–317 and470–471. Exact VEX256 forms only.
//! Independent widened-signed-difference and bit-selection references.
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
    let mut declarations = String::from("unsafe extern \"C\" {\n");
    let mut table = String::from("fn compare_cases()->Vec<(Block,usize,usize,usize)>{vec![");
    for dst in 0..16 {
        for first in 0..16 {
            for second in [dst, (dst + 7) % 16] {
                let name = format!("compare_{dst}_{first}_{second}");
                let vex =
                    0xe1 ^ (if dst >= 8 { 0x80 } else { 0 }) ^ (if second >= 8 { 0x20 } else { 0 });
                let bytes = [
                    0xc4,
                    vex,
                    0x85 | ((!first & 15) << 3) as u8,
                    0x66,
                    0xc0 | ((dst & 7) << 3) as u8 | (second & 7) as u8,
                ];
                assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                    &bytes, 0x100, &name,
                )?);
                declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
                table.push_str(&format!("({name},{dst},{first},{second}),"));
            }
        }
    }
    table.push_str("]}\nfn shift_cases()->Vec<(Block,usize,usize,u8)>{vec![");
    for count in 0..256 {
        for (dst, src) in [(0, 0), (15, 15), (14, 15)] {
            let name = format!("shift_{dst}_{src}_{count}");
            let vex = 0xe1 ^ (if src >= 8 { 0x20 } else { 0 });
            let bytes = [
                0xc4,
                vex,
                0x85 | ((!dst & 15) << 3) as u8,
                0x72,
                0xd0 | (src & 7) as u8,
                count as u8,
            ];
            assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
                &bytes, 0x100, &name,
            )?);
            declarations.push_str(&format!("fn {name}(c:*mut Cpu);\n"));
            table.push_str(&format!("({name},{dst},{src},{count}),"));
        }
    }
    table.push_str("]}\n");
    declarations.push_str("}\n");
    for (name, bytes) in [
        ("memory", &[0xc5, 0xfd, 0x66, 0x07][..]),
        ("memory_high", &[0xc5, 0x05, 0x66, 0x3f][..]),
    ] {
        assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
            bytes, 0x100, name,
        )?);
    }
    assembly.push_str(&native_ps5::aot::translate_rust_memory_region(
        &[
            0xc4, 0xe2, 0x7d, 0x58, 0x43, 0x3c, 0xc5, 0xfd, 0x66, 0x05, 0x9d, 0x71, 0x17, 0, 0xc5,
            0xfd, 0x72, 0xd0, 0x1f, 0xc5, 0xfd, 0x7f, 0x05, 0x10, 0xef, 0x47, 0,
        ],
        0x11b255,
        "combined",
    )?);
    let asm = directory.join("packed-compare-shift.S");
    let obj = directory.join("packed-compare-shift.o");
    let source = directory.join("packed-compare-shift-driver.rs");
    let binary = directory.join("packed-compare-shift-driver");
    std::fs::write(&asm, assembly).map_err(|e| e.to_string())?;
    std::fs::write(&source, format!("{DRIVER}\n{declarations}{table}"))
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
type Block=unsafe extern "C" fn(*mut Cpu);
unsafe extern "C"{fn memory(c:*mut Cpu);fn memory_high(c:*mut Cpu);fn combined(c:*mut Cpu);}
fn seed()->Cpu{let mut c=Cpu{rip:0x100,fuel:20,flags:0xad7,mxcsr:0x3fa0,x87_control:0x077f,x87_status:0x20,..Cpu::default()};c.registers=[0xfedcba9876543210;16];for(r,v)in c.vectors.iter_mut().enumerate(){for(i,b)in v.iter_mut().enumerate(){*b=(r*37+i*11+1)as u8;}}c.x87.push_raw(c.x87_control,&mut c.x87_status,[0;10]).unwrap();c}
fn preserved(c:&Cpu,b:&Cpu){assert_eq!(c.registers,b.registers);assert_eq!(c.flags,b.flags);assert_eq!(c.x87,b.x87);assert_eq!((c.mxcsr,c.x87_control,c.x87_status),(b.mxcsr,b.x87_control,b.x87_status));}
fn cmp(a:&[u8;32],b:&[u8;32])->[u8;32]{let mut r=[0;32];for l in 0..8{let start=l*4;let x=i32::from_le_bytes(a[start..start+4].try_into().unwrap())as i64;let y=i32::from_le_bytes(b[start..start+4].try_into().unwrap())as i64;let value=if x-y>0{u32::MAX}else{0};r[start..start+4].copy_from_slice(&value.to_le_bytes());}r}
fn shift(a:&[u8;32],n:u8)->[u8;32]{let mut r=[0;32];for lane in 0..8{for dstbit in 0..32{let srcbit=dstbit+n as usize;if srcbit<32 && a[lane*4+srcbit/8]&(1<<(srcbit%8))!=0 {r[lane*4+dstbit/8]|=1<<(dstbit%8);}}}r}
fn main(){let mut checks=0;let compares=compare_cases();let shifts=shift_cases();let mut random=0x5a3b9c7d812346efu64;
 for n in 0..64{let mut c0=seed();for v in &mut c0.vectors{for lane in v.chunks_exact_mut(4){random^=random<<13;random^=random>>7;random^=random<<17;let values=[i32::MIN,i32::MAX,-1,0,1,0x7f800001,0x40000000,-1073741824];let value=if n<8{values[(random as usize+n)%8]}else{random as i32};lane.copy_from_slice(&value.to_le_bytes());}}
 for &(block,dst,first,second)in &compares{let mut c=c0.clone();let b=c.clone();let mut expected=b.vectors;expected[dst]=cmp(&b.vectors[first],&b.vectors[second]);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected);preserved(&c,&b);assert_eq!(c.fuel,19);checks+=1;}
 for &(block,dst,src,count)in &shifts{let mut c=c0.clone();let b=c.clone();let mut expected=b.vectors;expected[dst]=shift(&b.vectors[src],count);unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected);preserved(&c,&b);assert_eq!(c.fuel,19);checks+=1;}}
 for offset in [0,1,3,7,31,32]{for(block,dst)in[(memory as Block,0),(memory_high,15)]{let mut bytes:Vec<u8>=(0..64).map(|i|(i*17+3)as u8).collect();let before=bytes.clone();let addr=0x1_00300001u64;let mut mem=Memory::default();mem.add(addr,&mut bytes,false,false).unwrap();let mut c=seed();c.memory=(&mut mem as *mut Memory<'_>)as u64;c.registers[7]=addr+offset;let b=c.clone();let mut expected=b.vectors;expected[dst]=cmp(&b.vectors[dst],before[offset as usize..offset as usize+32].try_into().unwrap());unsafe{block(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.vectors,expected);preserved(&c,&b);drop(mem);assert_eq!(bytes,before);checks+=1;}}
 for length in 0..32{for block in [memory as Block,memory_high]{let mut bytes=vec![0xaa;length];let mut mem=Memory::default();let addr=0x1_00300001;if length>0{mem.add(addr,&mut bytes,false,false).unwrap();}let mut c=seed();c.memory=(&mut mem as *mut Memory<'_>)as u64;c.registers[7]=addr;let b=c.clone();unsafe{block(&mut c)};assert_eq!(c.fault,1);assert_eq!(c.rip,b.rip);assert_eq!(c.vectors,b.vectors);preserved(&c,&b);checks+=1;}}
 for value in [i32::MIN,-1,0,1,4,7,8,i32::MAX]{let mut input=value.to_le_bytes();let mut constants=[0;32];for lane in 0..8{constants[lane*4..lane*4+4].copy_from_slice(&(lane as i32).to_le_bytes());}let mut output=[0xaa;32];let mut mem=Memory::default();mem.add(0x1003c,&mut input,false,false).unwrap();mem.add(0x292400,&mut constants,false,false).unwrap();mem.add(0x59a180,&mut output,true,false).unwrap();let mut c=seed();c.rip=0x11b255;c.registers[3]=0x10000;c.memory=(&mut mem as *mut Memory<'_>)as u64;let b=c.clone();unsafe{combined(&mut c)};assert_eq!(c.fault,0);assert_eq!(c.fuel,16);preserved(&c,&b);let mut expected=[0;32];for lane in 0..8{expected[lane*4..lane*4+4].copy_from_slice(&u32::from(value>lane as i32).to_le_bytes());}let mut vectors=b.vectors;vectors[0]=expected;assert_eq!(c.vectors,vectors);drop(mem);assert_eq!(output,expected);checks+=1;}
 let mut c=seed();c.fuel=0;let b=c.clone();unsafe{(compares[0].0)(&mut c)};assert_eq!((c.fault,c.stop,c.fuel,c.rip),(0,4,0,0x100));assert_eq!(c.vectors,b.vectors);preserved(&c,&b);checks+=1;
 println!("PASS: {checks} native packed compare/shift checks: signed extrema/random mixed lanes, all compare register pairs/alias patterns, every shift immediate with high/aliased registers, unaligned full64 readonly memory/full32 short-fault atomicity, preserved GPR/flags/FP, and actual pinned broadcast/compare/shift/store byte sequence");
}
"#;
