"""Build and run all currently implemented native Apple Silicon milestones."""
import argparse
import pathlib
import subprocess
import struct
import json

import elf_translation
import prototype
import startup_aot
import graphics_elf_fixture
import reachable_aot


def run(command):
    subprocess.run([str(item) for item in command], check=True)


def vector_xor_checks(source,build):
    assemblies=[];entries=[]
    header=['#pragma once','#include "guest_cpu.hpp"','struct VectorXorFixture { void (*block)(NativePS5::GuestCpu*); unsigned destination,first,second,size; bool memory; };']
    for bits in (128,256):
        for pp in (0,1):
            for dst,first,second,mem in ((0,1,2,False),(1,1,2,False),(2,1,2,False),(8,10,9,False),(7,7,7,False),(0,1,0,True),(1,1,0,True)):
                name=f'vector_xor_{bits}_{pp}_{dst}_{first}_{second}_{int(mem)}'
                vex2=((0 if dst>=8 else 1)<<7)|0x40|((0 if second>=8 else 1)<<5)|1
                vex3=((~first&15)<<3)|(4 if bits==256 else 0)|pp
                code=bytes([0xc4,vex2,vex3,0x57,(0 if mem else 0xc0)|((dst&7)<<3)|(second&7),0xc3])
                assemblies.append(startup_aot.translate_prefix(code,0,name)[0])
                header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
                entries.append(f'{{{name},{dst},{first},{second},{bits//8},{str(mem).lower()}}}')
    header.append('inline const VectorXorFixture vectorXorFixtures[]={'+','.join(entries)+'};')
    (build/'vector-xor-fixtures.S').write_text('\n'.join(assemblies))
    (build/'vector-xor-fixtures.hpp').write_text('\n'.join(header))
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror','-I',build,'-I',source,
         source/'guest_cpu.cpp',source/'vector_xor_verify.cpp',build/'vector-xor-fixtures.S','-o',build/'vector-xor-verify'])
    run([build/'vector-xor-verify'])


def vector_checks(source,build):
    fixtures={
        "vector_load128":"c5 f8 10 00 c3", "vector_store128":"c5 f8 11 00 c3",
        "vector_aligned_load128":"c5 f8 28 00 c3", "vector_aligned_store128":"c5 f8 29 00 c3",
        "vector_load256":"c5 fc 10 00 c3", "vector_store256":"c5 fc 11 00 c3",
        "vector_aligned_load256":"c5 fc 28 00 c3", "vector_aligned_store256":"c5 fc 29 00 c3",
        "vector_reg128":"c5 78 10 c1 c3", "vector_regstore128":"c5 78 11 c1 c3",
        "vector_reg256":"c4 41 7c 10 c1 c3", "vector_self128":"c5 f8 10 c0 c3",
        "vector_sib256":"c4 41 7c 10 7c 8a 10 c3", "vector_rip128":"c5 f8 10 05 10 00 00 00 c3",
    }
    (build/"vector-fixtures.S").write_text('\n'.join(startup_aot.translate_prefix(bytes.fromhex(code),0,name)[0] for name,code in fixtures.items()))
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         source/"guest_cpu.cpp",source/"vector_verify.cpp",build/"vector-fixtures.S","-o",build/"vector-verify"])
    run([build/"vector-verify"])


def shift_checks(source,build):
    assemblies=[]
    header=['#pragma once','#include "guest_cpu.hpp"',
            'struct ShiftFixture { void(*block)(NativePS5::GuestCpu*); unsigned bits,operation; int count; };']
    entries=[]
    def add(name,code):
        assemblies.append(startup_aot.translate_prefix(code,0,name)[0])
        header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
    for bits in (32,64):
        for operation,group in enumerate((4,5,7,0,1)):
            for count in (-1,0,1,31,32,63,64,255):
                name=f'shift_{bits}_{operation}_{count if count>=0 else "cl"}'
                code=bytes([0x48] if bits==64 else [])+bytes([0xd3 if count<0 else 0xc1,0xc0|(group<<3)])
                if count>=0:
                    code+=bytes([count])
                add(name,code+b'\xc3')
                entries.append(f'{{{name},{bits},{operation},{count}}}')
            name=f'shift_{bits}_{operation}_one'
            add(name,bytes([0x48] if bits==64 else [])+bytes([0xd1,0xc0|(group<<3),0xc3]))
            entries.append(f'{{{name},{bits},{operation},1}}')
    for name,code in {'shift_alias':'48 d3 e1 c3','shift_high':'49 d3 cb c3','shift_memory':'48 d3 3a c3'}.items():
        add(name,bytes.fromhex(code))
    header.append('inline const ShiftFixture shiftFixtures[]={'+','.join(entries)+'};')
    (build/"shift-fixtures.S").write_text('\n'.join(assemblies))
    (build/"shift-fixtures.hpp").write_text('\n'.join(header)+'\n')
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror","-I",build,"-I",source,
         source/"guest_cpu.cpp",source/"shift_verify.cpp",build/"shift-fixtures.S","-o",build/"shift-verify"])
    run([build/"shift-verify"])


