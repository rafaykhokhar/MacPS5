use native_ps5::{
    guest_memory::DataRegion,
    jit_memory::JitArena,
    loader::Image,
    module_executor::{Execution, NativeExecutor},
    modules::Arguments,
};
use std::{path::Path, rc::Rc};
mod compiled_workload_support;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = compiled_workload_support::compile(Path::new(args.get(1).expect("guest .rs path")));
    assert!(!code.is_empty() && code.len() < 0x1f00);
    let mut file = native_ps5::modules::fixture();
    file.resize(0x2000, 0);
    for at in [96, 104] {
        file[at..at + 8].copy_from_slice(&0x2000u64.to_le_bytes());
    }
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut image = Image::load(&file, 0x100000, 16384).unwrap();
    let mut executor = NativeExecutor::new(0x900000, 65536, 10000, 1000000).unwrap();
    let arena = Rc::new(unsafe { JitArena::new(4 * 1024 * 1024).unwrap() });
    unsafe {
        executor
            .register_direct_source(&image, 10000, arena.clone())
            .unwrap();
        executor.set_automatic_translation(true);
    }
    let mut checks = 0;
    let mut warmed = None;
    for _ in 0..2 {
        for count in [0usize, 1, 2, 3, 4, 7, 8, 9, 16, 31, 64, 255] {
            for misalign in [0, 1, 7] {
                let mut data = vec![0xa5; count * 8 + 16];
                let mut expected = data.clone();
                let mut sum = 0u128;
                for n in 0..count {
                    let value = (n as u64).wrapping_mul(0x9e3779b97f4a7c15) ^ u64::MAX;
                    let at = misalign + n * 8;
                    data[at..at + 8].copy_from_slice(&value.to_le_bytes());
                    let next = (u128::from(value) + count as u128) % (1u128 << 64);
                    sum = (sum + next) % (1u128 << 64);
                    let output = (next ^ (next >> 13)) as u64;
                    expected[at..at + 8].copy_from_slice(&output.to_le_bytes());
                }
                let result = executor
                    .run_with_data(
                        &mut image,
                        0x100100,
                        Arguments {
                            size: count as u64,
                            pointer: 0x30000000 + misalign as u64,
                        },
                        &mut [],
                        &mut [DataRegion {
                            address: 0x30000000,
                            bytes: &mut data,
                            writable: true,
                        }],
                    )
                    .unwrap();
                let expected_result = (sum ^ (sum >> 32)) as u32 as i32;
                assert!(matches!(result,Execution::Returned(value) if value==expected_result));
                assert_eq!(data, expected, "count={count} alignment={misalign}");
                checks += 1;
            }
        }
        let usage = (executor.direct_translation_usage(image.bias), arena.used());
        if let Some(old) = warmed {
            assert_eq!(usage, old);
        } else {
            warmed = Some(usage);
        }
    }
    assert_eq!(executor.translation_cache_usage().libraries, 0);
    println!(
        "compiled Rust memory workload: {checks} return/full-buffer checks passed; {} guest bytes; {:?}; {} JIT bytes; stable warm cache",
        code.len(),
        executor.direct_translation_usage(image.bias),
        arena.used()
    );
}
