//! Bounded RDNA integer-block to Metal translation.
//!
//! This is a register-state test ABI, not the console graphics ABI: 256 raw
//! VGPR values per lane and 106 shared SGPR values. Wave operations, memory
//! instructions remain rejected. Floating arithmetic requires an explicit policy;
//! the default entry points deliberately reject it.
use crate::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderStage {
    Vertex,
    Fragment,
}

/// Experimental fixed-mode arithmetic, not full console FP-mode emulation.
/// Round binary32 operations separately to nearest-even, flush signed input and
/// output subnormals to zero, and canonicalize every NaN to 0x7fc00000.
#[derive(Clone, Copy, Debug)]
pub enum FloatPolicy {
    NearestEvenFlushToZero,
}

const FLOAT_HELPERS: &str = r#"
uint rdna_flush(uint x) { return (x & 0x7f800000u)==0 ? (x & 0x80000000u) : x; }
uint rdna_add(uint a,uint b) {
 a=rdna_flush(a); b=rdna_flush(b);
 uint aa=a&0x7fffffffu, bb=b&0x7fffffffu;
 if (aa>0x7f800000u || bb>0x7f800000u) return 0x7fc00000u;
 if (aa==0x7f800000u) return bb==aa && ((a^b)&0x80000000u)!=0 ? 0x7fc00000u : a;
 if (bb==0x7f800000u) return b;
 if (aa==0 && bb==0) return (a&b)&0x80000000u;
 if (aa==0) return b; if (bb==0) return a;
 volatile float result=as_type<float>(a)+as_type<float>(b);
 float rounded=result; return rdna_flush(as_type<uint>(rounded));
}
uint rdna_mul(uint a,uint b) {
 a=rdna_flush(a); b=rdna_flush(b);
 uint aa=a&0x7fffffffu, bb=b&0x7fffffffu, sign=(a^b)&0x80000000u;
 if (aa>0x7f800000u || bb>0x7f800000u) return 0x7fc00000u;
 if (aa==0x7f800000u || bb==0x7f800000u) return aa==0 || bb==0 ? 0x7fc00000u : sign|0x7f800000u;
 if (aa==0 || bb==0) return sign;
 // Integer significand rounding avoids Metal's pre-rounding underflow flush.
 ulong product=ulong((a&0x7fffffu)|0x800000u)*ulong((b&0x7fffffu)|0x800000u);
 uint high=uint(product>>47);
 int exponent=int(aa>>23)+int(bb>>23)-127+int(high);
 uint shift=23u+high+uint(max(0,1-exponent));
 if (shift>63u) return sign;
 ulong rounded=product>>shift;
 ulong remainder=product&((1ul<<shift)-1ul), halfway=1ul<<(shift-1u);
 if (remainder>halfway || (remainder==halfway && (rounded&1ul)!=0)) ++rounded;
 if (exponent<=0) return rounded>=0x800000ul ? sign|0x00800000u : sign;
 if (rounded>=0x1000000ul) {rounded>>=1; ++exponent;}
 if (exponent>=255) return sign|0x7f800000u;
 return sign|(uint(exponent)<<23)|(uint(rounded)&0x7fffffu);
}
"#;

fn source(code: u32, words: &[u32], cursor: &mut usize) -> Result<String> {
    Ok(match code {
        0..=105 => format!("sgpr[{code}]"),
        128..=192 => format!("{}u", code - 128),
        193..=208 => format!("{}u", 192_u32.wrapping_sub(code)),
        240..=248 => {
            const FLOATS: [u32; 9] = [
                0x3f000000, 0xbf000000, 0x3f800000, 0xbf800000, 0x40000000, 0xc0000000, 0x40800000,
                0xc0800000, 0x3e22f983,
            ];
            format!("{}u", FLOATS[(code - 240) as usize])
        }
        255 => {
            let literal = words.get(*cursor).ok_or("Truncated RDNA literal")?;
            *cursor += 1;
            format!("{literal}u")
        }
        256..=511 => format!("v[{}]", code - 256),
        _ => return Err(format!("Unsupported RDNA source {code}")),
    })
}