def byte_checks(source,build):
    assemblies=[]
    header=['#pragma once','#include "guest_cpu.hpp"',
            'using ByteBlock=void(*)(NativePS5::GuestCpu*);',
            'struct ByteMove { ByteBlock block; unsigned source,destination; bool rex; };',
            'struct ExtensionMove { ByteBlock block; unsigned sourceBits,destinationBits,source,sourceBit,destination; bool sign,memory; };',
            'struct ImmediateMove { ByteBlock block; unsigned destination,bit; };']
    def add(name,code):
        assemblies.append(startup_aot.translate_prefix(code+b'\xc3',0,name)[0])
        header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
    moves=[]
    for rex in (False,True):
        for src in range(16 if rex else 8):
            for dst in range(16 if rex else 8):
                name=f'byte_{int(rex)}_{src}_{dst}'
                prefix=bytes([0x40|((src>>3)<<2)|(dst>>3)]) if rex else b''
                add(name,prefix+bytes([0x88,0xc0|((src&7)<<3)|(dst&7)]))
                moves.append(f'{{{name},{src},{dst},{str(rex).lower()}}}')
    extensions=[]
    for bits in (32,64):
        for opcode,sourcebits,sign in ((0xb6,8,False),(0xb7,16,False),(0xbe,8,True),(0xbf,16,True)):
            for memory in (False,True):
                name=f'extend_{bits}_{opcode}_{int(memory)}'
                prefix=bytes([0x48 if bits==64 else 0x40])
                add(name,prefix+bytes([0x0f,opcode,0x02 if memory else 0xc1]))
                extensions.append(f'{{{name},{sourcebits},{bits},1,0,0,{str(sign).lower()},{str(memory).lower()}}}')
    for name,code,sourcebits,destbits,src,sourcebit,dst,sign,memory in (
        ('extend_ah','0f b6 c4',8,32,0,8,0,False,False),
        ('extend_spl','48 0f be c4',8,64,4,0,0,True,False),
        ('extend_r15','4d 0f be c7',8,64,15,0,8,True,False),
        ('extend_r15w','45 0f bf c7',16,32,15,0,8,True,False),
        ('extend_sxd','4d 63 c7',32,64,15,0,8,True,False),
        ('extend_sxd_memory','48 63 02',32,64,0,0,0,True,True)):
        add(name,bytes.fromhex(code))
        extensions.append(f'{{{name},{sourcebits},{destbits},{src},{sourcebit},{dst},{str(sign).lower()},{str(memory).lower()}}}')
    immediates=[]
    for name,code,dst,bit in (('imm_ah','b4 80',0,8),('imm_bpl','40 b5 80',5,0),
                              ('imm_r15','41 b7 80',15,0),('imm_c6','48 c6 c0 80',0,0)):
        add(name,bytes.fromhex(code)); immediates.append(f'{{{name},{dst},{bit}}}')
    add('byte_memory_load',bytes.fromhex('8a 22'))
    add('byte_memory_store',bytes.fromhex('88 22'))
    for type_,name,entries in [('ByteMove','byteMoves',moves),('ExtensionMove','extensionMoves',extensions),('ImmediateMove','immediateMoves',immediates)]:
        header.append(f'inline const {type_} {name}[]={{'+','.join(entries)+'};')
    (build/"byte-fixtures.S").write_text('\n'.join(assemblies))
    (build/"byte-fixtures.hpp").write_text('\n'.join(header)+'\n')
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror","-I",build,"-I",source,
         source/"guest_cpu.cpp",source/"byte_verify.cpp",build/"byte-fixtures.S","-o",build/"byte-verify"])
    run([build/"byte-verify"])


def bextr_checks(source,build):
    fixtures={'bextr32':'c4 e2 70 f7 c2 c3','bextr64':'c4 e2 f0 f7 c2 c3',
              'bextr_alias':'c4 e2 70 f7 c0 c3','bextr_control_alias':'c4 e2 f8 f7 c2 c3',
              'bextr_high':'c4 42 b0 f7 c7 c3','bextr_memory':'c4 e2 f0 f7 02 c3'}
    (build/"bextr-fixtures.S").write_text('\n'.join(startup_aot.translate_prefix(bytes.fromhex(code),0,name)[0] for name,code in fixtures.items()))
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         source/"guest_cpu.cpp",source/"bextr_verify.cpp",build/"bextr-fixtures.S","-o",build/"bextr-verify"])
    run([build/"bextr-verify"])


