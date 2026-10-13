use native_ps5::{
    direct_cpu,
    jit_memory::JitArena,
    legacy_cpu::Cpu,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
fn main() {
    // Independent condition truth-table bits indexed by CF/PF/ZF/SF/OF.
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
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for near in [false, true] {
            for condition in 0..16u8 {
                let mut bytes = if near {
                    vec![0x0f, 0x80 | condition, 5, 0, 0, 0]
                } else {
                    vec![0x70 | condition, 5]
                };
                let branch_len = bytes.len() as u64;
                bytes.extend_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
                let block = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 3).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(block.entry(0).unwrap());
                for bits in 0..32 {
                    let flags = 2
                        | ((bits & 1) as u64)
                        | (((bits >> 1) & 1) as u64) << 2
                        | (((bits >> 2) & 1) as u64) << 6
                        | (((bits >> 3) & 1) as u64) << 7
                        | (((bits >> 4) & 1) as u64) << 11;
                    let taken = (tables[condition as usize] >> bits) & 1 != 0;
                    for fuel in [0, 1, 2, 3, 10] {
                        let mut cpu = Cpu {
                            rip: 0x100,
                            flags,
                            fuel,
                            ..Cpu::default()
                        };
                        cpu.registers.fill(99);
                        run(&mut cpu);
                        let executed = if taken { 2 } else { 3 };
                        assert_eq!(cpu.registers[0], if !taken && fuel >= 2 { 42 } else { 99 });
                        assert!(cpu.registers[1..].iter().all(|v| *v == 99));
                        assert_eq!(cpu.flags, flags);
                        assert_eq!(cpu.fault, 0);
                        assert_eq!(cpu.fuel, fuel.saturating_sub(executed));
                        assert_eq!(cpu.stop, if fuel < executed { 4 } else { 3 });
                        assert_eq!(
                            cpu.rip,
                            if fuel == 0 {
                                0x100
                            } else if taken || fuel >= 2 {
                                0x100 + branch_len + 5
                            } else {
                                0x100 + branch_len
                            }
                        );
                        checks += 1;
                    }
                }
            }
        }
        for bytes in [
            &[0xeb, 0xfe, 0xc3][..],
            &[0xe9, 0xfb, 0xff, 0xff, 0xff, 0xc3],
            &[0x75, 0xfe, 0xc3],
        ] {
            let code = arena
                .publish(&direct_cpu::translate(bytes, 0x100, 2).unwrap())
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            let mut cpu = Cpu {
                rip: 0x100,
                fuel: 100,
                flags: 2,
                ..Cpu::default()
            };
            run(&mut cpu);
            assert_eq!((cpu.rip, cpu.fuel, cpu.stop, cpu.fault), (0x100, 0, 4, 0));
        }
        for bytes in [
            &[0xeb, 1, 0xb8, 1, 0, 0, 0, 0xc3][..],
            &[0xff, 0xe0, 0xc3],
            &[0xe2, 0xfe, 0xc3],
        ] {
            assert!(direct_cpu::translate(bytes, 0x100, 10).is_err());
        }
        let bytes = [0xb8, 7, 0, 0, 0, 0x75, 5, 0xb8, 42, 0, 0, 0, 0xc3];
        let mut file = memory_fixture();
        file[0x100..0x100 + bytes.len()].copy_from_slice(&bytes);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        executor
            .register_direct_in_image(&image, 0x100, &bytes, 4, &arena)
            .unwrap();
        assert!(matches!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(7)
        ));
        println!(
            "PASS: {checks} direct Jcc truth-table/fuel checks, three bounded backward loops, rejected invalid targets/forms and actual executor branch result 7"
        );
    }
}
