//! Bounded direct-backend entry probe; no import binding or game compatibility claim.
use native_ps5::{
    jit_memory::JitArena,
    loader::Image,
    module_executor::{Execution, NativeExecutor},
    modules::Arguments,
};
use std::rc::Rc;
fn main() -> Result<(), String> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: direct_elf_probe <ELF>")?;
    let file = std::fs::read(path).map_err(|e| e.to_string())?;
    let elf = native_ps5::elf::Elf::parse(&file)?;
    let root = elf.entry;
    println!("ELF entry: {root:#x}");
    let mut pc = root;
    for _ in 0..16 {
        let i = native_ps5::decode::instruction(elf.code(pc, 15)?, pc)?;
        println!("{pc:#x}: {:?}", i.code());
        pc += i.len() as u64;
        if i.flow_control() != iced_x86::FlowControl::Next {
            break;
        }
    }
    let mut image = Image::load(&file, 0x100000000, 128 * 1024 * 1024)?;
    unsafe {
        let arena = Rc::new(JitArena::new(16 * 1024 * 1024)?);
        let mut executor = NativeExecutor::new(0x900000000, 1024 * 1024, 100000, 100000)?;
        executor.register_direct_source(&image, 10000, arena.clone())?;
        executor.set_automatic_translation(true);
        let entry = image.entry;
        match executor.run(&mut image, entry, Arguments::default(), &mut []) {
            Ok(Execution::Returned(value)) => {
                println!("Entry returned {value}; imports and process setup were not provided")
            }
            Ok(_) => println!("Entry yielded; requires runtime orchestration"),
            Err(error) => println!("STOP: {error}"),
        }
        println!(
            "JIT used: {} bytes; dylibs: {}",
            arena.used(),
            executor.translation_cache_usage().libraries
        );
    }
    Ok(())
}