def conditional_checks(source,build):
    assemblies=[]
    header=['#pragma once','#include "guest_cpu.hpp"','using ConditionalBlock=void(*)(NativePS5::GuestCpu*);',
            'struct ConditionalMove { ConditionalBlock block; unsigned condition,bits,destination,source; bool memory; };',
            'struct ConditionalByte { ConditionalBlock block; unsigned condition,destination,bit; bool memory; };']
    def add(name,code):
        assemblies.append(startup_aot.translate_prefix(code+b'\xc3',0,name)[0])
        header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
    moves=[]; sets=[]
    for condition in range(16):
        for bits in (32,64):
            for variant,modrm,dst,src,memory in ((0,0xc1,0,1,False),(1,0x02,0,0,True),(2,0xc7,8,15,False),(3,0xc0,0,0,False)):
                name=f'cmov_{condition}_{bits}_{variant}'
                rex=(0x48 if bits==64 else 0x40)|(5 if variant==2 else 0)
                add(name,bytes([rex,0x0f,0x40+condition,modrm]))
                moves.append(f'{{{name},{condition},{bits},{dst},{src},{str(memory).lower()}}}')
        for variant,prefix,modrm,dst,bit,memory in (
            (0,b'',0xc0,0,0,False),(1,b'',0xc4,0,8,False),(2,b'\x40',0xc4,4,0,False),
            (3,b'\x41',0xc7,15,0,False),(4,b'',0x02,0,0,True),
            (5,b'',0xfc,0,8,False),(6,b'\x44',0xf8,0,0,False)):
            name=f'setcc_{condition}_{variant}'
            add(name,prefix+bytes([0x0f,0x90+condition,modrm]))
            sets.append(f'{{{name},{condition},{dst},{bit},{str(memory).lower()}}}')
    header.append('inline const ConditionalMove conditionalMoves[]={'+','.join(moves)+'};')
    header.append('inline const ConditionalByte conditionalBytes[]={'+','.join(sets)+'};')
    (build/"conditional-fixtures.S").write_text('\n'.join(assemblies))
    (build/"conditional-fixtures.hpp").write_text('\n'.join(header)+'\n')
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror","-I",build,"-I",source,
         source/"guest_cpu.cpp",source/"conditional_verify.cpp",build/"conditional-fixtures.S","-o",build/"conditional-verify"])
    run([build/"conditional-verify"])


def logical_checks(source,build):
    assemblies=[]
    header=['#pragma once','#include "guest_cpu.hpp"','using LogicalBlock=void(*)(NativePS5::GuestCpu*);',
            'struct LogicalFixture { LogicalBlock block; unsigned bits,operation,form; uint64_t immediate; };',
            'struct BmiShiftFixture { LogicalBlock block; unsigned bits,operation; };']
    def add(name,code):
        assemblies.append(startup_aot.translate_prefix(code+b'\xc3',0,name)[0])
        header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
    fixtures=[]; shifts=[]
    for bits in (8,32,64):
        prefix=b'\x48' if bits==64 else b''
        for operation,base,group,acc in ((0,0x20,4,0x24),(1,0x08,1,0x0c),(2,0x30,6,0x34),(3,0x84,0,0xa8),(4,0,2,0)):
            for form in (range(2) if operation==4 else range(4)):
                name=f'logical_{bits}_{operation}_{form}'
                immediate=0
                if operation==4:
                    code=bytes([0xf6 if bits==8 else 0xf7,0xd0 if form==0 else 0x12])
                elif form==3:
                    immediate=0x80 if bits==8 else (0xffffffff80000080 if bits==64 else 0x80000080)
                    opcode=(0xf6 if bits==8 else 0xf7) if operation==3 else (0x80 if bits==8 else 0x81)
                    code=bytes([opcode,0xc0|(group<<3)])+(immediate&((1<<(8 if bits==8 else 32))-1)).to_bytes(1 if bits==8 else 4,'little')
                else:
                    opcode=base+(0 if bits==8 else 1)+(2 if form==2 and operation!=3 else 0)
                    code=bytes([opcode,0xc8 if form==0 else (0x0a if form==1 else 0x02)])
                add(name,prefix+code)
                fixtures.append(f'{{{name},{bits},{operation},{form},{immediate}ull}}')
                if form==3:
                    other=name+'_acc'
                    add(other,prefix+bytes([acc+(0 if bits==8 else 1)])+code[2:])
                    fixtures.append(f'{{{other},{bits},{operation},3,{immediate}ull}}')
        if bits!=8:
            for operation,pp in ((0,1),(1,3),(2,2)):
                name=f'bmi_shift_{bits}_{operation}'
                add(name,bytes([0xc4,0xe2,(0xf0 if bits==64 else 0x70)|pp,0xf7,0xc2]))
                shifts.append(f'{{{name},{bits},{operation}}}')
    add('logical_not_ah',bytes.fromhex('f6 d4'))
    header.append('inline const LogicalFixture logicalFixtures[]={'+','.join(fixtures)+'};')
    header.append('inline const BmiShiftFixture bmiShiftFixtures[]={'+','.join(shifts)+'};')
    (build/"logical-fixtures.S").write_text('\n'.join(assemblies))
    (build/"logical-fixtures.hpp").write_text('\n'.join(header)+'\n')
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror","-I",build,"-I",source,
         source/"guest_cpu.cpp",source/"logical_verify.cpp",build/"logical-fixtures.S","-o",build/"logical-verify"])
    run([build/"logical-verify"])