fn body(words: &[u32], stage: Option<RenderStage>, floats: bool) -> Result<String> {
    if words.is_empty() || words.len() > 4096 {
        return Err("RDNA block must contain 1..=4096 words".into());
    }
    let mut text = String::new();
    let mut cursor = 0;
    let mut exported = false;
    while cursor < words.len() {
        let pc = cursor * 4;
        let word = words[cursor];
        cursor += 1;
        if word == 0xbf810000 {
            if cursor != words.len() || stage.is_some() != exported {
                return Err(
                    "RDNA end must be terminal and render stages require one export".into(),
                );
            }
            return Ok(text);
        }
        if word >> 26 == 0x3e {
            let stage = stage.ok_or("Compute block cannot export render values")?;
            let expected = match stage {
                RenderStage::Vertex => 0xf80008cf,
                RenderStage::Fragment => 0xf800180f,
            };
            if exported || word != expected {
                return Err("Only full POS0(done) or MRT0(done,vm) export is supported".into());
            }
            let registers = *words.get(cursor).ok_or("Truncated RDNA export")?;
            cursor += 1;
            let values: Vec<_> = (0..4)
                .map(|i| format!("v[{}]", (registers >> (i * 8)) & 255))
                .collect();
            text += &format!("exported=as_type<float4>(uint4({}));\n", values.join(","));
            exported = true;
            continue;
        }
        let family = word >> 25;
        if family > 0x3f || family == 0x3e {
            return Err(format!("Unsupported RDNA instruction at PC {pc}"));
        }
        let a = source(word & 511, words, &mut cursor)?;
        let b = format!("v[{}]", (word >> 9) & 255);
        let expression = if family == 0x3f {
            match (word >> 9) & 255 {
                1 => a,
                0x37 => format!("~({a})"),
                op => return Err(format!("Unsupported RDNA VOP1 opcode {op} at PC {pc}")),
            }
        } else {
            match family {
                0x03 if floats => format!("rdna_add({a},{b})"),
                0x08 if floats => format!("rdna_mul({a},{b})"),
                0x25 => format!("{a} + {b}"),
                0x26 => format!("{a} - {b}"),
                0x27 => format!("{b} - {a}"),
                0x1b => format!("{a} & {b}"),
                0x1c => format!("{a} | {b}"),
                0x1d => format!("{a} ^ {b}"),
                0x1e => format!("~({a} ^ {b})"),
                0x1a => format!("{b} << ({a} & 31u)"),
                0x16 => format!("{b} >> ({a} & 31u)"),
                op => return Err(format!("Unsupported RDNA VOP2 opcode {op} at PC {pc}")),
            }
        };
        text += &format!("v[{}] = {expression};\n", (word >> 17) & 255);
    }
    Err("RDNA block is missing terminal s_endpgm 0".into())
}

/// Emit an isolated, predicated integer compute block. Inputs/outputs use the
/// raw register ABI described above; buffer 3 is lane count, buffer 4 activity.
pub fn translate_integer_block(words: &[u32]) -> Result<String> {
    compute(words, false)
}
/// Opt into the documented fixed FP policy. Console mode registers, exceptions,
/// NaN payload preservation and gradual underflow are not implemented.
pub fn translate_float_block(words: &[u32], _policy: FloatPolicy) -> Result<String> {
    compute(words, true)
}
fn compute(words: &[u32], floats: bool) -> Result<String> {
    let body = body(words, None, floats)?;
    let helpers = if floats { FLOAT_HELPERS } else { "" };
    Ok(format!(
        "#include <metal_stdlib>\nusing namespace metal;\n{helpers}\n\
         kernel void rdna_block(device const uint* input [[buffer(0)]],\n\
         constant uint* sgpr [[buffer(1)]], device uint* output [[buffer(2)]],\n\
         constant uint& count [[buffer(3)]], device const uint* active [[buffer(4)]],\n\
         uint i [[thread_position_in_grid]]) {{\n\
         if (i >= count) return;\nuint v[256];\n\
         for (uint r=0; r<256; ++r) v[r]=input[size_t(i)*256+r];\n\
         if (active[i] != 0) {{\n{body}}}\n\
         for (uint r=0; r<256; ++r) output[size_t(i)*256+r]=v[r];\n}}\n"
    ))
}

