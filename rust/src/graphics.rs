//! Synchronous graphics queue for the explicit raw-register fixture ABI.
//! Guest shader objects and DCBs use guarded snapshots, but this API does not
//! claim the full AGC submission-service calling convention or resource model.
use crate::{
    Result,
    guest_memory::Memory,
    guest_shader, guest_submission,
    metal_render::{self, Frame, Inputs},
    pm4::State,
    render_plan::Registry,
};

#[derive(Default)]
pub struct Queue {
    shaders: Registry,
    committed: State,
}
impl Queue {
    /// Snapshot/register a shader only when the current renderer implements all
    /// its declared requirements. Failed registration preserves the registry.
    pub fn register_shader(&mut self, memory: &Memory<'_>, header: u64) -> Result<()> {
        let registration = guest_shader::read(memory, header)?;
        crate::rdna_metal::translate_render_block(&registration.words, registration.stage)?;
        registration.register_simple(&mut self.shaders)
    }
    pub fn state(&self) -> &State {
        &self.committed
    }
    /// Return a new frame only after GPU completion and readback. Packet state
    /// commits on that same success boundary. A failed request preserves prior
    /// queue state and never modifies a previously returned frame. GPU work uses
    /// a fresh offscreen target and retains no borrowed guest address or buffer.
    pub fn submit(
        &mut self,
        memory: &Memory<'_>,
        descriptor: u64,
        inputs: Inputs<'_>,
        width: u32,
        height: u32,
    ) -> Result<Frame> {
        let submission = guest_submission::prepare(memory, descriptor, &self.committed)?;
        let plans = self
            .shaders
            .prepare(&submission.decoded, inputs.vertices.len())?;
        let frame = metal_render::render(&plans, inputs, width, height)?;
        self.committed = submission.decoded.final_state;
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_guest_descriptors_never_change_queue_state() {
        let mut queue = Queue::default();
        let memory = Memory::default();
        let vertices = [[0; 256]];
        let fragment = [0; 256];
        let scalars = [0; 106];
        for descriptor in [0, 1, 0x10000, u64::MAX] {
            assert!(
                queue
                    .submit(
                        &memory,
                        descriptor,
                        Inputs {
                            vertices: &vertices,
                            fragment: &fragment,
                            scalars: &scalars
                        },
                        1,
                        1
                    )
                    .is_err()
            );
            assert_eq!(queue.state(), &State::default());
        }
        assert!(queue.register_shader(&memory, 0x10000).is_err());
        assert_eq!(queue.state(), &State::default());
    }
}