def accumulator_checks(source,build):
    assemblies=[];entries=[]
    header=['#pragma once','#include "guest_cpu.hpp"','struct AccumulatorFixture { void (*block)(NativePS5::GuestCpu*); unsigned bits,operation; uint64_t immediate; };']
    for bits in (8,32,64):
        for operation,opcode in enumerate((4,0x2c,0x3c)):
            for value in (range(256) if bits==8 else (0,1,15,0x7fffffff,0x80000000,0xffffffff)):
                name=f'accumulator_{bits}_{operation}_{value}'
                immediate=value if bits!=64 or value<0x80000000 else value-(1<<32)
                code=(b'\x48' if bits==64 else b'')+bytes([opcode+(bits!=8)])+value.to_bytes(1 if bits==8 else 4,'little')+b'\xc3'
                assemblies.append(startup_aot.translate_prefix(code,0,name)[0])
                header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
                entries.append(f'{{{name},{bits},{operation},{immediate&((1<<64)-1)}ull}}')
    header.append('inline const AccumulatorFixture accumulatorFixtures[]={'+','.join(entries)+'};')
    (build/'accumulator-fixtures.S').write_text('\n'.join(assemblies))
    (build/'accumulator-fixtures.hpp').write_text('\n'.join(header))
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror','-I',build,'-I',source,
         source/'guest_cpu.cpp',source/'accumulator_verify.cpp',build/'accumulator-fixtures.S','-o',build/'accumulator-verify'])
    run([build/'accumulator-verify'])


def rorx_checks(source,build):
    assemblies=[];entries=[]
    header=['#pragma once','#include "guest_cpu.hpp"','struct RorxFixture { void (*block)(NativePS5::GuestCpu*); unsigned bits,destination,source,count; bool memory; };']
    for bits in (32,64):
        for dst,src,mem in ((0,1,False),(1,1,False),(9,10,False),(0,1,True),(1,1,True)):
            for count in range(256):
                name=f'rorx_{bits}_{dst}_{src}_{int(mem)}_{count}'
                vex2=((0 if dst>=8 else 1)<<7)|0x40|((0 if src>=8 else 1)<<5)|3
                code=bytes([0xc4,vex2,0xfb if bits==64 else 0x7b,0xf0,(0 if mem else 0xc0)|((dst&7)<<3)|(src&7),count,0xc3])
                assemblies.append(startup_aot.translate_prefix(code,0,name)[0])
                header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
                entries.append(f'{{{name},{bits},{dst},{src},{count},{str(mem).lower()}}}')
    header.append('inline const RorxFixture rorxFixtures[]={'+','.join(entries)+'};')
    (build/'rorx-fixtures.S').write_text('\n'.join(assemblies))
    (build/'rorx-fixtures.hpp').write_text('\n'.join(header))
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror','-I',build,'-I',source,
         source/'guest_cpu.cpp',source/'rorx_verify.cpp',build/'rorx-fixtures.S','-o',build/'rorx-verify'])
    run([build/'rorx-verify'])


def mulx_checks(source,build):
    assemblies=[];entries=[]
    header=['#pragma once','#include "guest_cpu.hpp"','struct MulxFixture { void (*block)(NativePS5::GuestCpu*); unsigned bits,high,low,source; bool memory; };']
    for bits in (32,64):
        for high,low,src,mem in ((0,1,3,False),(0,0,0,False),(2,1,3,False),(0,2,3,False),(2,2,2,False),(3,1,3,False),(0,3,3,False),(9,10,8,False),(0,1,3,True),(3,3,3,True),(9,10,8,True)):
            name=f'mulx_{bits}_{high}_{low}_{src}_{int(mem)}'
            vex2=((0 if high>=8 else 1)<<7)|0x40|((0 if src>=8 else 1)<<5)|2
            vex3=(0x80 if bits==64 else 0)|((~low&15)<<3)|3
            code=bytes([0xc4,vex2,vex3,0xf6,(0 if mem else 0xc0)|((high&7)<<3)|(src&7),0xc3])
            assemblies.append(startup_aot.translate_prefix(code,0,name)[0])
            header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
            entries.append(f'{{{name},{bits},{high},{low},{src},{str(mem).lower()}}}')
    header.append('inline const MulxFixture mulxFixtures[]={'+','.join(entries)+'};')
    (build/'mulx-fixtures.S').write_text('\n'.join(assemblies))
    (build/'mulx-fixtures.hpp').write_text('\n'.join(header))
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror','-I',build,'-I',source,
         source/'guest_cpu.cpp',source/'mulx_verify.cpp',build/'mulx-fixtures.S','-o',build/'mulx-verify'])
    run([build/'mulx-verify'])


