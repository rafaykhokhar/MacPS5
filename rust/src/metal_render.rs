//! Reusable synchronous Metal rendering for the explicit register-buffer ABI.
//! No AGC submission service, texture resources, depth or blending are implied.
use crate::{Result, render_plan::DrawPlan};

pub struct Inputs<'a> {
    pub vertices: &'a [[u32; 256]],
    pub fragment: &'a [u32; 256],
    pub scalars: &'a [u32; 106],
}
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Name queried from the actual MTLDevice that rendered this frame.
    pub device_name: String,
    /// Row-major RGBA8Unorm pixels, after GPU completion.
    pub pixels: Vec<u8>,
}
fn validate(plans: &[DrawPlan], inputs: &Inputs<'_>, width: u32, height: u32) -> Result<()> {
    if width == 0
        || height == 0
        || width > 2048
        || height > 2048
        || inputs.vertices.is_empty()
        || inputs.vertices.len() > 4096
        || plans.len() > 128
    {
        return Err("Metal render input or target exceeds supported bounds".into());
    }
    let mut work = 0_u64;
    for plan in plans {
        if u64::from(plan.first_vertex) + u64::from(plan.count) > inputs.vertices.len() as u64 {
            return Err("Metal draw exceeds actual vertex buffer capacity".into());
        }
        work += u64::from(plan.count);
        if work > 65536 {
            return Err("Metal render vertex work limit exceeded".into());
        }
    }
    Ok(())
}
/// Execute only plans constructed by the checked shader registry. All input
/// validation and pipeline compilation finish before the command is submitted.
/// Target/readback are each capped at16MiB; input registers are capped at4MiB.
pub fn render(plans: &[DrawPlan], inputs: Inputs<'_>, width: u32, height: u32) -> Result<Frame> {
    validate(plans, &inputs, width, height)?;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        apple::render(plans, inputs, width, height)
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        Err("Metal rendering requires Apple Silicon macOS".into())
    }
}
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[allow(unsafe_code)]
mod apple;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_resource_bounds_before_platform_work() {
        let vertices = [[0; 256]; 4];
        let fragment = [0; 256];
        let scalars = [0; 106];
        let inputs = Inputs {
            vertices: &vertices,
            fragment: &fragment,
            scalars: &scalars,
        };
        for (width, height) in [(0, 1), (1, 0), (2049, 1), (1, 2049)] {
            assert!(validate(&[], &inputs, width, height).is_err());
        }
        let plan = DrawPlan {
            first_vertex: 2,
            count: 3,
            vertex_msl: String::new(),
            fragment_msl: String::new(),
        };
        assert!(validate(&[plan], &inputs, 256, 256).is_err());
        assert!(validate(&[], &inputs, 2048, 2048).is_ok());
        assert!(
            validate(
                &[],
                &Inputs {
                    vertices: &[],
                    fragment: &fragment,
                    scalars: &scalars
                },
                1,
                1
            )
            .is_err()
        );
    }
}
