//! Actual translated abort calls and guest handler instructions, not Cpu injection.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, ExitKind, NativeExecutor, Service, memory_fixture},
    scheduler::{Scheduler, Step},
};
use std::{collections::BTreeSet, path::Path};
const IMAGE: u64 = 0x100000;
const REAL: u64 = 0x10000000;
const DATA: u64 = 0x37000000;
const ABORT: u64 = 0x7e0000;
const ACTION: u64 = ABORT + 16;
const SIGNAL: u64 = ABORT + 32;
const MASK: u64 = ABORT + 48;
const EXIT: u64 = ABORT + 64;
const SLEEP: u64 = ABORT + 80;
fn mov(c: &mut Vec<u8>, opcode: u8, value: u64) {
    c.extend([0x48, opcode]);
    c.extend(value.to_le_bytes());
}
fn call(c: &mut Vec<u8>, address: u64) {
    mov(c, 0xb8, address);
    c.extend([0xff, 0xd0]);
}
fn patch(i: &mut Image, offset: u64, value: u64) -> Result<(), String> {
    let address = REAL + offset;
    let seg = i
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing abort GOT")?;
    let at = (address - seg.address) as usize;
    seg.bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}
#[derive(Clone, Copy, Debug)]
enum Case {
    Default,
    Ignored,
    Handler,
    Nodefer,
    NodeferMasked,
    HandlerSleep,
    Reset,
    Nested,
    HandlerExit,
}
fn handler(second: bool, case: Case) -> Vec<u8> {
    let mut c = vec![0x53];
    mov(&mut c, 0xbb, DATA);
    c.extend([0x48, 0x89, 0x7b, 32]); // actual handler argument
    c.extend([0xc7, 0x43, 40, if second { 2 } else { 1 }, 0, 0, 0]);
    mov(&mut c, 0xbf, 0);
    mov(&mut c, 0xbe, 0);
    mov(&mut c, 0xba, DATA + 80);
    call(&mut c, MASK);
    if !second {
        match case {
            Case::HandlerSleep => {
                // Real guest instructions establish GPR/vector/FP-control state,
                // suspend in nanosleep, and record it after actual resumption.
                c.extend([0x49, 0xbc]);
                c.extend(0x1234_5678_9abc_def0u64.to_le_bytes());
                c.extend([0x0f, 0xae, 0x93, 0xa0, 0, 0, 0]); // ldmxcsr [rbx+160]
                c.extend([0xd9, 0xab, 0xa4, 0, 0, 0]); // fldcw [rbx+164]
                c.extend([0xc5, 0xfa, 0x6f, 0x83, 0xd0, 0, 0, 0]); // vmovdqu xmm0,[rbx+208]
                mov(&mut c, 0xbf, DATA + 144);
                mov(&mut c, 0xbe, 0);
                call(&mut c, SLEEP);
                c.extend([0x48, 0x89, 0x83, 0xb8, 0, 0, 0]); // service result [rbx+184]
                c.extend([0x4c, 0x89, 0xa3, 0xc0, 0, 0, 0]); // saved r12 [rbx+192]
                c.extend([0x0f, 0xae, 0x9b, 0xb0, 0, 0, 0]); // stmxcsr [rbx+176]
                c.extend([0xd9, 0xbb, 0xb4, 0, 0, 0]); // fnstcw [rbx+180]
                c.extend([0xc5, 0xfa, 0x7f, 0x83, 0xe0, 0, 0, 0]); // vmovdqu [rbx+224],xmm0
            }
            Case::Nested => {
                mov(&mut c, 0xbf, 6);
                mov(&mut c, 0xbe, IMAGE + 0x500);
                call(&mut c, SIGNAL);
                call(&mut c, ABORT);
                c.extend([0x0f, 0x0b]);
            }
            Case::Reset => {
                call(&mut c, ABORT);
                c.extend([0x0f, 0x0b]);
            }
            Case::HandlerExit => {
                mov(&mut c, 0xbf, 7);
                call(&mut c, EXIT);
                c.extend([0x0f, 0x0b]);
            }
            _ => {
                // Changing disposition to ignore must not defeat abort after return.
                mov(&mut c, 0xbf, 6);
                mov(&mut c, 0xbe, 1);
                call(&mut c, SIGNAL);
            }
        }
    }
    c.extend([0x5b, 0xc3]);
    c
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    for case in [
        Case::Default,
        Case::Ignored,
        Case::Handler,
        Case::Nodefer,
        Case::NodeferMasked,
        Case::HandlerSleep,
        Case::Reset,
        Case::Nested,
        Case::HandlerExit,
    ] {
        let mut f = memory_fixture();
        f.resize(0x800, 0);
        f[56..58].copy_from_slice(&1u16.to_le_bytes());
        f[96..104].copy_from_slice(&0x800u64.to_le_bytes());
        f[104..112].copy_from_slice(&0x800u64.to_le_bytes());
        let mut main = Vec::new();
        mov(&mut main, 0xbf, 6);
        mov(&mut main, 0xbe, DATA);
        mov(&mut main, 0xba, 0);
        call(&mut main, ACTION);
        mov(&mut main, 0xbf, 1);
        mov(&mut main, 0xbe, DATA + 64);
        mov(&mut main, 0xba, 0);
        call(&mut main, MASK);
        // Real wchar_to_loop_convert abort branch, followed by guest UD2.
        call(&mut main, REAL + 0x1027cf);
        main.extend([0x0f, 0x0b]);
        f[0x100..0x100 + main.len()].copy_from_slice(&main);
        for (offset, second) in [(0x300, false), (0x500, true)] {
            let code = handler(second, case);
            f[offset..offset + code.len()].copy_from_slice(&code);
        }
        let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
        for (at, s) in [
            (ABORT, Service::Abort),
            (ACTION, Service::Sigaction),
            (SIGNAL, Service::Signal),
            (MASK, Service::ThreadSigmask),
            (EXIT, Service::Exit),
            (SLEEP, Service::Nanosleep),
        ] {
            n.register_service(at, s)?;
        }
        n.configure_manual_sleep_clock(0)?;
        n.set_worker_quantum(Some(3))?;
        let mut real = Image::load(file, REAL, 64 * 1024 * 1024)?;
        patch(&mut real, 0x26fab0, ABORT)?;
        let mut a = Image::load(&f, IMAGE, 65536)?;
        let dir = directory.join(format!("{case:?}"));
        unsafe {
            n.compile_image(
                file,
                REAL,
                BTreeSet::from([0x1027cf]),
                20,
                archive,
                &dir.join("real"),
            )?;
            n.compile_image(
                &f,
                IMAGE,
                BTreeSet::from([0x100, 0x300, 0x500]),
                2000,
                archive,
                &dir.join("handlers"),
            )?;
        }
        let mut data = [0u8; 256];
        let target = match case {
            Case::Default => 0,
            Case::Ignored => 1,
            _ => IMAGE + 0x300,
        };
        data[..8].copy_from_slice(&target.to_le_bytes());
        let flags = match case {
            Case::Reset => 4u32,
            Case::Nodefer | Case::NodeferMasked => 16,
            _ => 0,
        };
        data[8..12].copy_from_slice(&flags.to_le_bytes());
        if matches!(case, Case::NodeferMasked) {
            data[12] = 32;
        }
        data[152..160].copy_from_slice(&1_000i64.to_le_bytes());
        data[160..164].copy_from_slice(&0x3f80u32.to_le_bytes());
        data[164..166].copy_from_slice(&0x077fu16.to_le_bytes());
        data[208..224]
            .copy_from_slice(&[0x81, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0xff]);
        data[64] = 32; // initially blocked SIGABRT
        let mut yields = 0;
        let execution = if matches!(case, Case::HandlerSleep) {
            let mut scheduler = Scheduler::new(3, 10000, 10000)?;
            scheduler.enqueue(&mut n, 2, IMAGE + 0x100, 0)?;
            let mut slept = false;
            let mut terminal = None;
            for _ in 0..1000 {
                let observed = scheduler.step(
                    &mut n,
                    &mut a,
                    &mut [&mut real],
                    &mut [DataRegion {
                        address: DATA,
                        bytes: &mut data,
                        writable: true,
                    }],
                )?;
                match observed {
                    Step::Yielded(_) => yields += 1,
                    Step::Sleeping { deadline_ns } => {
                        slept = true;
                        assert_eq!(&data[176..208], &[0u8; 32]);
                        assert_eq!(&data[224..240], &[0u8; 16]);
                        n.advance_sleep_clock(deadline_ns)?;
                    }
                    Step::SignalTerminated { signal } => {
                        terminal = Some(Execution::SignalTerminated { signal });
                        break;
                    }
                    _ => {}
                }
            }
            assert!(slept, "Handler never entered a real service wait");
            terminal.ok_or("Sleeping abort handler did not terminate")?
        } else {
            let mut execution = n.run_worker_with_data(
                &mut a,
                IMAGE + 0x100,
                0,
                &mut [&mut real],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true,
                }],
            )?;

            while let Execution::QuantumYield(mut pending) = execution {
                yields += 1;
                assert!(
                    yields < 1000,
                    "abort handler exceeded fixture quantum bound"
                );
                execution = n.resume_quantum(
                    &mut a,
                    &mut [&mut real],
                    &mut pending,
                    &mut [DataRegion {
                        address: DATA,
                        bytes: &mut data,
                        writable: true,
                    }],
                )?;
            }
            execution
        };
        assert!(yields > 0);
        if matches!(case, Case::HandlerExit) {
            assert!(
                matches!(execution,Execution::ExitRequested(r) if r.kind==ExitKind::Normal && r.status==7)
            );
            assert_eq!(n.signal_termination(), None);
        } else {
            assert!(matches!(
                execution,
                Execution::SignalTerminated { signal: 6 }
            ));
            assert_eq!(n.signal_termination(), Some(6));
            assert_eq!(n.exit_request(), None);
        }
        if matches!(case, Case::Default | Case::Ignored) {
            assert_eq!(&data[32..48], &[0u8; 16]);
        } else {
            assert_eq!(u64::from_le_bytes(data[32..40].try_into().unwrap()), 6);
            assert_eq!(
                u32::from_le_bytes(data[40..44].try_into().unwrap()),
                if matches!(case, Case::Nested) { 2 } else { 1 }
            );
            assert_eq!(data[80] & 32 == 0, matches!(case, Case::Nodefer));
        }
        if matches!(case, Case::HandlerSleep) {
            assert_eq!(
                u32::from_le_bytes(data[176..180].try_into().unwrap()),
                0x3f80
            );
            assert_eq!(
                u16::from_le_bytes(data[180..182].try_into().unwrap()),
                0x077f
            );
            assert_eq!(u64::from_le_bytes(data[184..192].try_into().unwrap()), 0);
            assert_eq!(
                u64::from_le_bytes(data[192..200].try_into().unwrap()),
                0x1234_5678_9abc_def0
            );
            assert_eq!(&data[224..240], &data[208..224]);
            // These are preservation across handler service suspension, NOT an
            // observation of CPU/FP restored after the handler's terminal return.
        }
        // Process latch is persistent; no second guest entry can resume after abort.
        assert!(
            n.run_worker_with_data(
                &mut a,
                IMAGE + 0x100,
                0,
                &mut [&mut real],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true
                }]
            )
            .is_err()
        );
        println!(
            "Native abort {case:?}: {yields} quantum yields, actual handler/termination verified"
        );
    }
    Ok(())
}
