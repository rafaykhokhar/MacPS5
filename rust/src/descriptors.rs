//! Bounded, process-local descriptors for the selected payload's control pairs.
//! These are modeled kernel-copy controls, not host sockets or network access.
use crate::{Result, guest_memory::Memory};
use std::{cell::Cell, rc::Rc};

const LIMIT: usize = 256;
enum Endpoint {
    Control {
        target: Rc<Cell<u64>>,
        selector: bool,
    },
    Pipe {
        state: Rc<Pipe>,
        writer: bool,
    },
}
struct Pipe {
    control: u64,
    reader_open: Cell<bool>,
    writer_open: Cell<bool>,
}
pub struct Descriptors {
    endpoints: [Option<Endpoint>; LIMIT],
}
impl Default for Descriptors {
    fn default() -> Self {
        Self {
            endpoints: std::array::from_fn(|_| None),
        }
    }
}
impl Descriptors {
    pub fn packet_control_pair(&mut self) -> Result<(u32, u32)> {
        let mut free = self
            .endpoints
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_none());
        let first = free.next().ok_or("Guest descriptor limit reached")?.0;
        let second = free.next().ok_or("Guest descriptor limit reached")?.0;
        let target = Rc::new(Cell::new(0));
        self.endpoints[first] = Some(Endpoint::Control {
            target: target.clone(),
            selector: true,
        });
        self.endpoints[second] = Some(Endpoint::Control {
            target,
            selector: false,
        });
        Ok((first as u32 + 3, second as u32 + 3))
    }
    pub fn kernel_pipe(&mut self, kernel: &Memory<'_>, control: u64) -> Result<(u32, u32)> {
        if control == 0 {
            return Err("Null kernel pipe control".into());
        }
        kernel.validate_write(control, 24)?;
        let mut free = self
            .endpoints
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_none());
        let first = free.next().ok_or("Guest descriptor limit reached")?.0;
        let second = free.next().ok_or("Guest descriptor limit reached")?.0;
        let state = Rc::new(Pipe {
            control,
            reader_open: Cell::new(true),
            writer_open: Cell::new(true),
        });
        self.endpoints[first] = Some(Endpoint::Pipe {
            state: state.clone(),
            writer: false,
        });
        self.endpoints[second] = Some(Endpoint::Pipe {
            state,
            writer: true,
        });
        Ok((first as u32 + 3, second as u32 + 3))
    }
    fn index(descriptor: u64) -> Option<usize> {
        descriptor
            .checked_sub(3)
            .filter(|&n| n < LIMIT as u64)
            .map(|n| n as usize)
    }
    /// Guest errno, matching the retained payload model (zero is success).
    pub fn close(&mut self, descriptor: u64) -> u32 {
        let Some(index) = Self::index(descriptor) else {
            return 9;
        };
        if let Some(endpoint) = self.endpoints[index].take() {
            if let Endpoint::Pipe { state, writer } = endpoint {
                if writer {
                    state.writer_open.set(false);
                } else {
                    state.reader_open.set(false);
                }
            }
            0
        } else {
            9
        }
    }
    /// The caller supplies the same process-owned kernel address space on every
    /// call. No guest address becomes a host pointer; failed writes are atomic.
    pub fn set_socket_option(
        &mut self,
        user: &Memory<'_>,
        kernel: &mut Memory<'_>,
        arguments: [u64; 5],
    ) -> u32 {
        let [descriptor, level, option, address, length] = arguments;
        let Some(endpoint) = Self::index(descriptor).and_then(|i| self.endpoints[i].as_ref())
        else {
            return 9;
        };
        let Endpoint::Control { target, selector } = endpoint else {
            return 38;
        };
        if level != 41 || option != 46 {
            return 42;
        }
        if length != 20 {
            return 22;
        }
        let Ok(value) = user.read(address, 20) else {
            return 14;
        };
        if *selector {
            target.set(u64::from_le_bytes(value[..8].try_into().unwrap()));
        } else {
            let target = target.get();
            if target == 0 || kernel.write(target, value).is_err() {
                return 14;
            }
        }
        0
    }
    /// Transfer through the guest-controlled kernel ring. All ranges are
    /// validated before data or control indices change. Maximum copy: 1 MiB.
    pub fn transfer(
        &self,
        user: &mut Memory<'_>,
        kernel: &mut Memory<'_>,
        descriptor: u64,
        address: u64,
        count: u64,
        write: bool,
    ) -> std::result::Result<u64, u32> {
        let Some(Endpoint::Pipe { state, writer }) =
            Self::index(descriptor).and_then(|i| self.endpoints[i].as_ref())
        else {
            return Err(9);
        };
        if *writer != write {
            return Err(9);
        }
        if count > 1_048_576 {
            return Err(22);
        }
        if count == 0 {
            return Ok(0);
        }
        let c = state.control;
        kernel.validate_write(c, 24).map_err(|_| 14u32)?;
        let used = kernel.load(c, 4).map_err(|_| 14u32)?;
        let input = kernel.load(c + 4, 4).map_err(|_| 14u32)?;
        let output = kernel.load(c + 8, 4).map_err(|_| 14u32)?;
        let capacity = kernel.load(c + 12, 4).map_err(|_| 14u32)?;
        let buffer = kernel.load(c + 16, 8).map_err(|_| 14u32)?;
        if capacity == 0
            || capacity > 0x40000000
            || used > capacity
            || input > capacity
            || output > capacity
            || buffer.checked_add(capacity).is_none()
        {
            return Err(22);
        }
        if write && !state.reader_open.get() {
            return Err(32);
        }
        let available = if write { capacity - used } else { used };
        if available == 0 {
            return if !write && !state.writer_open.get() {
                Ok(0)
            } else {
                Err(35)
            };
        }
        if write && count <= 512 && count > available {
            return Err(35);
        }
        let amount = count.min(available);
        let position = if write { input } else { output } % capacity;
        let first = amount.min(capacity - position);
        let second = amount - first;
        let overlaps =
            |start: u64, length: u64| length != 0 && start < c + 24 && c < start + length;
        if write && (overlaps(buffer + position, first) || overlaps(buffer, second)) {
            return Err(22);
        }
        let checked = (|| -> Result<()> {
            if write {
                let copy = user.read(address, amount as usize)?.to_vec();
                kernel.validate_write(buffer + position, first as usize)?;
                kernel.validate_write(buffer, second as usize)?;
                kernel.write(buffer + position, &copy[..first as usize])?;
                if second != 0 {
                    kernel.write(buffer, &copy[first as usize..])?;
                }
            } else {
                user.validate_write(address, amount as usize)?;
                let mut copy = kernel.read(buffer + position, first as usize)?.to_vec();
                if second != 0 {
                    copy.extend_from_slice(kernel.read(buffer, second as usize)?);
                }
                user.write(address, &copy)?;
            }
            kernel.store(c, if write { used + amount } else { used - amount }, 4)?;
            kernel.store(
                c + if write { 4 } else { 8 },
                (position + amount) % capacity,
                4,
            )?;
            Ok(())
        })();
        checked.map_err(|_| 14u32)?;
        Ok(amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kernel_ring_wraparound_failures_and_close() {
        let mut descriptors = Descriptors::default();
        let mut control = [0u8; 24];
        let mut ring = [0u8; 16];
        let mut bytes = [0u8; 64];
        bytes[..8].copy_from_slice(b"abcdefgh");
        let mut user = Memory::default();
        let mut kernel = Memory::default();
        user.add(0x1000, &mut bytes, true, false).unwrap();
        kernel.add(0x2000, &mut control, true, false).unwrap();
        kernel.add(0x3000, &mut ring, true, false).unwrap();
        kernel.store(0x2004, 12, 4).unwrap();
        kernel.store(0x2008, 12, 4).unwrap();
        kernel.store(0x200c, 16, 4).unwrap();
        kernel.store(0x2010, 0x3000, 8).unwrap();
        let (reader, writer) = descriptors.kernel_pipe(&kernel, 0x2000).unwrap();
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, writer.into(), 0x1000, 8, true),
            Ok(8)
        );
        assert_eq!(kernel.read(0x300c, 4).unwrap(), b"abcd");
        assert_eq!(kernel.read(0x3000, 4).unwrap(), b"efgh");
        let before = kernel.read(0x2000, 24).unwrap().to_vec();
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, writer.into(), 0x1000, 9, true),
            Err(35)
        );
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, reader.into(), 0x103c, 8, false),
            Err(14)
        );
        assert_eq!(kernel.read(0x2000, 24).unwrap(), before);
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, reader.into(), 0x1020, 8, false),
            Ok(8)
        );
        assert_eq!(user.read(0x1020, 8).unwrap(), b"abcdefgh");
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, reader.into(), 0x1020, 1, false),
            Err(35)
        );
        assert_eq!(descriptors.close(writer.into()), 0);
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, reader.into(), 0x1020, 1, false),
            Ok(0)
        );
        let (r2, w2) = descriptors.kernel_pipe(&kernel, 0x2000).unwrap();
        assert_eq!(descriptors.close(r2.into()), 0);
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, w2.into(), 0x1000, 1, true),
            Err(32)
        );
    }
    #[test]
    fn kernel_ring_split_write_is_atomic_and_control_alias_is_rejected() {
        let mut d = Descriptors::default();
        let mut control = [0u8; 24];
        let mut tail = [0x55u8; 4];
        let mut bytes = [9u8; 8];
        let mut user = Memory::default();
        let mut kernel = Memory::default();
        user.add(0x1000, &mut bytes, true, false).unwrap();
        kernel.add(0x2000, &mut control, true, false).unwrap();
        kernel.add(0x300c, &mut tail, true, false).unwrap();
        kernel.store(0x2004, 12, 4).unwrap();
        kernel.store(0x200c, 16, 4).unwrap();
        kernel.store(0x2010, 0x3000, 8).unwrap();
        let (r, w) = d.kernel_pipe(&kernel, 0x2000).unwrap();
        assert_eq!(
            d.transfer(&mut user, &mut kernel, w.into(), 0x1000, 8, true),
            Err(14)
        );
        assert_eq!(kernel.read(0x300c, 4).unwrap(), &[0x55; 4]);
        assert_eq!(kernel.load(0x2000, 4).unwrap(), 0);
        assert_eq!(kernel.load(0x2004, 4).unwrap(), 12);
        kernel.store(0x2010, 0x2000, 8).unwrap();
        assert_eq!(
            d.transfer(&mut user, &mut kernel, w.into(), 0x1000, 8, true),
            Err(22)
        );
        assert_eq!(
            d.transfer(&mut user, &mut kernel, r.into(), 0x1000, 1, true),
            Err(9)
        );
        assert_eq!(
            d.transfer(&mut user, &mut kernel, w.into(), 0x1000, 1_048_577, true),
            Err(22)
        );
        kernel.store(0x2010, u64::MAX - 1, 8).unwrap();
        assert_eq!(
            d.transfer(&mut user, &mut kernel, w.into(), 0x1000, 1, true),
            Err(22)
        );
    }
    #[test]
    fn selection_copy_faults_and_pair_isolation() {
        let mut descriptors = Descriptors::default();
        let (selector, writer) = descriptors.packet_control_pair().unwrap();
        let (_, other_writer) = descriptors.packet_control_pair().unwrap();
        let mut input = [0u8; 40];
        input[..8].copy_from_slice(&0xffff800000001000u64.to_le_bytes());
        input[20..].fill(0xa5);
        let mut output = [0x55u8; 24];
        let mut user = Memory::default();
        let mut kernel = Memory::default();
        user.add(0x1000, &mut input, true, false).unwrap();
        kernel
            .add(0xffff800000001000, &mut output, true, false)
            .unwrap();
        let call = |fd, ptr| [u64::from(fd), 41, 46, ptr, 20];
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(writer, 0x1014)),
            14
        );
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(selector, 0x1000)),
            0
        );
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(other_writer, 0x1014)),
            14
        );
        // A failed selector read must preserve the previous selection.
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(selector, 0x1020)),
            14
        );
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(writer, 0x1014)),
            0
        );
        assert_eq!(
            kernel.read(0xffff800000001000, 24).unwrap(),
            [&[0xa5; 20][..], &[0x55; 4][..]].concat()
        );
        user.store(0x1000, 0xffff800000001008, 8).unwrap();
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(selector, 0x1000)),
            0
        );
        let before = kernel.read(0xffff800000001000, 24).unwrap().to_vec();
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(writer, 0x1014)),
            14
        );
        assert_eq!(kernel.read(0xffff800000001000, 24).unwrap(), before);
        assert_eq!(descriptors.close(writer.into()), 0);
        assert_eq!(descriptors.close(writer.into()), 9);
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(writer, 0x1014)),
            9
        );
        assert_eq!(descriptors.close(selector.into()), 0);
        let replacement = descriptors.packet_control_pair().unwrap();
        assert_eq!(replacement, (selector, writer));
        // Reusing the descriptor numbers must not retain the old target.
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(replacement.1, 0x1014)),
            14
        );
    }
    #[test]
    fn capacity_failure_is_atomic_and_freed_slots_are_reused() {
        let mut d = Descriptors::default();
        for i in 0..128 {
            assert_eq!(d.packet_control_pair().unwrap(), (3 + 2 * i, 4 + 2 * i));
        }
        assert!(d.packet_control_pair().is_err());
        assert_eq!(d.close(3), 0);
        assert!(d.packet_control_pair().is_err());
        assert_eq!(d.close(258), 0);
        assert_eq!(d.packet_control_pair().unwrap(), (3, 258));
        for fd in [0, 2, 259, u64::MAX] {
            assert_eq!(d.close(fd), 9);
        }
    }
    #[test]
    fn read_only_kernel_and_argument_errors() {
        let mut d = Descriptors::default();
        let (s, w) = d.packet_control_pair().unwrap();
        let mut input = [0u8; 20];
        input[..8].copy_from_slice(&0x2000u64.to_le_bytes());
        let mut output = [7u8; 20];
        let mut user = Memory::default();
        let mut kernel = Memory::default();
        user.add(0x1000, &mut input, false, false).unwrap();
        kernel.add(0x2000, &mut output, false, false).unwrap();
        assert_eq!(
            d.set_socket_option(&user, &mut kernel, [s.into(), 41, 46, 0x1000, 20]),
            0
        );
        assert_eq!(
            d.set_socket_option(&user, &mut kernel, [w.into(), 41, 46, 0x1000, 20]),
            14
        );
        assert_eq!(kernel.read(0x2000, 20).unwrap(), &[7; 20]);
        assert_eq!(
            d.set_socket_option(&user, &mut kernel, [s.into(), 0, 46, 0x1000, 20]),
            42
        );
        assert_eq!(
            d.set_socket_option(&user, &mut kernel, [s.into(), 41, 46, 0x1000, 19]),
            22
        );
    }
}