def andn_checks(source,build):
    assemblies=[];entries=[]
    header=['#pragma once','#include "guest_cpu.hpp"','struct AndnFixture { void (*block)(NativePS5::GuestCpu*); unsigned bits,destination,first,second; bool memory; };']
    for bits in (32,64):
        for dst,first,second,mem in ((0,1,2,False),(1,1,2,False),(2,1,2,False),(9,10,8,False),(0,1,2,True),(2,1,2,True)):
            name=f'andn_{bits}_{dst}_{first}_{second}_{int(mem)}'
            vex2=((0 if dst>=8 else 1)<<7)|0x40|((0 if second>=8 else 1)<<5)|2
            vex3=(0x80 if bits==64 else 0)|((~first&15)<<3)
            code=bytes([0xc4,vex2,vex3,0xf2,(0 if mem else 0xc0)|((dst&7)<<3)|(second&7),0xc3])
            assemblies.append(startup_aot.translate_prefix(code,0,name)[0])
            header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
            entries.append(f'{{{name},{bits},{dst},{first},{second},{str(mem).lower()}}}')
    header.append('inline const AndnFixture andnFixtures[]={'+','.join(entries)+'};')
    (build/'andn-fixtures.S').write_text('\n'.join(assemblies))
    (build/'andn-fixtures.hpp').write_text('\n'.join(header))
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror','-I',build,'-I',source,
         source/'guest_cpu.cpp',source/'andn_verify.cpp',build/'andn-fixtures.S','-o',build/'andn-verify'])
    run([build/'andn-verify'])


def movbe_checks(source,build):
    assemblies=[]; entries=[]
    header=['#pragma once','#include "guest_cpu.hpp"','struct MovbeFixture { void (*block)(NativePS5::GuestCpu*); unsigned bits; bool store; };']
    for bits in (16,32,64):
        for store in (False,True):
            name=f'movbe_{bits}_{int(store)}'
            prefix={16:b'\x66',32:b'',64:b'\x48'}[bits]
            code=prefix+bytes([0x0f,0x38,0xf1 if store else 0xf0,0x01,0xc3])
            assemblies.append(startup_aot.translate_prefix(code,0,name)[0])
            header.append(f'extern "C" void {name}(NativePS5::GuestCpu*);')
            entries.append(f'{{{name},{bits},{str(store).lower()}}}')
    header.append('inline const MovbeFixture movbeFixtures[]={'+','.join(entries)+'};')
    (build/'movbe-fixtures.S').write_text('\n'.join(assemblies))
    (build/'movbe-fixtures.hpp').write_text('\n'.join(header))
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror','-I',build,'-I',source,
         source/'guest_cpu.cpp',source/'movbe_verify.cpp',build/'movbe-fixtures.S','-o',build/'movbe-verify'])
    run([build/'movbe-verify'])


