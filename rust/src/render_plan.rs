//! Explicit synthetic pipeline bindings for bounded PM4 draws.
//!
//! This is not the console render-state ABI: initial VGPR/SGPR buffers are
//! supplied by the caller, with triangle-list topology, one RGBA8 target and no
//! depth, blending, textures or instancing. Unsupported state is rejected before
//! any Metal work can be submitted.
use crate::{
    Result, pm4,
    rdna_metal::{self, RenderStage},
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
struct Shader {
    stage: RenderStage,
    words: Arc<[u32]>,
}
#[derive(Default)]
pub struct Registry {
    shaders: BTreeMap<u64, Shader>,
}

pub struct DrawPlan {
    pub(crate) first_vertex: u32,
    pub(crate) count: u32,
    pub(crate) vertex_msl: String,
    pub(crate) fragment_msl: String,
}
impl Registry {
    /// Copy immutable code; replacement cannot alter an already prepared plan.
    pub fn register(&mut self, address: u64, stage: RenderStage, words: &[u32]) -> Result<()> {
        const LIMIT: u64 = 1 << 48;
        if address == 0
            || address & 255 != 0
            || address >= LIMIT
            || words.is_empty()
            || words.len() > 4096
            || words.len() as u64 * 4 > LIMIT - address
        {
            return Err("Invalid shader address or code length".into());
        }
        if !self.shaders.contains_key(&address) && self.shaders.len() >= 64 {
            return Err("Shader registry limit exceeded".into());
        }
        for (&base, image) in &self.shaders {
            if base != address
                && address < base + image.words.len() as u64 * 4
                && base < address + words.len() as u64 * 4
            {
                return Err("Overlapping shader registrations".into());
            }
        }
        self.shaders.insert(
            address,
            Shader {
                stage,
                words: Arc::from(words),
            },
        );
        Ok(())
    }
    fn source(&self, state: &pm4::State, stage: RenderStage) -> Result<String> {
        let low = if stage == RenderStage::Vertex {
            0xc8
        } else {
            8
        };
        let lo = *state
            .shader
            .get(&low)
            .ok_or("Missing shader program low register")?;
        let hi = *state
            .shader
            .get(&(low + 1))
            .ok_or("Missing shader program high register")?;
        if hi > 255 {
            return Err("Unsupported shader address high bits".into());
        }
        let address = (u64::from(hi) << 40) | (u64::from(lo) << 8);
        let (&base, image) = self
            .shaders
            .range(..=address)
            .next_back()
            .ok_or("Unregistered shader")?;
        let offset = (address - base) / 4;
        if offset >= image.words.len() as u64 || image.stage != stage {
            return Err("Shader entry outside registered stage/code".into());
        }
        rdna_metal::translate_render_block(&image.words[offset as usize..], stage)
    }
    /// Validate the complete submission before returning any executable plans.
    /// Vertex capacity describes the caller's actual 256-word-per-vertex buffer.
    pub fn prepare(
        &self,
        submission: &pm4::Submission,
        vertex_capacity: usize,
    ) -> Result<Vec<DrawPlan>> {
        if vertex_capacity > 4096 || submission.draws.len() > 128 {
            return Err("Render submission capacity exceeded".into());
        }
        let mut total = 0_u64;
        let mut result = Vec::new();
        for draw in &submission.draws {
            let state = &draw.state;
            if state.shader.len() != 4
                || !state.context.is_empty()
                || state.user_config.len() != 2
                || state.user_config.get(&0x242) != Some(&4)
                || state.user_config.get(&0x24a) != Some(&draw.first_vertex)
                || state.instances != Some(1)
                || draw.instances != 1
                || draw.flags != 0
                || u64::from(draw.first_vertex) + u64::from(draw.count) > vertex_capacity as u64
            {
                return Err("Unsupported render state or vertex range".into());
            }
            total += u64::from(draw.count);
            if total > 65536 {
                return Err("Render vertex work limit exceeded".into());
            }
            result.push(DrawPlan {
                first_vertex: draw.first_vertex,
                count: draw.count,
                vertex_msl: self.source(state, RenderStage::Vertex)?,
                fragment_msl: self.source(state, RenderStage::Fragment)?,
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Registry, pm4::Submission) {
        let mut registry = Registry::default();
        registry
            .register(
                0x10000,
                RenderStage::Vertex,
                &[0xf80008cf, 0x03020100, 0xbf810000],
            )
            .unwrap();
        registry
            .register(
                0x20000,
                RenderStage::Fragment,
                &[0xf800180f, 0x03020100, 0xbf810000],
            )
            .unwrap();
        let state = pm4::State {
            shader: BTreeMap::from([(0xc8, 0x100), (0xc9, 0), (8, 0x200), (9, 0)]),
            user_config: BTreeMap::from([(0x242, 4), (0x24a, 1)]),
            instances: Some(1),
            ..pm4::State::default()
        };
        let submission = pm4::Submission {
            final_state: state.clone(),
            draws: vec![pm4::Draw {
                first_vertex: 1,
                count: 3,
                instances: 1,
                flags: 0,
                state,
            }],
        };
        (registry, submission)
    }
    #[test]
    fn complete_submission_validated_and_plans_preserve_code() {
        let (mut registry, mut submission) = fixture();
        let plans = registry.prepare(&submission, 4).unwrap();
        assert_eq!((plans[0].first_vertex, plans[0].count), (1, 3));
        let source = plans[0].vertex_msl.clone();
        registry
            .register(0x10000, RenderStage::Vertex, &[0xffffffff])
            .unwrap();
        assert_eq!(plans[0].vertex_msl, source);
        assert!(registry.prepare(&submission, 4).is_err());
        let (registry, _) = fixture();
        submission.draws.push(submission.draws[0].clone());
        submission.draws[1].state.context.insert(0, 0);
        assert!(registry.prepare(&submission, 4).is_err());
    }
    #[test]
    fn rejects_unknown_state_addresses_stages_and_work_limits() {
        let (mut registry, submission) = fixture();
        for variant in 0..9 {
            let mut bad = pm4::Submission {
                final_state: submission.final_state.clone(),
                draws: submission.draws.clone(),
            };
            let draw = &mut bad.draws[0];
            match variant {
                0 => draw.flags = 32,
                1 => draw.instances = 2,
                2 => draw.count = 4,
                3 => {
                    draw.state.shader.insert(0xc9, 256);
                }
                4 => {
                    draw.state.shader.insert(0xc8, 0x300);
                }
                5 => {
                    draw.state.shader.insert(0xc8, 0x200);
                }
                6 => {
                    draw.state.user_config.insert(0x242, 5);
                }
                7 => draw.state.instances = Some(2),
                _ => {
                    draw.state.shader.insert(0, 0);
                }
            }
            assert!(registry.prepare(&bad, 4).is_err());
        }
        assert!(registry.prepare(&submission, 4097).is_err());
        let mut large = pm4::Submission {
            final_state: submission.final_state.clone(),
            draws: vec![submission.draws[0].clone(); 129],
        };
        assert!(registry.prepare(&large, 4).is_err());
        large.draws.truncate(17);
        for draw in &mut large.draws {
            draw.first_vertex = 0;
            draw.count = 4096;
            draw.state.user_config.insert(0x24a, 0);
        }
        assert!(registry.prepare(&large, 4096).is_err());
        for address in [0, 1, 1 << 48] {
            assert!(
                registry
                    .register(address, RenderStage::Vertex, &[0])
                    .is_err()
            );
        }
        assert!(
            registry
                .register(0x10000, RenderStage::Vertex, &[])
                .is_err()
        );
        assert!(
            registry
                .register(0x1ff00, RenderStage::Vertex, &[0; 65])
                .is_err()
        );
        assert!(
            registry
                .register(0x10000, RenderStage::Vertex, &[0; 4097])
                .is_err()
        );
    }
}
