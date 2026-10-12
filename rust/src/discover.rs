//! Bounded reachable-code discovery. Unsupported instructions are explicit boundaries.
use crate::{Result, aot, decode, elf::Elf};
use iced_x86::Instruction;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub struct Discovery {
    pub instructions: BTreeMap<u64, Instruction>,
    pub failures: BTreeMap<u64, String>,
    pub roots: BTreeSet<u64>,
}
impl Discovery {
    pub fn run(elf: &Elf<'_>, roots: BTreeSet<u64>, limit: usize) -> Result<Self> {
        if roots.is_empty() || limit == 0 {
            return Err("Discovery needs roots and a positive budget".into());
        }
        let mut queue: VecDeque<_> = roots.iter().copied().collect();
        let mut queued = roots.clone();
        let mut owners = BTreeMap::new();
        let mut result = Self {
            instructions: BTreeMap::new(),
            failures: BTreeMap::new(),
            roots,
        };
        while let Some(pc) = queue.pop_front() {
            if owners.get(&pc).is_some_and(|owner| *owner != pc) {
                return Err(format!(
                    "Control flow enters instruction interior at {pc:#x}"
                ));
            }
            if result.instructions.len() >= limit {
                return Err("Reachable instruction budget exceeded".into());
            }
            let ins = elf
                .code(pc, 15)
                .and_then(|bytes| decode::instruction(bytes, pc))
                .and_then(|i| aot::supported(&i).map(|()| i));
            let i = match ins {
                Ok(i) => i,
                Err(e) => {
                    result.failures.insert(pc, e);
                    continue;
                }
            };
            for byte in pc..i.next_ip() {
                if owners.contains_key(&byte) || (byte != pc && queued.contains(&byte)) {
                    return Err(format!("Overlapping instructions at {pc:#x}"));
                }
            }
            for byte in pc..i.next_ip() {
                owners.insert(byte, pc);
            }
            result.instructions.insert(pc, i);
            for target in aot::successors(&i) {
                if owners.get(&target).is_some_and(|owner| *owner != target) {
                    return Err(format!(
                        "Control flow enters instruction interior at {target:#x}"
                    ));
                }
                if queued.insert(target) {
                    queue.push_back(target);
                }
            }
        }
        Ok(result)
    }
    pub fn emit(&self, elf: &Elf<'_>, file: &[u8], prefix: &str) -> Result<Artifacts> {
        self.emit_with_trace(elf, file, prefix, &crate::trace::selected()?)
    }
    pub fn emit_with_trace(
        &self,
        elf: &Elf<'_>,
        file: &[u8],
        prefix: &str,
        trace_pcs: &BTreeSet<u64>,
    ) -> Result<Artifacts> {
        // Validate even when every root is unsupported and there are no blocks.
        aot::translate(&[0xc3], 0, prefix, false, false)?;
        let mut leaders = self.roots.clone();
        for i in self.instructions.values() {
            if aot::terminal(i) {
                leaders.extend(aot::successors(i));
            }
        }
        let mut remaining: BTreeSet<_> = self.instructions.keys().copied().collect();
        let mut blocks = vec![];
        let mut assembly = String::new();
        let mut declarations = String::new();
        let mut checks = String::new();
        let mut registrations = String::new();
        while let Some(start) = remaining.first().copied() {
            let mut pcs = vec![];
            let mut pc = start;
            let end = loop {
                let i = self
                    .instructions
                    .get(&pc)
                    .ok_or("Missing discovered instruction")?;
                remaining.remove(&pc);
                pcs.push(pc);
                if aot::terminal(i)
                    || leaders.contains(&i.next_ip())
                    || !remaining.contains(&i.next_ip())
                {
                    break i.next_ip();
                }
                pc = i.next_ip();
            };
            let length = usize::try_from(end - start).map_err(|_| "Block too large")?;
            let bytes = elf.code(start, length)?;
            if bytes.len() != length {
                return Err("Block crosses executable segment".into());
            }
            let symbol = format!("{prefix}_{start:x}");
            let (text, decoded) =
                aot::translate_with_trace(bytes, start, &symbol, true, true, trace_pcs)?;
            if decoded.iter().map(Instruction::ip).collect::<Vec<_>>() != pcs {
                return Err("Discovery/emission boundary mismatch".into());
            }
            assembly.push_str(&text);
            declarations.push_str(&format!(
                "extern \"C\" void {symbol}(NativePS5::GuestCpu*);\n"
            ));
            checks.push_str(&format!(
                "    const unsigned char bytes_{symbol}[]={{ {} }};\n",
                bytes
                    .iter()
                    .map(u8::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            checks.push_str(&format!("    if (std::memcmp(image.Read({start},{length}).data(),bytes_{symbol},{length})) throw std::runtime_error(\"ELF bytes differ from Rust translation\");\n"));
            for pc in &pcs {
                registrations.push_str(&format!("    dispatcher.Add({pc},&{symbol});\n"));
            }
            blocks.push(Block {
                start,
                end,
                instructions: pcs.len(),
            });
        }
        if blocks.is_empty() {
            checks.push_str("    (void)dispatcher; (void)image;\n");
        }
        let header = format!(
            "// Generated Rust-to-legacy-runtime ABI glue.\n#pragma once\n#include \"dispatcher.hpp\"\n#include \"elf_image.hpp\"\n#include <cstring>\n#include <stdexcept>\n{declarations}inline void Register_{prefix}(NativePS5::Dispatcher& dispatcher,const NativePS5::ElfImage& image) {{\n{checks}{registrations}}}\n"
        );
        let manifest = Manifest {
            sha256: format!("{:x}", Sha256::digest(file)),
            roots: self.roots.iter().copied().collect(),
            instruction_count: self.instructions.len(),
            block_count: blocks.len(),
            untranslated: self
                .failures
                .iter()
                .map(|(&pc, reason)| Failure {
                    pc,
                    reason: reason.clone(),
                })
                .collect(),
            indirect_calls: self
                .instructions
                .values()
                .filter(|i| i.is_call_near_indirect())
                .map(Instruction::ip)
                .collect(),
            blocks,
        };
        Ok(Artifacts {
            assembly,
            header,
            manifest,
        })
    }
}
pub struct Artifacts {
    pub assembly: String,
    pub header: String,
    pub manifest: Manifest,
}
#[derive(Serialize)]
pub struct Manifest {
    pub sha256: String,
    pub roots: Vec<u64>,
    pub instruction_count: usize,
    pub block_count: usize,
    pub untranslated: Vec<Failure>,
    pub indirect_calls: Vec<u64>,
    pub blocks: Vec<Block>,
}
#[derive(Serialize)]
pub struct Failure {
    pub pc: u64,
    pub reason: String,
}
#[derive(Serialize)]
pub struct Block {
    pub start: u64,
    pub end: u64,
    pub instructions: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(code: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 120];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        for (p, v) in [(16, 3u16), (18, 62), (52, 64), (54, 56), (56, 1)] {
            b[p..p + 2].copy_from_slice(&v.to_le_bytes());
        }
        for (p, v) in [(20, 1u32), (64, 1), (68, 5)] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        for (p, v) in [
            (24, 0x1000u64),
            (32, 64),
            (72, 120),
            (80, 0x1000),
            (96, code.len() as u64),
            (104, code.len() as u64),
        ] {
            b[p..p + 8].copy_from_slice(&v.to_le_bytes());
        }
        b.extend(code);
        b
    }
    #[test]
    fn follows_both_edges_and_terminates_cycles() {
        let b = fixture(&[0x74, 2, 0xeb, 0xfc, 0xc3]);
        let elf = Elf::parse(&b).unwrap();
        let d = Discovery::run(&elf, BTreeSet::from([elf.entry]), 10).unwrap();
        assert_eq!(
            d.instructions.keys().copied().collect::<Vec<_>>(),
            vec![0x1000, 0x1002, 0x1004]
        );
        assert!(d.failures.is_empty());
        let out = d.emit(&elf, &b, "loop_test").unwrap();
        assert_eq!(out.manifest.block_count, 3);
        assert!(
            out.header.find("std::memcmp").unwrap() < out.header.find("dispatcher.Add").unwrap()
        );
    }
    #[test]
    fn rejects_overlapping_targets_in_both_discovery_orders() {
        for code in [
            &[0x74, 1, 0x48, 0x89, 0xc0, 0xc3][..],
            &[0xeb, 3, 0x48, 0x89, 0xc0, 0xeb, 0xfc],
        ] {
            let b = fixture(code);
            let elf = Elf::parse(&b).unwrap();
            assert!(Discovery::run(&elf, BTreeSet::from([0x1000, 0x1002]), 20).is_err());
        }
    }
    #[test]
    fn unsupported_boundary_is_explicit_and_budget_is_enforced() {
        let b = fixture(&[0x90, 0x0f, 0x0b]);
        let elf = Elf::parse(&b).unwrap();
        let d = Discovery::run(&elf, BTreeSet::from([elf.entry]), 10).unwrap();
        assert_eq!(d.instructions.len(), 1);
        assert!(d.failures.contains_key(&0x1001));
        assert!(Discovery::run(&elf, BTreeSet::from([elf.entry]), 1).is_err());
        assert!(d.emit(&elf, &b, "bad-symbol").is_err());
    }
}