def rdna_metal_checks(source,build):
    nid=source.parents[1]/"AnyPS5/core/libs/nid"
    nid_objects=[]
    for name in ("NidCompute","Sha1"):
        target=build/(name+".o")
        # Upstream keeps an unused libraryName parameter for API compatibility.
        flags=["-Wno-unused-parameter"] if name=="NidCompute" else []
        run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",*flags,
             "-I",nid/"include","-c",nid/"src"/(name+".cpp"),"-o",target])
        nid_objects.append(target)
    decoder=source.parents[1]/"AnyPS5/core/shader/recompiler/RdnaDecoder"
    decoder_sources=[path for path in sorted((decoder/"src").glob("*.cpp")) if path.name!="RdnaDescriptorFormat.cpp"]
    if len(decoder_sources)!=8:
        raise ValueError("Expected the checked AnyPS5 RDNA decoder sources")
    common=["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror","-fobjc-arc",
         "-I",decoder/"include",source/"rdna_metal.cpp",*decoder_sources,"-framework","Foundation","-framework","Metal"]
    run([*common,source/"rdna_metal_verify.mm","-o",build/"rdna-metal-verify"])
    run([build/"rdna-metal-verify",build/"rdna-generated.metal"])
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         source/"pm4.cpp",source/"pm4_verify.cpp","-o",build/"pm4-verify"])
    run([build/"pm4-verify"])
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         source/"shader_registry.cpp",source/"shader_registry_verify.cpp","-o",build/"shader-registry-verify"])
    run([build/"shader-registry-verify"])
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         "-I",source.parents[1]/"AnyPS5/core/libs",source/"guest_cpu.cpp",source/"shader_registry.cpp",
         source/"guest_shader.cpp",source/"guest_shader_verify.cpp","-o",build/"guest-shader-verify"])
    run([build/"guest-shader-verify"])
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         source/"guest_cpu.cpp",source/"pm4.cpp",source/"guest_submission.cpp",source/"guest_submission_verify.cpp","-o",build/"guest-submission-verify"])
    run([build/"guest-submission-verify"])
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         source/"guest_cpu.cpp",source/"pm4.cpp",source/"guest_submission.cpp",source/"graphics_services.cpp",
         source/"graphics_services_verify.cpp","-o",build/"graphics-services-verify"])
    run([build/"graphics-services-verify"])
    graphics_elf=graphics_elf_fixture.build()
    (build/"graphics-call.elf").write_bytes(graphics_elf)
    (build/"sce-graphics-call.elf").write_bytes(graphics_elf_fixture.build_sce())
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         source/"elf_image.cpp",source/"sce_loader_verify.cpp","-o",build/"sce-loader-verify"])
    run([build/"sce-loader-verify",build/"sce-graphics-call.elf"])
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         "-I",nid/"include",source/"elf_image.cpp",source/"import_registry.cpp",source/"import_verify.cpp",
         *nid_objects,"-o",build/"import-verify"])
    run([build/"import-verify",build/"graphics-call.elf"])
    reachable_aot.generate(graphics_elf,build/"graphics-discovered",prefix="graphics")
    run([*common,"-I",source.parents[1]/"AnyPS5/core/libs","-I",nid/"include","-I",build,"-I",source,source/"import_registry.cpp",*nid_objects,source/"guest_cpu.cpp",source/"guest_shader.cpp",
         source/"elf_image.cpp",source/"bootstrap_services.cpp",source/"dispatcher.cpp",source/"graphics_services.cpp",build/"graphics-discovered.S",
         source/"pm4.cpp",source/"guest_submission.cpp",source/"shader_registry.cpp",source/"rdna_render_verify.mm","-framework","CoreGraphics","-framework","ImageIO","-o",build/"rdna-render-verify"])
    run([build/"rdna-render-verify",source/"rdna-triangle.png",build/"graphics-call.elf"])
    run([build/"rdna-render-verify",source/"rdna-triangle.png",build/"sce-graphics-call.elf"])


def module_metadata_checks(source,build,homebrew):
    data=homebrew.read_bytes();assemblies=[]
    for pc,size,name in ((0x212a,0x2167-0x212a,'firmware_query'),(0x380,0x39e-0x380,'metadata_syscall'),(0x2b5c,0x2b99-0x2b5c,'kernel_packet_setup')):
        _,code=startup_aot.entry_code(data,limit=size,entry=pc)
        assemblies.append(startup_aot.translate_prefix(code,pc,name,region=True,external_branches=True)[0])
    (build/'metadata-query.S').write_text('\n'.join(assemblies))
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror',source/'guest_cpu.cpp',
         source/'bootstrap_services.cpp',source/'module_metadata_verify.cpp',build/'metadata-query.S','-o',build/'module-metadata-verify'])
    run([build/'module-metadata-verify'])
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror',source/'guest_cpu.cpp',
         source/'bootstrap_services.cpp',source/'descriptors_verify.cpp',build/'metadata-query.S','-o',build/'descriptors-verify'])
    run([build/'descriptors-verify'])
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror',source/'guest_cpu.cpp',
         source/'bootstrap_services.cpp',source/'allocation_verify.cpp',build/'metadata-query.S','-o',build/'allocation-verify'])
    run([build/'allocation-verify'])
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror',source/'guest_cpu.cpp',
         source/'bootstrap_services.cpp',source/'packet_control_verify.cpp',build/'metadata-query.S','-o',build/'packet-control-verify'])
    run([build/'packet-control-verify'])
    reachable_aot.generate(data,build/'kernel-copy',prefix='kernel_copy',roots=[0x2ad0,0x2ca0,0x30e0,0x3320,0x3810],limit=20000)
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror','-I',build,'-I',source,
         source/'guest_cpu.cpp',source/'bootstrap_services.cpp',source/'dispatcher.cpp',source/'elf_image.cpp',
         source/'kernel_copy_verify.cpp',build/'kernel-copy.S','-o',build/'kernel-copy-verify'])
    run([build/'kernel-copy-verify',homebrew])


