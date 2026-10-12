use native_ps5::{Result, aot, decode, discover::Discovery, elf::Elf};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, env, fs, path::Path};

#[derive(Deserialize)]
struct Block {
    start: u64,
    end: u64,
    instructions: usize,
}
#[derive(Deserialize)]
struct Legacy {
    sha256: String,
    roots: BTreeSet<u64>,
    instruction_count: usize,
    blocks: Vec<Block>,
    untranslated: Vec<Boundary>,
}
#[derive(Deserialize)]
struct Boundary {
    pc: u64,
}
fn number(value: &str) -> Result<u64> {
    if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
    .map_err(|e| e.to_string())
}
fn run() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        return Err("Usage: native-ps5 inspect ELF [address] [count] | translate-prefix ELF output.S [address] [symbol] | discover ELF output-base [roots...] | verify-decoding ELF legacy-manifest.json | verify-arithmetic build-directory | verify-gpu build-directory | verify-native ELF build-directory".into());
    }
    if args[1] == "verify-arithmetic" {
        return native_ps5::checks::arithmetic_native(Path::new(&args[2]));
    }
    if args[1] == "verify-gpu" {
        return native_ps5::checks::gpu_native(Path::new(&args[2]));
    }
    if args[1] == "verify-native" || args[1] == "verify-payload" {
        let build = args
            .get(3)
            .ok_or("Usage: native-ps5 verify-native ELF build-directory")?;
        return if args[1] == "verify-payload" {
            native_ps5::checks::payload_native(Path::new(build), Path::new(&args[2]))
        } else {
            native_ps5::checks::native(Path::new(build), Path::new(&args[2]))
        };
    }
    let bytes = fs::read(&args[2]).map_err(|e| e.to_string())?;
    let elf = Elf::parse(&bytes)?;
    match args[1].as_str() {
        "discover" => {
            let output = Path::new(args.get(3).ok_or("Missing output base path")?);
            let roots = if args.len() > 4 {
                args[4..]
                    .iter()
                    .map(|s| number(s))
                    .collect::<Result<BTreeSet<_>>>()?
            } else {
                BTreeSet::from([elf.entry])
            };
            let discovery = Discovery::run(&elf, roots, 65_536)?;
            let artifacts = discovery.emit(&elf, &bytes, "discovered")?;
            let manifest =
                serde_json::to_vec_pretty(&artifacts.manifest).map_err(|e| e.to_string())?;
            fs::write(output.with_extension("S"), artifacts.assembly).map_err(|e| e.to_string())?;
            fs::write(output.with_extension("hpp"), artifacts.header).map_err(|e| e.to_string())?;
            fs::write(output.with_extension("json"), manifest).map_err(|e| e.to_string())?;
            println!(
                "Rust discovered and emitted {} instructions in {} blocks; {} unsupported boundaries",
                artifacts.manifest.instruction_count,
                artifacts.manifest.block_count,
                artifacts.manifest.untranslated.len()
            );
        }
        "translate-prefix" => {
            let output = args.get(3).ok_or("Missing output assembly path")?;
            let address = args
                .get(4)
                .map(|s| number(s))
                .transpose()?
                .unwrap_or(elf.entry);
            let symbol = args
                .get(5)
                .map(String::as_str)
                .unwrap_or("translated_start");
            let (assembly, instructions) =
                aot::translate(elf.code(address, 4096)?, address, symbol, false, false)?;
            fs::write(output, assembly).map_err(|e| e.to_string())?;
            println!(
                "Rust emitted {} instructions as native ARM64",
                instructions.len()
            );
        }
        "inspect" => {
            let mut pc = if let Some(s) = args.get(3) {
                number(s)?
            } else {
                elf.entry
            };
            let count = if let Some(s) = args.get(4) {
                number(s)?
            } else {
                16
            };
            if count > 1_000_000 {
                return Err("Inspection count exceeds limit".into());
            }
            for _ in 0..count {
                let i = decode::instruction(elf.code(pc, 15)?, pc)?;
                println!(
                    "{}",
                    serde_json::to_string(&decode::describe(&i)?).map_err(|e| e.to_string())?
                );
                pc = i.next_ip();
            }
        }
        "verify-decoding" => {
            let path = args.get(3).ok_or("Missing legacy manifest")?;
            let manifest: Legacy =
                serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if format!("{:x}", Sha256::digest(&bytes)) != manifest.sha256 {
                return Err("Manifest ELF hash mismatch".into());
            }
            let header = fs::read_to_string(Path::new(path).with_extension("hpp"))
                .map_err(|e| e.to_string())?;
            let mut expected = BTreeSet::new();
            for line in header.lines() {
                if let Some(rest) = line.trim().strip_prefix("dispatcher.Add(") {
                    let address = number(
                        rest.split(',')
                            .next()
                            .ok_or("Malformed legacy registration")?,
                    )?;
                    if !expected.insert(address) {
                        return Err("Duplicate legacy registration".into());
                    }
                }
            }
            if expected.len() != manifest.instruction_count {
                return Err("Legacy header/count mismatch".into());
            }
            let mut visited = BTreeSet::new();
            let mut total = 0;
            for block in &manifest.blocks {
                if block.end <= block.start || block.instructions == 0 {
                    return Err("Invalid legacy block".into());
                }
                let mut pc = block.start;
                let mut count = 0;
                while pc < block.end {
                    if !expected.contains(&pc) || !visited.insert(pc) {
                        return Err(format!("Instruction boundary differs at {pc:#x}"));
                    }
                    let instruction = decode::instruction(elf.code(pc, 15)?, pc)?;
                    if instruction.next_ip() > block.end {
                        return Err(format!("Decode crosses legacy boundary at {pc:#x}"));
                    }
                    decode::describe(&instruction)?;
                    pc = instruction.next_ip();
                    count += 1;
                }
                if count != block.instructions {
                    return Err(format!("Instruction count differs at {:#x}", block.start));
                }
                total += count;
            }
            if total != manifest.instruction_count {
                return Err("Manifest instruction total mismatch".into());
            }
            let discovery = Discovery::run(&elf, manifest.roots, 20_000)?;
            let artifacts = discovery.emit(&elf, &bytes, "discovered")?;
            let expected_failures: BTreeSet<_> =
                manifest.untranslated.iter().map(|b| b.pc).collect();
            let actual: BTreeSet<_> = discovery.instructions.keys().copied().collect();
            if !expected.is_subset(&actual)
                || expected_failures
                    .iter()
                    .any(|pc| !actual.contains(pc) && !discovery.failures.contains_key(pc))
            {
                return Err(
                    "Rust discovery lost a legacy instruction or reachable boundary".into(),
                );
            }
            println!(
                "PASS: Rust retains all {total} legacy instruction boundaries across {} hash-matched blocks. Current Rust discovery emits {} instructions in {} blocks (including new coverage); every legacy unsupported boundary is still accounted for. This checks structural coverage, not native execution or gameplay.",
                manifest.blocks.len(),
                artifacts.manifest.instruction_count,
                artifacts.manifest.block_count
            );
        }
        _ => return Err("Unknown command".into()),
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
