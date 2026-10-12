"""Discover supported executable control flow and emit guarded ARM64 blocks.

Static direct edges are followed; indirect destinations need explicit roots or
later discovery. Unsupported instructions remain reported dispatch boundaries.
No address range is guessed from ELF symbol sizes or adjacent data.
"""
import argparse
from collections import deque
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path

from startup_aot import Decoder, entry_code, translate_prefix


@dataclass
class Discovery:
    instructions: dict
    failures: dict
    roots: list
    edges: dict


def discover(data, roots=None, limit=10000):
    if limit<=0:
        raise ValueError("Instruction limit must be positive")
    entry,_=entry_code(data,limit=1)
    roots=sorted(set([entry] if roots is None else roots))
    if not roots or any(pc<0 or pc>=1<<64 for pc in roots):
        raise ValueError("Invalid discovery roots")
    queue=deque(roots)
    queued=set(roots)
    instructions,failures,owners,edges={},{},{},{}
    while queue:
        pc=queue.popleft()
        if pc in owners and owners[pc]!=pc:
            raise ValueError(f"Control flow targets instruction interior at {pc:#x}")
        if len(instructions)>=limit:
            raise ValueError("Reachable instruction budget exceeded")
        try:
            _,code=entry_code(data,limit=15,entry=pc)
            ins=Decoder(code,pc).next()
        except ValueError as error:
            failures[pc]=str(error)
            continue
        # Reject competing decodes in either discovery order. A queued target
        # is a boundary even if its instruction will later be unsupported.
        if any(byte in owners or (byte!=pc and byte in queued) for byte in range(pc,ins.end)):
            raise ValueError(f"Overlapping reachable instructions at {pc:#x}")
        instructions[pc]=ins
        for byte in range(pc,ins.end):
            owners[byte]=pc
        if ins.kind=="ret":
            successors=[]
        elif ins.kind=="jump":
            successors=[ins.target]
        elif ins.kind in ("branch","call"):
            successors=[ins.end,ins.target]
        else:
            successors=[ins.end]
        edges[pc]=successors
        for target in successors:
            if target<0 or target>=1<<64:
                raise ValueError("Control flow address overflows guest address space")
            if target in owners and owners[target]!=target:
                raise ValueError(f"Control flow targets instruction interior at {target:#x}")
            if target not in queued:
                queue.append(target)
                queued.add(target)
    return Discovery(instructions,failures,roots,edges)


def emit(data, discovery, prefix="discovered"):
    if not prefix.isascii() or not prefix.isidentifier():
        raise ValueError("Invalid discovery symbol prefix")
    leaders=set(discovery.roots)
    for pc,ins in discovery.instructions.items():
        if ins.kind in ("branch","jump","call","indirect_call","ret"):
            leaders.update(discovery.edges[pc])
    remaining=set(discovery.instructions)
    blocks=[]
    while remaining:
        start=min(remaining)
        block=[]
        pc=start
        while pc in remaining:
            ins=discovery.instructions[pc]
            block.append(ins)
            remaining.remove(pc)
            if ins.kind in ("branch","jump","call","indirect_call","ret") or ins.end in leaders:
                break
            pc=ins.end
        end=block[-1].end
        _,code=entry_code(data,limit=end-start,entry=start)
        if len(code)!=end-start:
            raise ValueError("Discovered block crosses executable segment")
        symbol=f"{prefix}_{start:x}"
        assembly,decoded=translate_prefix(code,start,symbol,region=True,external_branches=True)
        if decoded!=block:
            raise ValueError("Discovery/emission decoder mismatch")
        blocks.append((symbol,code,block,assembly))
    header=['#pragma once','#include "dispatcher.hpp"','#include "elf_image.hpp"',
            '#include <cstring>','#include <stdexcept>']
    for symbol,_,_,_ in blocks:
        header.append(f'extern "C" void {symbol}(NativePS5::GuestCpu*);')
    header.append(f'inline void Register_{prefix}(NativePS5::Dispatcher& dispatcher,const NativePS5::ElfImage& image) {{')
    if not blocks:
        header.append('    (void)dispatcher; (void)image;')
    for symbol,code,block,_ in blocks:
        header.append(f'    const unsigned char bytes_{symbol}[]={{'+','.join(str(byte) for byte in code)+'};')
        header.append(f'    if (std::memcmp(image.Read({block[0].address},{len(code)}).data(),bytes_{symbol},{len(code)})) throw std::runtime_error("ELF bytes differ from discovered translation");')
    for symbol,_,block,_ in blocks:
        header.extend(f'    dispatcher.Add({ins.address},&{symbol});' for ins in block)
    header.append('}')
    manifest={"sha256":hashlib.sha256(data).hexdigest(),"roots":discovery.roots,
              "instruction_count":len(discovery.instructions),"block_count":len(blocks),
              "untranslated":[{"pc":pc,"reason":reason} for pc,reason in sorted(discovery.failures.items())],
              "indirect_calls":[pc for pc,ins in sorted(discovery.instructions.items()) if ins.kind=="indirect_call"],
              "blocks":[{"start":block[0].address,"end":block[-1].end,"instructions":len(block)} for _,_,block,_ in blocks]}
    return '\n'.join(item[3] for item in blocks),'\n'.join(header)+'\n',manifest


def generate(data, output, prefix="discovered", roots=None, limit=10000):
    discovery=discover(data,roots,limit)
    assembly,header,manifest=emit(data,discovery,prefix)
    output.with_suffix('.S').write_text(assembly)
    output.with_suffix('.hpp').write_text(header)
    output.with_suffix('.json').write_text(json.dumps(manifest,indent=2)+'\n')
    return manifest


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input',type=Path)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--root',type=lambda x:int(x,0),action='append')
    parser.add_argument('--limit',type=int,default=10000)
    args=parser.parse_args()
    manifest=generate(args.input.read_bytes(),args.output,roots=args.root,limit=args.limit)
    print(f"Discovered {manifest['instruction_count']} instructions in {manifest['block_count']} blocks; {len(manifest['untranslated'])} unsupported boundaries")