def dispatcher_fixtures(source, build, homebrew):
    assemblies=[]
    declarations=['#pragma once', '#include "dispatcher.hpp"']
    registrations={"Synthetic":[]}
    def add(group, code, pc, symbol):
        assembly,instructions=startup_aot.translate_prefix(code,pc,symbol,region=True,external_branches=True)
        assemblies.append(assembly)
        declarations.append(f'extern "C" void {symbol}(NativePS5::GuestCpu*);')
        registrations[group].extend(f'    dispatcher.Add({ins.address}, &{symbol});' for ins in instructions)
    # Native fixtures: direct call/return, both branch outcomes, bounded loop, missing callee.
    add("Synthetic",bytes.fromhex("e8 1b 00 00 00 83 f8 2a 74 66"),0x10,"dispatch_caller")
    add("Synthetic",bytes.fromhex("b8 2a 00 00 00 c3"),0x30,"dispatch_callee")
    add("Synthetic",bytes.fromhex("74 2e"),0x50,"dispatch_branch")
    add("Synthetic",bytes.fromhex("eb fe"),0x60,"dispatch_loop")
    add("Synthetic",bytes.fromhex("e8 1b 00 00 00"),0x70,"dispatch_missing")
    data=homebrew.read_bytes()
    phoff=struct.unpack_from('<Q',data,32)[0]
    phsize,phcount=struct.unpack_from('<HH',data,54)
    name=None
    for index in range(phcount):
        kind,flags,offset,address,_,filesize,_,_=struct.unpack_from('<IIQQQQQQ',data,phoff+index*phsize)
        if kind==1 and address<=0x288c20<address+filesize:
            start=offset+0x288c20-address
            end=data.index(b'\0',start,offset+filesize)
            name=data[start:end].decode('ascii')
    if name is None: raise ValueError('Missing checked homebrew NID input')
    (build/'homebrew-nid.hpp').write_text('inline constexpr char expectedHomebrewNid[]='+json.dumps(graphics_elf_fixture.nid(name))+';\n')
    reachable_aot.generate(data,build/"discovered",limit=20000)
    for group,lines in registrations.items():
        declarations.extend([f'inline void Register{group}(NativePS5::Dispatcher& dispatcher) {{',*lines,'}'])
    (build/"dispatch-regions.S").write_text("\n".join(assemblies))
    (build/"dispatch-registry.hpp").write_text("\n".join(declarations)+"\n")
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         "-I",source,"-I",build,source/"elf_image.cpp",source/"guest_cpu.cpp",source/"bootstrap_services.cpp",
         source/"dispatcher.cpp",source/"dispatcher_verify.cpp",build/"dispatch-regions.S",build/"discovered.S","-o",build/"dispatcher-verify"])
    run([build/"dispatcher-verify",homebrew])
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror','-I',build,'-I',source,
         source/'guest_cpu.cpp',source/'bootstrap_services.cpp',source/'elf_image.cpp',source/'dispatcher.cpp',
         source/'payload_boot_verify.cpp',build/'discovered.S','-o',build/'payload-boot-verify'])
    run([build/'payload-boot-verify',homebrew])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", type=pathlib.Path, required=True)
    parser.add_argument("--homebrew", type=pathlib.Path, help="Optional PS5 Crispy Doom ELF for real loader verification")
    args = parser.parse_args()
    build = args.build.resolve()
    source = pathlib.Path(__file__).resolve().parent
    prototype.verify(build)
    run(["python3", source / "test_startup_aot.py"])
    run(["python3", source / "test_reachable_aot.py"])
    run(["python3", source / "elf_translation.py", "--test"])
    code = bytes.fromhex("89 f8 01 f0 c3")
    elf = elf_translation.fixture(code)
    (build / "synthetic-x86.elf").write_bytes(elf)
    assembly = build / "elf-translated.S"
    assembly.write_text(prototype.translate(elf_translation.entry_function(elf, len(code)), "elf_add"))
    harness = build / "elf-verify.cpp"
    harness.write_text('''#include <cstdint>
#include <iostream>
extern "C" uint32_t elf_add(uint32_t, uint32_t);
int main() {
    uint32_t state = 0x13579bdf;
    for (int i = 0; i < 100000; ++i) {
        state = state * 1664525u + 1013904223u; auto a = state;
        state = state * 1664525u + 1013904223u; auto b = state;
        if (elf_add(a,b) != uint32_t(a+b)) return 1;
    }
    std::cout << "PASS: synthetic x86-64 ELF -> ARM64 Mach-O; 100000 execution checks\\n";
}
''')
    run(["xcrun", "clang++", "-arch", "arm64", "-std=c++20", "-O2", harness, assembly, "-o", build / "elf-verify"])
    run([build / "elf-verify"])
    run(["xcrun", "clang++", "-arch", "arm64", "-std=c++20", "-Wall", "-Wextra", "-Werror", "-O2",
         source / "elf_image.cpp", source / "loader_verify.cpp", "-o", build / "loader-verify"])
    loader_arguments = [build / "loader-verify"]
    if args.homebrew:
        loader_arguments.append(args.homebrew.resolve())
    run(loader_arguments)
    cpu_fixtures = {
        "cpu_add64": "48 01 c8 c3", "cpu_sub64": "48 29 c8 c3", "cpu_add32": "01 c8 c3",
        "cpu_inc64": "48 ff c0 c3", "cpu_high_registers": "4d 01 c8 c3",
        "cpu_indexed_load": "48 8b 44 8b f8 c3", "cpu_memory_add": "48 01 03 c3",
        "cpu_loop": "eb fe c3",
    }
    (build / "cpu-fixtures.S").write_text("\n".join(startup_aot.translate_prefix(bytes.fromhex(code),0,name)[0] for name,code in cpu_fixtures.items()))
    run(["xcrun", "clang++", "-arch", "arm64", "-std=c++20", "-Wall", "-Wextra", "-Werror", "-O2",
         source / "guest_cpu.cpp", source / "cpu_verify.cpp", build / "cpu-fixtures.S", "-o", build / "cpu-verify"])
    run([build / "cpu-verify"])
    run(["xcrun","clang++","-arch","arm64","-std=c++20","-O2","-Wall","-Wextra","-Werror",
         source/"guest_cpu.cpp",source/"virtual_memory_verify.cpp",build/"cpu-fixtures.S","-o",build/"virtual-memory-verify"])
    run([build/"virtual-memory-verify"])
    run(['xcrun','clang++','-arch','arm64','-std=c++20','-O2','-Wall','-Wextra','-Werror',
         source/'guest_cpu.cpp',source/'bootstrap_services.cpp',source/'format_verify.cpp','-o',build/'format-verify'])
    run([build/'format-verify'])
    vector_checks(source,build)
    vector_xor_checks(source,build)
    shift_checks(source,build)
    byte_checks(source,build)
    bextr_checks(source,build)
    conditional_checks(source,build)
    logical_checks(source,build)
    movbe_checks(source,build)
    andn_checks(source,build)
    mulx_checks(source,build)
    rorx_checks(source,build)
    accumulator_checks(source,build)
    if args.homebrew:
        run(["python3", source / "startup_aot.py", args.homebrew.resolve(), "--output", build / "startup.S", "--manifest", build / "startup.json"])
        run(["python3", source / "startup_aot.py", args.homebrew.resolve(), "--entry", "0x3a0", "--symbol", "translated_crt", "--output", build / "crt.S", "--manifest", build / "crt.json"])
        run(["xcrun", "clang++", "-arch", "arm64", "-std=c++20", "-Wall", "-Wextra", "-Werror", "-O2",
             source / "elf_image.cpp", source / "guest_cpu.cpp", source / "startup_verify.cpp", build / "startup.S", build / "crt.S", "-o", build / "startup-verify"])
        run([build / "startup-verify", args.homebrew.resolve()])
        run(["python3", source / "startup_aot.py", args.homebrew.resolve(), "--entry", "0x3a0", "--region-size", "0xa0", "--symbol", "translated_crt_region", "--output", build / "crt-region.S", "--manifest", build / "crt-region.json"])
        run(["xcrun", "clang++", "-arch", "arm64", "-std=c++20", "-Wall", "-Wextra", "-Werror", "-O2",
             source / "elf_image.cpp", source / "guest_cpu.cpp", source / "bootstrap_services.cpp", source / "service_verify.cpp",
             build / "startup.S", build / "crt-region.S", "-o", build / "service-verify"])
        run([build / "service-verify", args.homebrew.resolve()])
        dispatcher_fixtures(source,build,args.homebrew.resolve())
        module_metadata_checks(source,build,args.homebrew.resolve())
    for name, frameworks in [("metal_verify", ["Foundation", "Metal"]),
                             ("render_verify", ["Foundation", "Metal", "CoreGraphics", "ImageIO"])]:
        command = ["xcrun", "clang++", "-arch", "arm64", "-std=c++20", "-O2", "-fobjc-arc", source / (name + ".mm")]
        if name == "metal_verify":
            command.append(build / "translated.S")
        for framework in frameworks:
            command.extend(["-framework", framework])
        command.extend(["-o", build / name])
        run(command)
        arguments = [build / name]
        if name == "render_verify":
            arguments.append(source / "metal-triangle.png")
        run(arguments)
    rdna_metal_checks(source,build)
    print("All implemented milestone checks passed. PS5 game execution is not implemented.")


if __name__ == "__main__":
    main()
