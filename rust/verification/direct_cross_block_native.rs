use native_ps5::{
    direct_cpu,
    jit_memory::JitArena,
    legacy_cpu::Cpu,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
fn main() {
    // SAFETY: standalone quiescent host owns all mappings and generated code.
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let tables = [
            0xffff0000u32,
            0x0000ffff,
            0xaaaaaaaa,
            0x55555555,
            0xf0f0f0f0,
            0x0f0f0f0f,
            0xfafafafa,
            0x05050505,
            0xff00ff00,
            0x00ff00ff,
            0xcccccccc,
            0x33333333,
            0x00ffff00,
            0xff0000ff,
            0xf0fffff0,
            0x0f00000f,
        ];
        let mut checks = 0;
        for near in [false, true] {
            for cc in 0..16u8 {
                let bytes = if near {
                    vec![0x0f, 0x80 | cc, 0x1a, 0, 0, 0]
                } else {
                    vec![0x70 | cc, 0x1e]
                };
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 1).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for bits in 0..32 {
                    let flags = 2
                        | ((bits & 1) as u64)
                        | (((bits >> 1) & 1) as u64) << 2
                        | (((bits >> 2) & 1) as u64) << 6
                        | (((bits >> 3) & 1) as u64) << 7
                        | (((bits >> 4) & 1) as u64) << 11;
                    let target = if (tables[cc as usize] >> bits) & 1 != 0 {
                        0x120
                    } else {
                        0x100 + bytes.len() as u64
                    };
                    let mut cpu = Cpu {
                        rip: 0x100,
                        fuel: 10,
                        flags,
                        ..Cpu::default()
                    };
                    cpu.registers.fill(0x55);
                    run(&mut cpu);
                    assert_eq!(
                        (cpu.rip, cpu.target, cpu.stop, cpu.fuel, cpu.fault),
                        (target, target, 2, 9, 0)
                    );
                    assert_eq!(cpu.flags, flags);
                    assert_eq!(cpu.registers, [0x55; 16]);
                    checks += 1;
                }
            }
        }
        let mut file = memory_fixture();
        let source = [0x83, 0xff, 0, 0x74, 0x1b]; // CMP EDI,0; JE 0x120, else 0x105
        let fallthrough = [0xb8, 7, 0, 0, 0, 0xc3];
        let taken = [0xb8, 42, 0, 0, 0, 0xc3];
        file[0x100..0x105].copy_from_slice(&source);
        file[0x105..0x10b].copy_from_slice(&fallthrough);
        file[0x120..0x126].copy_from_slice(&taken);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 100, 1000).unwrap();
        executor
            .register_direct_in_image(&image, 0x100, &source, 2, &arena)
            .unwrap();
        assert!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .is_err(),
            "Unregistered target must fail explicitly"
        );
        executor
            .register_direct_in_image(&image, 0x105, &fallthrough, 2, &arena)
            .unwrap();
        executor
            .register_direct_in_image(&image, 0x120, &taken, 2, &arena)
            .unwrap();
        for (argument, expected) in [(0, 42), (1, 7), (u64::MAX, 7)] {
            assert!(
                matches!(executor.run(&mut image,0x100100,Arguments{size:argument,pointer:0},&mut []).unwrap(),Execution::Returned(value) if value==expected)
            );
        }
        executor.remove_image_translations(image.bias);
        file[0x100..0x102].copy_from_slice(&[0xeb, 0x1e]);
        file[0x120..0x122].copy_from_slice(&[0xeb, 0xde]);
        let mut loop_image = Image::load(&file, 0x200000, 4096).unwrap();
        executor
            .register_direct_in_image(&loop_image, 0x100, &file[0x100..0x102], 1, &arena)
            .unwrap();
        executor
            .register_direct_in_image(&loop_image, 0x120, &file[0x120..0x122], 1, &arena)
            .unwrap();
        let error = executor
            .run(&mut loop_image, 0x200100, Arguments::default(), &mut [])
            .err()
            .expect("two-block loop must stop");
        assert!(error.contains("fuel"), "{error}");
        println!(
            "PASS: {checks} external Jcc state checks, separately registered taken/fallthrough execution, missing-target rejection and two-block loop fuel exhaustion"
        );
    }
}
