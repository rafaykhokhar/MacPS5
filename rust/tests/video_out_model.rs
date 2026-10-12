use native_ps5::video_out::*;
use native_ps5::{Result, direct_memory, equeue, guest_memory, metal_present};
#[derive(Default)]
struct Mock {
    fail: bool,
    calls: usize,
    close_failed: bool,
}
impl Backend for Mock {
    type Surface = ();
    fn open(&mut self, _: u32, _: u32) -> Result<()> {
        Ok(())
    }
    fn present(&mut self, _: &mut (), rgba: &[u8]) -> Result<metal_present::Evidence> {
        self.calls += 1;
        assert_eq!(rgba.len(), 128 * 4);
        if self.fail {
            return Err("injected failure".into());
        }
        Ok(metal_present::Evidence {
            device_name: "test only".into(),
            presented_time: 1.,
            drawable_id: 0,
            attempts: 1,
        })
    }
    fn close(&mut self, _: &mut ()) -> Result<()> {
        if self.fail && !self.close_failed {
            self.close_failed = true;
            return Err("injected close failure".into());
        }
        Ok(())
    }
}
#[test]
fn guarded_registration_pins_and_completion() {
    exercise(false);
}
#[test]
fn backend_failure_has_no_event_and_cannot_replay() {
    exercise(true);
}
fn exercise(fail: bool) {
    let _metal = MetalBackend;
    let mut v = VideoOut::new(
        Mock {
            fail,
            calls: 0,
            close_failed: false,
        },
        Some(FlipEventPolicy {
            ident: 1,
            filter: 0,
            flags: 0,
            fflags: 0,
        }),
    );
    let mut owner = guest_memory::OwnedMappings::default();
    let mut bytes = [0; 512];
    let mut m = guest_memory::Memory::default();
    m.add(0x1000, &mut bytes, true, false).unwrap();
    m.attach_owned(&mut owner).unwrap();
    let mut d = direct_memory::DirectMemory::default();
    d.allocate(&mut m, 0x40000, 0x20000, 3, 0x1000).unwrap();
    let physical = m.load(0x1000, 8).unwrap();
    d.map(&mut m, 0x1008, 0x40000, 0x33, 0, physical, 0x20000)
        .unwrap();
    let address = m.load(0x1008, 8).unwrap();
    m.store(0x1020, address, 8).unwrap();
    m.store(0x1040, address + 0x20000, 8).unwrap();
    let h = v.open(255, 0, 0, 0).unwrap();
    v.set_flip_rate(h, 0).unwrap();
    v.set_buffer_attribute(&mut m, 0x1080, 0x8000000022000000, 0, 128, 1, 0, 0, 0)
        .unwrap();
    m.store(0x1088, 999, 8).unwrap();
    assert!(
        v.register_buffers(&m, &mut d, h, 0, 0, 0x1020, 2, 0x1080, 0, 0)
            .is_err()
    );
    v.set_buffer_attribute(&mut m, 0x1080, 0x8000000022000000, 0, 128, 1, 0, 0, 0)
        .unwrap();
    v.register_buffers(&m, &mut d, h, 0, 0, 0x1020, 2, 0x1080, 0, 0)
        .unwrap();
    assert!(d.release(&mut m, physical, 0x40000).is_err());
    let mut q = equeue::Equeues::default();
    m.write(0x1100, b"flip\0").unwrap();
    q.create(&mut m, 0x1110, 0x1100).unwrap();
    let queue = m.load(0x1110, 8).unwrap();
    v.add_flip_event(&q, queue, h, 123).unwrap();
    if fail {
        assert!(v.submit_flip(&m, &mut q, h, 0, 1, 42).is_err());
        assert!(
            v.submit_flip(&m, &mut q, h, 0, 1, 42)
                .unwrap_err()
                .to_string()
                .contains("faulted")
        );
        assert_eq!(q.pending_count(queue).unwrap(), 0);
        assert!(v.last_evidence().is_none());
    } else {
        v.submit_flip(&m, &mut q, h, 0, 1, 42).unwrap();
        assert_eq!(q.pending_count(queue).unwrap(), 1);
        assert!(v.last_evidence().is_some());
        for _ in 1..equeue::MAX_EVENTS {
            q.enqueue(
                queue,
                equeue::Event {
                    ident: 0,
                    filter: 0,
                    flags: 0,
                    fflags: 0,
                    data: 0,
                    udata: 0,
                },
            )
            .unwrap();
        }
        assert!(
            v.submit_flip(&m, &mut q, h, 1, 1, 43)
                .unwrap_err()
                .to_string()
                .contains("full before")
        );
    }
    v.delete_flip_event(queue, h).unwrap();
    assert!(v.submit_flip(&m, &mut q, h, 0, 1, 43).is_err());
    if fail {
        assert!(v.close(&mut d, h).is_err());
        assert!(d.release(&mut m, physical, 0x40000).is_err());
    }
    v.close(&mut d, h).unwrap();
    d.release(&mut m, physical, 0x40000).unwrap();
    assert!(v.close(&mut d, h).is_err());
}

#[derive(Default)]
struct RejectOpen;
impl Backend for RejectOpen {
    type Surface = ();
    fn open(&mut self, _: u32, _: u32) -> Result<()> {
        Err("injected open failure".into())
    }
    fn present(&mut self, _: &mut (), _: &[u8]) -> Result<metal_present::Evidence> {
        panic!("unopened surface")
    }
    fn close(&mut self, _: &mut ()) -> Result<()> {
        panic!("unopened surface")
    }
}
#[test]
fn registration_rolls_back_first_pin_and_all_pins_on_open_failure() {
    for second_is_direct in [false, true] {
        let mut v = VideoOut::new(RejectOpen, None);
        v.configure_event_policy(None).unwrap();
        let mut owner = guest_memory::OwnedMappings::default();
        let mut controls = [0u8; 256];
        let mut ordinary = vec![0u8; 0x20000];
        let mut m = guest_memory::Memory::default();
        m.add(0x1000, &mut controls, true, false).unwrap();
        m.add(0x100000, &mut ordinary, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let mut d = direct_memory::DirectMemory::default();
        d.allocate(&mut m, 0x40000, 0x20000, 3, 0x1000).unwrap();
        let physical = m.load(0x1000, 8).unwrap();
        d.map(&mut m, 0x1008, 0x40000, 0x33, 0, physical, 0x20000)
            .unwrap();
        let address = m.load(0x1008, 8).unwrap();
        m.store(0x1020, address, 8).unwrap();
        m.store(
            0x1040,
            if second_is_direct {
                address + 0x20000
            } else {
                0x100000
            },
            8,
        )
        .unwrap();
        let h = v.open(255, 0, 0, 0).unwrap();
        assert!(v.configure_event_policy(None).is_err());
        v.set_buffer_attribute(&mut m, 0x1080, 0x8000000022000000, 0, 128, 1, 0, 0, 0)
            .unwrap();
        let error = v
            .register_buffers(&m, &mut d, h, 0, 0, 0x1020, 2, 0x1080, 0, 0)
            .unwrap_err();
        assert!(error.to_string().contains(if second_is_direct {
            "injected open"
        } else {
            "not owned direct"
        }));
        // Successful release proves every pin was rolled back, including the
        // first valid descriptor when the second pin fails.
        d.release(&mut m, physical, 0x40000).unwrap();
        v.close(&mut d, h).unwrap();
        v.configure_event_policy(None).unwrap();
    }
}