/// Emit a vertex POS0 or fragment MRT0 export with the same limited integer
/// operations. Values exported as float are bitcast, never numerically converted.
pub fn translate_render_block(words: &[u32], stage: RenderStage) -> Result<String> {
    let body = body(words, Some(stage), false)?;
    let (header, initialize, finish) = match stage {
        RenderStage::Vertex => (
            "struct RdnaVertexOutput { float4 position [[position]]; };\nvertex RdnaVertexOutput rdna_vertex(device const uint* input [[buffer(0)]], constant uint* sgpr [[buffer(1)]], uint i [[vertex_id]]) {\n",
            "input[size_t(i)*256+r]",
            "RdnaVertexOutput result; result.position=exported; return result;",
        ),
        RenderStage::Fragment => (
            "fragment float4 rdna_fragment(device const uint* input [[buffer(0)]], constant uint* sgpr [[buffer(1)]]) {\n",
            "input[r]",
            "return exported;",
        ),
    };
    Ok(format!(
        "#include <metal_stdlib>\nusing namespace metal;\n{header}uint v[256]; for (uint r=0;r<256;++r) v[r]={initialize};\nfloat4 exported;\n{body}{finish}\n}}\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn float_instructions_require_policy_and_reject_modifiers() {
        for opcode in [3_u32, 8] {
            let instruction = (opcode << 25) | 256;
            assert!(translate_integer_block(&[instruction, 0xbf810000]).is_err());
            assert!(
                translate_float_block(
                    &[instruction, 0xbf810000],
                    FloatPolicy::NearestEvenFlushToZero
                )
                .is_ok()
            );
            for modifier in [233, 234, 249, 250] {
                assert!(
                    translate_float_block(
                        &[(opcode << 25) | modifier, 0, 0xbf810000],
                        FloatPolicy::NearestEvenFlushToZero
                    )
                    .is_err()
                );
            }
        }
        assert!(
            translate_float_block(
                &[0xd5030000, 0, 0xbf810000],
                FloatPolicy::NearestEvenFlushToZero
            )
            .is_err()
        );
    }
    #[test]
    fn rejects_unsupported_and_truncated_encodings() {
        let invalid = [
            vec![],
            vec![0x7e0002ff],
            vec![0xbf810000, 0xbf810000],
            vec![0xbf810001],
            vec![0x06000300, 0xbf810000],
            vec![0x7e00027e, 0xbf810000],
            vec![0x7e0002f9, 0x06000600, 0xbf810000],
            vec![0x7e0002fa, 0, 0xbf810000],
            vec![0xbf820000, 0xbf810000],
            vec![0xffffffff, 0xbf810000],
            vec![0xbf810000; 4097],
            vec![0xf800180f, 0x03020100, 0xbf810000],
        ];
        for words in invalid {
            assert!(translate_integer_block(&words).is_err(), "{words:x?}");
        }
        // An end-marker literal is data, so a separate terminal is required.
        assert!(translate_integer_block(&[0x7e0002ff, 0xbf810000]).is_err());
        assert!(translate_integer_block(&[0x7e0002ff, 0xbf810000, 0xbf810000]).is_ok());
    }
    #[test]
    fn render_exports_are_stage_specific_and_complete() {
        for (stage, export) in [
            (RenderStage::Vertex, 0xf80008cf),
            (RenderStage::Fragment, 0xf800180f),
        ] {
            assert!(translate_render_block(&[export, 0x03020100, 0xbf810000], stage).is_ok());
            for words in [
                vec![export],
                vec![0xbf810000],
                vec![export, 0, export, 0, 0xbf810000],
                vec![export ^ 1, 0, 0xbf810000],
                vec![export | 0x2000, 0, 0xbf810000],
            ] {
                assert!(translate_render_block(&words, stage).is_err());
            }
        }
        assert!(
            translate_render_block(&[0xf80008cf, 0, 0xbf810000], RenderStage::Fragment).is_err()
        );
        assert!(translate_render_block(&[0xf800180f, 0, 0xbf810000], RenderStage::Vertex).is_err());
    }
}
