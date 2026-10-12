import struct
import unittest
import graphics_elf_fixture
from reachable_aot import discover, emit


def fixture(code):
    data=bytearray(graphics_elf_fixture.build())
    data[0x100:0x180]=b'\xcc'*0x80
    data[0x100:0x100+len(code)]=code
    return bytes(data)


class ReachabilityTests(unittest.TestCase):
    def test_calls_branches_and_padding(self):
        # call 0x110; cmp eax,42; je 0x110; ret; unreachable INT3; callee ret.
        data=fixture(bytes.fromhex('e8 0b 00 00 00 83 f8 2a 74 06 c3 cc cc cc cc cc c3'))
        result=discover(data)
        self.assertEqual(set(result.instructions),{0x100,0x105,0x108,0x10a,0x110})
        self.assertFalse(result.failures)
        assembly,header,manifest=emit(data,result)
        self.assertEqual(manifest['instruction_count'],5)
        self.assertIn('image.Read(',header)
        self.assertIn('dispatcher.Add(272',header)
        self.assertIn('_discovered_110',assembly)

    def test_indirect_call_continuation(self):
        data=fixture(bytes.fromhex('ff d0 90 c3'))
        result=discover(data)
        self.assertEqual(set(result.instructions),{0x100,0x102,0x103})
        self.assertEqual(emit(data,result)[2]['indirect_calls'],[0x100])

    def test_unsupported_boundary_preserved(self):
        result=discover(fixture(bytes.fromhex('90 0f 0b')))
        self.assertEqual(set(result.instructions),{0x100})
        self.assertEqual(set(result.failures),{0x101})
        self.assertIn('Unsupported',result.failures[0x101])

    def test_loop_and_long_jump(self):
        result=discover(fixture(bytes.fromhex('e9 fb ff ff ff')))
        self.assertEqual(result.edges,{0x100:[0x100]})
        self.assertEqual(len(result.instructions),1)

    def test_overlap_both_orders(self):
        for code,roots in [(bytes.fromhex('eb ff'),None),
                           (bytes.fromhex('b8 00 00 00 00 c3'),[0x100,0x102]),
                           (bytes.fromhex('74 02 b8 00 00 00 00 c3'),None)]:
            with self.subTest(code=code),self.assertRaises(ValueError):
                discover(fixture(code),roots)

    def test_nonexecutables_and_bounds(self):
        result=discover(fixture(bytes.fromhex('e9 fb 3e 00 00')))
        self.assertEqual(set(result.failures),{0x4000})
        data=bytearray(fixture(bytes.fromhex('c3')))
        struct.pack_into('<Q',data,24,0xfff)
        data[0xfff]=0xe8
        result=discover(data)
        self.assertIn('Truncated',result.failures[0xfff])

    def test_instruction_budget_and_symbol(self):
        data=fixture(bytes.fromhex('90 90 c3'))
        with self.assertRaises(ValueError):
            discover(data,limit=2)
        with self.assertRaises(ValueError):
            emit(data,discover(data),'bad-name')


if __name__=='__main__':
    unittest.main()
