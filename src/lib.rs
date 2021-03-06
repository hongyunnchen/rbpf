// Derived from uBPF <https://github.com/iovisor/ubpf>
// Copyright 2015 Big Switch Networks, Inc
//      (uBPF: VM architecture, parts of the interpreter, originally in C)
// Copyright 2016 Quentin Monnet <quentin.monnet@6wind.com>
//      (Translation to Rust, MetaBuff/multiple classes addition, hashmaps for helpers)
//
// Licensed under the Apache License, Version 2.0 <http://www.apache.org/licenses/LICENSE-2.0> or
// the MIT license <http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.


//! Virtual machine and JIT compiler for eBPF programs.
#![doc(html_logo_url = "https://raw.githubusercontent.com/qmonnet/rbpf/master/misc/rbpf.png",
       html_favicon_url = "https://raw.githubusercontent.com/qmonnet/rbpf/master/misc/rbpf.ico")]

#![warn(missing_docs)]

use std::u32;
use std::collections::HashMap;

extern crate libc;

pub mod ebpf;
pub mod helpers;
mod verifier;
mod jit;

// A metadata buffer with two offset indications. It can be used in one kind of eBPF VM to simulate
// the use of a metadata buffer each time the program is executed, without the user having to
// actually handle it. The offsets are used to tell the VM where in the buffer the pointers to
// packet data start and end should be stored each time the program is run on a new packet.
struct MetaBuff {
    data_offset:     usize,
    data_end_offset: usize,
    buffer:          std::vec::Vec<u8>,
}

/// A virtual machine to run eBPF program. This kind of VM is used for programs expecting to work
/// on a metadata buffer containing pointers to packet data.
///
/// # Examples
///
/// ```
/// let prog = vec![
///     0x79, 0x11, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, // Load mem from mbuff at offset 8 into R1.
///     0x69, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // ldhx r1[2], r0
///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
/// ];
/// let mut mem = vec![
///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0xdd
/// ];
///
/// // Just for the example we create our metadata buffer from scratch, and we store the pointers
/// // to packet data start and end in it.
/// let mut mbuff = vec![0u8; 32];
/// unsafe {
///     let mut data     = mbuff.as_ptr().offset(8)  as *mut u64;
///     let mut data_end = mbuff.as_ptr().offset(24) as *mut u64;
///     *data     = mem.as_ptr() as u64;
///     *data_end = mem.as_ptr() as u64 + mem.len() as u64;
/// }
///
/// // Instantiate a VM.
/// let mut vm = rbpf::EbpfVmMbuff::new(&prog);
///
/// // Provide both a reference to the packet data, and to the metadata buffer.
/// let res = vm.prog_exec(&mut mem, &mut mbuff);
/// assert_eq!(res, 0x2211);
/// ```
pub struct EbpfVmMbuff<'a> {
    prog:    &'a std::vec::Vec<u8>,
    jit:     (fn (*mut u8, usize, *mut u8, usize, usize, usize) -> u64),
    helpers: HashMap<u32, fn (u64, u64, u64, u64, u64) -> u64>,
}

// Runs on packet data, with a metadata buffer
impl<'a> EbpfVmMbuff<'a> {

    /// Create a new virtual machine instance, and load an eBPF program into that instance.
    /// When attempting to load the program, it passes through a simple verifier.
    ///
    /// # Panics
    ///
    /// The simple verifier may panic if it finds errors in the eBPF program at load time.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0x79, 0x11, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, // Load mem from mbuff into R1.
    ///     0x69, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // ldhx r1[2], r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// // Instantiate a VM.
    /// let mut vm = rbpf::EbpfVmMbuff::new(&prog);
    /// ```
    pub fn new(prog: &'a std::vec::Vec<u8>) -> EbpfVmMbuff<'a> {
        verifier::check(prog);

        #[allow(unused_variables)]
        fn no_jit(foo: *mut u8, foo_len: usize, bar: *mut u8, bar_len: usize,
                  nodata_offset: usize, nodata_end_offset: usize) -> u64 {
            panic!("Error: program has not been JIT-compiled");
        }

        EbpfVmMbuff {
            prog:    prog,
            jit:     no_jit,
            helpers: HashMap::new(),
        }
    }

    /// Load a new eBPF program into the virtual machine instance.
    ///
    /// # Panics
    ///
    /// The simple verifier may panic if it finds errors in the eBPF program at load time.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog1 = vec![
    ///     0xb7, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    /// let prog2 = vec![
    ///     0x79, 0x11, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, // Load mem from mbuff into R1.
    ///     0x69, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // ldhx r1[2], r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// // Instantiate a VM.
    /// let mut vm = rbpf::EbpfVmMbuff::new(&prog1);
    /// vm.set_prog(&prog2);
    /// ```
    pub fn set_prog(&mut self, prog: &'a std::vec::Vec<u8>) {
        verifier::check(prog);
        self.prog = prog;
    }

    /// Register a built-in or user-defined helper function in order to use it later from within
    /// the eBPF program. The helper is registered into a hashmap, so the `key` can be any `u32`.
    ///
    /// If using JIT-compiled eBPF programs, be sure to register all helpers before compiling the
    /// program. You should be able to change registered helpers after compiling, but not to add
    /// new ones (i.e. with new keys).
    ///
    /// # Examples
    ///
    /// ```
    /// use rbpf::helpers;
    ///
    /// // This program was compiled with clang, from a C program containing the following single
    /// // instruction: `return bpf_trace_printk("foo %c %c %c\n", 10, 1, 2, 3);`
    /// let prog = vec![
    ///     0x18, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // load 0 as u64 into r1 (That would be
    ///     0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // replaced by tc by the address of
    ///                                                     // the format string, in the .map
    ///                                                     // section of the ELF file).
    ///     0xb7, 0x02, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, // mov r2, 10
    ///     0xb7, 0x03, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // mov r3, 1
    ///     0xb7, 0x04, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, // mov r4, 2
    ///     0xb7, 0x05, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, // mov r5, 3
    ///     0x85, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, // call helper with key 6
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// // Instantiate a VM.
    /// let mut vm = rbpf::EbpfVmMbuff::new(&prog);
    ///
    /// // Register a helper.
    /// // On running the program this helper will print the content of registers r3, r4 and r5 to
    /// // standard output.
    /// vm.register_helper(6, helpers::bpf_trace_printf);
    /// ```
    pub fn register_helper(&mut self, key: u32, function: fn (u64, u64, u64, u64, u64) -> u64) {
        self.helpers.insert(key, function);
    }

    /// Execute the program loaded, with the given packet data and metadata buffer.
    ///
    /// If the program is made to be compatible with Linux kernel, it is expected to load the
    /// address of the beginning and of the end of the memory area used for packet data from the
    /// metadata buffer, at some appointed offsets. It is up to the user to ensure that these
    /// pointers are correctly stored in the buffer.
    ///
    /// # Panics
    ///
    /// This function is currently expected to panic if it encounters any error during the program
    /// execution, such as out of bounds accesses or division by zero attempts. This may be changed
    /// in the future (we could raise errors instead).
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0x79, 0x11, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, // Load mem from mbuff into R1.
    ///     0x69, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // ldhx r1[2], r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    /// let mut mem = vec![
    ///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0xdd
    /// ];
    ///
    /// // Just for the example we create our metadata buffer from scratch, and we store the
    /// // pointers to packet data start and end in it.
    /// let mut mbuff = vec![0u8; 32];
    /// unsafe {
    ///     let mut data     = mbuff.as_ptr().offset(8)  as *mut u64;
    ///     let mut data_end = mbuff.as_ptr().offset(24) as *mut u64;
    ///     *data     = mem.as_ptr() as u64;
    ///     *data_end = mem.as_ptr() as u64 + mem.len() as u64;
    /// }
    ///
    /// // Instantiate a VM.
    /// let mut vm = rbpf::EbpfVmMbuff::new(&prog);
    ///
    /// // Provide both a reference to the packet data, and to the metadata buffer.
    /// let res = vm.prog_exec(&mut mem, &mut mbuff);
    /// assert_eq!(res, 0x2211);
    /// ```
    pub fn prog_exec(&self, mem: &mut std::vec::Vec<u8>, mbuff: &'a mut std::vec::Vec<u8>) -> u64 {
        const U32MAX: u64 = u32::MAX as u64;

        let stack = vec![0u8;ebpf::STACK_SIZE];

        // R1 points to beginning of memory area, R10 to stack
        let mut reg: [u64;11] = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, stack.as_ptr() as u64 + stack.len() as u64
        ];
        if mbuff.len() > 0 {
            reg[1] = mbuff.as_ptr() as u64;
        }
        else if mem.len() > 0 {
            reg[1] = mem.as_ptr() as u64;
        }

        let check_mem_load = | addr: u64, len: usize, insn_ptr: usize | {
            EbpfVmMbuff::check_mem(addr, len, "load", insn_ptr, &mbuff, &mem, &stack);
        };
        let check_mem_store = | addr: u64, len: usize, insn_ptr: usize | {
            EbpfVmMbuff::check_mem(addr, len, "store", insn_ptr, &mbuff, &mem, &stack);
        };

        // Loop on instructions
        let mut insn_ptr:usize = 0;
        while insn_ptr * ebpf::INSN_SIZE < self.prog.len() {
            let insn = ebpf::get_insn(self.prog, insn_ptr);
            insn_ptr += 1;
            let _dst    = insn.dst as usize;
            let _src    = insn.src as usize;

            match insn.opc {

                // BPF_LD class
                ebpf::LD_ABS_B   => unimplemented!(),
                ebpf::LD_ABS_H   => unimplemented!(),
                ebpf::LD_ABS_W   => unimplemented!(),
                ebpf::LD_ABS_DW  => unimplemented!(),
                ebpf::LD_IND_B   => unimplemented!(),
                ebpf::LD_IND_H   => unimplemented!(),
                ebpf::LD_IND_W   => unimplemented!(),
                ebpf::LD_IND_DW  => unimplemented!(),

                // BPF_LDX class
                ebpf::LD_DW_IMM  => {
                    let next_insn = ebpf::get_insn(self.prog, insn_ptr);
                    insn_ptr += 1;
                    reg[_dst] = ((insn.imm as u32) as u64) + ((next_insn.imm as u64) << 32);
                },
                ebpf::LD_B_REG   => reg[_dst] = unsafe {
                    let x = (reg[_src] as *const u8).offset(insn.off as isize) as *const u8;
                    check_mem_load(x as u64, 1, insn_ptr);
                    *x as u64
                },
                ebpf::LD_H_REG   => reg[_dst] = unsafe {
                    let x = (reg[_src] as *const u8).offset(insn.off as isize) as *const u16;
                    check_mem_load(x as u64, 2, insn_ptr);
                    *x as u64
                },
                ebpf::LD_W_REG   => reg[_dst] = unsafe {
                    let x = (reg[_src] as *const u8).offset(insn.off as isize) as *const u32;
                    check_mem_load(x as u64, 4, insn_ptr);
                    *x as u64
                },
                ebpf::LD_DW_REG  => reg[_dst] = unsafe {
                    let x = (reg[_src] as *const u8).offset(insn.off as isize) as *const u64;
                    check_mem_load(x as u64, 8, insn_ptr);
                    *x as u64
                },

                // BPF_ST class
                ebpf::ST_B_IMM   => unsafe {
                    let x = (reg[_dst] as *const u8).offset(insn.off as isize) as *mut u8;
                    check_mem_store(x as u64, 1, insn_ptr);
                    *x = insn.imm as u8;
                },
                ebpf::ST_H_IMM   => unsafe {
                    let x = (reg[_dst] as *const u8).offset(insn.off as isize) as *mut u16;
                    check_mem_store(x as u64, 2, insn_ptr);
                    *x = insn.imm as u16;
                },
                ebpf::ST_W_IMM   => unsafe {
                    let x = (reg[_dst] as *const u8).offset(insn.off as isize) as *mut u32;
                    check_mem_store(x as u64, 4, insn_ptr);
                    *x = insn.imm as u32;
                },
                ebpf::ST_DW_IMM  => unsafe {
                    let x = (reg[_dst] as *const u8).offset(insn.off as isize) as *mut u64;
                    check_mem_store(x as u64, 8, insn_ptr);
                    *x = insn.imm as u64;
                },

                // BPF_STX class
                ebpf::ST_B_REG   => unsafe {
                    let x = (reg[_dst] as *const u8).offset(insn.off as isize) as *mut u8;
                    check_mem_store(x as u64, 1, insn_ptr);
                    *x = reg[_src] as u8;
                },
                ebpf::ST_H_REG   => unsafe {
                    let x = (reg[_dst] as *const u8).offset(insn.off as isize) as *mut u16;
                    check_mem_store(x as u64, 2, insn_ptr);
                    *x = reg[_src] as u16;
                },
                ebpf::ST_W_REG   => unsafe {
                    let x = (reg[_dst] as *const u8).offset(insn.off as isize) as *mut u32;
                    check_mem_store(x as u64, 4, insn_ptr);
                    *x = reg[_src] as u32;
                },
                ebpf::ST_DW_REG  => unsafe {
                    let x = (reg[_dst] as *const u8).offset(insn.off as isize) as *mut u64;
                    check_mem_store(x as u64, 8, insn_ptr);
                    *x = reg[_src] as u64;
                },
                ebpf::ST_W_XADD  => unimplemented!(),
                ebpf::ST_DW_XADD => unimplemented!(),

                // BPF_ALU class
                // TODO Check how overflow works in kernel. Should we &= U32MAX all src register value
                // before we do the operation?
                // Cf ((0x11 << 32) - (0x1 << 32)) as u32 VS ((0x11 << 32) as u32 - (0x1 << 32) as u32
                ebpf::ADD32_IMM  => reg[_dst] = (reg[_dst] as i32).wrapping_add(insn.imm)         as u64, //((reg[_dst] & U32MAX) + insn.imm  as u64)     & U32MAX,
                ebpf::ADD32_REG  => reg[_dst] = (reg[_dst] as i32).wrapping_add(reg[_src] as i32) as u64, //((reg[_dst] & U32MAX) + (reg[_src] & U32MAX)) & U32MAX,
                ebpf::SUB32_IMM  => reg[_dst] = (reg[_dst] as i32).wrapping_sub(insn.imm)         as u64,
                ebpf::SUB32_REG  => reg[_dst] = (reg[_dst] as i32).wrapping_sub(reg[_src] as i32) as u64,
                ebpf::MUL32_IMM  => reg[_dst] = (reg[_dst] as i32).wrapping_mul(insn.imm)         as u64,
                ebpf::MUL32_REG  => reg[_dst] = (reg[_dst] as i32).wrapping_mul(reg[_src] as i32) as u64,
                ebpf::DIV32_IMM  => reg[_dst] = (reg[_dst] as u32 / insn.imm              as u32) as u64,
                ebpf::DIV32_REG  => {
                    if reg[_src] == 0 {
                        panic!("Error: division by 0");
                    }
                    reg[_dst] = (reg[_dst] as u32 / reg[_src] as u32) as u64;
                },
                ebpf::OR32_IMM   =>   reg[_dst] = (reg[_dst] as u32             | insn.imm  as u32) as u64,
                ebpf::OR32_REG   =>   reg[_dst] = (reg[_dst] as u32             | reg[_src] as u32) as u64,
                ebpf::AND32_IMM  =>   reg[_dst] = (reg[_dst] as u32             & insn.imm  as u32) as u64,
                ebpf::AND32_REG  =>   reg[_dst] = (reg[_dst] as u32             & reg[_src] as u32) as u64,
                ebpf::LSH32_IMM  =>   reg[_dst] = (reg[_dst] as u32).wrapping_shl(insn.imm  as u32) as u64,
                ebpf::LSH32_REG  =>   reg[_dst] = (reg[_dst] as u32).wrapping_shl(reg[_src] as u32) as u64,
                ebpf::RSH32_IMM  =>   reg[_dst] = (reg[_dst] as u32).wrapping_shr(insn.imm  as u32) as u64,
                ebpf::RSH32_REG  =>   reg[_dst] = (reg[_dst] as u32).wrapping_shr(reg[_src] as u32) as u64,
                ebpf::NEG32      => { reg[_dst] = (reg[_dst] as i32).wrapping_neg()                 as u64; reg[_dst] &= U32MAX; },
                ebpf::MOD32_IMM  =>   reg[_dst] = (reg[_dst] as u32             % insn.imm  as u32) as u64,
                ebpf::MOD32_REG  => {
                    if reg[_src] == 0 {
                        panic!("Error: division by 0");
                    }
                    reg[_dst] = (reg[_dst] as u32 % reg[_src] as u32) as u64;
                },
                ebpf::XOR32_IMM  =>   reg[_dst] = (reg[_dst] as u32             ^ insn.imm  as u32) as u64,
                ebpf::XOR32_REG  =>   reg[_dst] = (reg[_dst] as u32             ^ reg[_src] as u32) as u64,
                ebpf::MOV32_IMM  =>   reg[_dst] = insn.imm                                          as u64,
                ebpf::MOV32_REG  =>   reg[_dst] = (reg[_src] as u32)                                as u64,
                ebpf::ARSH32_IMM => { reg[_dst] = (reg[_dst] as i32).wrapping_shr(insn.imm  as u32) as u64; reg[_dst] &= U32MAX; },
                ebpf::ARSH32_REG => { reg[_dst] = (reg[_dst] as i32).wrapping_shr(reg[_src] as u32) as u64; reg[_dst] &= U32MAX; },
                ebpf::LE         => {
                    reg[_dst] = match insn.imm {
                        16 => (reg[_dst] as u16).to_le() as u64,
                        32 => (reg[_dst] as u32).to_le() as u64,
                        64 =>  reg[_dst].to_le(),
                        _  => unreachable!(),
                    };
                },
                ebpf::BE         => {
                    reg[_dst] = match insn.imm {
                        16 => (reg[_dst] as u16).to_be() as u64,
                        32 => (reg[_dst] as u32).to_be() as u64,
                        64 =>  reg[_dst].to_be(),
                        _  => unreachable!(),
                    };
                },

                // BPF_ALU64 class
                ebpf::ADD64_IMM  => reg[_dst] = reg[_dst].wrapping_add(insn.imm as u64),
                ebpf::ADD64_REG  => reg[_dst] = reg[_dst].wrapping_add(reg[_src]),
                ebpf::SUB64_IMM  => reg[_dst] = reg[_dst].wrapping_sub(insn.imm as u64),
                ebpf::SUB64_REG  => reg[_dst] = reg[_dst].wrapping_sub(reg[_src]),
                ebpf::MUL64_IMM  => reg[_dst] = reg[_dst].wrapping_mul(insn.imm as u64),
                ebpf::MUL64_REG  => reg[_dst] = reg[_dst].wrapping_mul(reg[_src]),
                ebpf::DIV64_IMM  => reg[_dst]                       /= insn.imm as u64,
                ebpf::DIV64_REG  => {
                    if reg[_src] == 0 {
                        panic!("Error: division by 0");
                    }
                    reg[_dst] /= reg[_src];
                },
                ebpf::OR64_IMM   => reg[_dst] |=  insn.imm as u64,
                ebpf::OR64_REG   => reg[_dst] |=  reg[_src],
                ebpf::AND64_IMM  => reg[_dst] &=  insn.imm as u64,
                ebpf::AND64_REG  => reg[_dst] &=  reg[_src],
                ebpf::LSH64_IMM  => reg[_dst] <<= insn.imm as u64,
                ebpf::LSH64_REG  => reg[_dst] <<= reg[_src],
                ebpf::RSH64_IMM  => reg[_dst] >>= insn.imm as u64,
                ebpf::RSH64_REG  => reg[_dst] >>= reg[_src],
                ebpf::NEG64      => reg[_dst] = -(reg[_dst] as i64) as u64,
                ebpf::MOD64_IMM  => reg[_dst] %=  insn.imm as u64,
                ebpf::MOD64_REG  => {
                    if reg[_src] == 0 {
                        panic!("Error: division by 0");
                    }
                    reg[_dst] %= reg[_src];
                },
                ebpf::XOR64_IMM  => reg[_dst] ^= insn.imm  as u64,
                ebpf::XOR64_REG  => reg[_dst] ^= reg[_src],
                ebpf::MOV64_IMM  => reg[_dst] =  insn.imm  as u64,
                ebpf::MOV64_REG  => reg[_dst] =  reg[_src],
                ebpf::ARSH64_IMM => reg[_dst] = (reg[_dst] as i64 >> insn.imm)  as u64,
                ebpf::ARSH64_REG => reg[_dst] = (reg[_dst] as i64 >> reg[_src]) as u64,

                // BPF_JMP class
                // TODO: check this actually works as expected for signed / unsigned ops
                ebpf::JA         =>                                           insn_ptr = (insn_ptr as i16 + insn.off) as usize,
                ebpf::JEQ_IMM    => if reg[_dst] == insn.imm as u64         { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JEQ_REG    => if reg[_dst] == reg[_src]               { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JGT_IMM    => if reg[_dst] >  insn.imm as u64         { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JGT_REG    => if reg[_dst] >  reg[_src]               { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JGE_IMM    => if reg[_dst] >= insn.imm as u64         { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JGE_REG    => if reg[_dst] >= reg[_src]               { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JSET_IMM   => if reg[_dst] &  insn.imm as u64 != 0    { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JSET_REG   => if reg[_dst] &  reg[_src]       != 0    { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JNE_IMM    => if reg[_dst] != insn.imm as u64         { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JNE_REG    => if reg[_dst] != reg[_src]               { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JSGT_IMM   => if reg[_dst] as i64 >  insn.imm  as i64 { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JSGT_REG   => if reg[_dst] as i64 >  reg[_src] as i64 { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JSGE_IMM   => if reg[_dst] as i64 >= insn.imm  as i64 { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                ebpf::JSGE_REG   => if reg[_dst] as i64 >= reg[_src] as i64 { insn_ptr = (insn_ptr as i16 + insn.off) as usize; },
                // Do not delegate the check to the verifier, since registered functions can be
                // changed after the program has been verified.
                ebpf::CALL       => if let Some(function) = self.helpers.get(&(insn.imm as u32)) {
                    reg[0] = function(reg[1], reg[2], reg[3], reg[4], reg[5]);
                } else {
                    panic!("Error: unknown helper function (id: {:#x})", insn.imm as u32);
                },
                ebpf::TAIL_CALL  => unimplemented!(),
                ebpf::EXIT       => return reg[0],

                _                => unreachable!()
            }
        }

        return 0;
    }

    fn check_mem(addr: u64, len: usize, access_type: &str, insn_ptr: usize,
                 mbuff: &std::vec::Vec<u8>, mem: &std::vec::Vec<u8>, stack: &std::vec::Vec<u8>) {
        if mbuff.as_ptr() as u64 <= addr && addr + len as u64 <= mbuff.as_ptr() as u64 + mbuff.len() as u64 {
            return
        }
        if mem.as_ptr() as u64 <= addr && addr + len as u64 <= mem.as_ptr() as u64 + mem.len() as u64 {
            return
        }
        if stack.as_ptr() as u64 <= addr && addr + len as u64 <= stack.as_ptr() as u64 + stack.len() as u64 {
            return
        }

        panic!(
            "Error: out of bounds memory {} (insn #{:?}), addr {:#x}, size {:?}\nmbuff: {:#x}/{:#x}, mem: {:#x}/{:#x}, stack: {:#x}/{:#x}",
            access_type, insn_ptr, addr, len,
            mbuff.as_ptr() as u64, mbuff.len(),
            mem.as_ptr() as u64, mem.len(),
            stack.as_ptr() as u64, stack.len()
        );
    }

    /// JIT-compile the loaded program. No argument required for this.
    ///
    /// If using helper functions, be sure to register them into the VM before calling this
    /// function.
    ///
    /// # Panics
    ///
    /// This function panics if an error occurs during JIT-compiling, such as the occurrence of an
    /// unknown eBPF operation code.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0x79, 0x11, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, // Load mem from mbuff into R1.
    ///     0x69, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // ldhx r1[2], r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// // Instantiate a VM.
    /// let mut vm = rbpf::EbpfVmMbuff::new(&prog);
    ///
    /// vm.jit_compile();
    /// ```
    pub fn jit_compile(&mut self) {
        self.jit = jit::compile(&self.prog, &self.helpers, true, false);
    }

    /// Execute the previously JIT-compiled program, with the given packet data and metadata
    /// buffer, in a manner very similar to `prog_exec()`.
    ///
    /// If the program is made to be compatible with Linux kernel, it is expected to load the
    /// address of the beginning and of the end of the memory area used for packet data from the
    /// metadata buffer, at some appointed offsets. It is up to the user to ensure that these
    /// pointers are correctly stored in the buffer.
    ///
    /// # Panics
    ///
    /// This function panics if an error occurs during the execution of the program.
    ///
    /// **WARNING:** JIT-compiled assembly code is not safe, in particular there is no runtime
    /// check for memory access; so if the eBPF program attempts erroneous accesses, this may end
    /// very bad (program may segfault). It may be wise to check that the program works with the
    /// interpreter before running the JIT-compiled version of it.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0x79, 0x11, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, // Load mem from mbuff into r1.
    ///     0x69, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // ldhx r1[2], r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    /// let mut mem = vec![
    ///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0xdd
    /// ];
    ///
    /// // Just for the example we create our metadata buffer from scratch, and we store the
    /// // pointers to packet data start and end in it.
    /// let mut mbuff = vec![0u8; 32];
    /// unsafe {
    ///     let mut data     = mbuff.as_ptr().offset(8)  as *mut u64;
    ///     let mut data_end = mbuff.as_ptr().offset(24) as *mut u64;
    ///     *data     = mem.as_ptr() as u64;
    ///     *data_end = mem.as_ptr() as u64 + mem.len() as u64;
    /// }
    ///
    /// // Instantiate a VM.
    /// let mut vm = rbpf::EbpfVmMbuff::new(&prog);
    ///
    /// vm.jit_compile();
    ///
    /// // Provide both a reference to the packet data, and to the metadata buffer.
    /// let res = vm.prog_exec_jit(&mut mem, &mut mbuff);
    /// assert_eq!(res, 0x2211);
    /// ```
    pub fn prog_exec_jit(&self, mem: &mut std::vec::Vec<u8>, mbuff: &'a mut std::vec::Vec<u8>) -> u64 {
        // If packet data is empty, do not send the address of an empty vector; send a null
        // pointer (zero value) as first argument instead, as this is uBPF's behavior (empty
        // packet should not happen in the kernel; anyway the verifier would prevent the use of
        // uninitialized registers). See `mul_loop` test.
        let mem_ptr = match mem.len() {
            0 => 0 as *mut u8,
            _ => mem.as_ptr() as *mut u8
        };
        // The last two arguments are not used in this function. They would be used if there was a
        // need to indicate to the JIT at which offset in the mbuff mem_ptr and mem_ptr + mem.len()
        // should be stored; this is what happens with struct EbpfVmFixedMbuff.
        (self.jit)(mbuff.as_ptr() as *mut u8, mbuff.len(), mem_ptr, mem.len(), 0, 0)
    }
}

/// A virtual machine to run eBPF program. This kind of VM is used for programs expecting to work
/// on a metadata buffer containing pointers to packet data, but it internally handles the buffer
/// so as to save the effort to manually handle the metadata buffer for the user.
///
/// This struct implements a static internal buffer that is passed to the program. The user has to
/// indicate the offset values at which the eBPF program expects to find the start and the end of
/// packet data in the buffer. On calling the `prog_exec()` or `prog_exec_jit()` functions, the
/// struct automatically updates the addresses in this static buffer, at the appointed offsets, for
/// the start and the end of the packet data the program is called upon.
///
/// # Examples
///
/// This was compiled with clang from the following program, in C:
///
/// ```c
/// #include <linux/bpf.h>
/// #include "path/to/linux/samples/bpf/bpf_helpers.h"
///
/// SEC(".classifier")
/// int classifier(struct __sk_buff *skb)
/// {
///   void *data = (void *)(long)skb->data;
///   void *data_end = (void *)(long)skb->data_end;
///
///   // Check program is long enough.
///   if (data + 5 > data_end)
///     return 0;
///
///   return *((char *)data + 5);
/// }
/// ```
///
/// Some small modifications have been brought to have it work, see comments.
///
/// ```
/// let prog = vec![
///     0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
///     // Here opcode 0x61 had to be replace by 0x79 so as to load a 8-bytes long address.
///     // Also, offset 0x4c had to be replace with e.g. 0x40 so as to prevent the two pointers
///     // from overlapping in the buffer.
///     0x79, 0x12, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // load pointer to mem from r1[0x40] to r2
///     0x07, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // add r2, 5
///     // Here opcode 0x61 had to be replace by 0x79 so as to load a 8-bytes long address.
///     0x79, 0x11, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, // load ptr to mem_end from r1[0x50] to r1
///     0x2d, 0x12, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, // if r2 > r1 skip 3 instructions
///     0x71, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // load r2 (= *(mem + 5)) into r0
///     0x67, 0x00, 0x00, 0x00, 0x38, 0x00, 0x00, 0x00, // r0 >>= 56
///     0xc7, 0x00, 0x00, 0x00, 0x38, 0x00, 0x00, 0x00, // r0 <<= 56 (arsh) extend byte sign to u64
///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
/// ];
/// let mut mem1 = vec![
///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0xdd
/// ];
/// let mut mem2 = vec![
///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0x27
/// ];
///
/// // Instantiate a VM. Note that we provide the start and end offsets for mem pointers.
/// let mut vm = rbpf::EbpfVmFixedMbuff::new(&prog, 0x40, 0x50);
///
/// // Provide only a reference to the packet data. We do not manage the metadata buffer.
/// let res = vm.prog_exec(&mut mem1);
/// assert_eq!(res, 0xffffffffffffffdd);
///
/// let res = vm.prog_exec(&mut mem2);
/// assert_eq!(res, 0x27);
/// ```
pub struct EbpfVmFixedMbuff<'a> {
    parent: EbpfVmMbuff<'a>,
    mbuff:  MetaBuff,
}

impl<'a> EbpfVmFixedMbuff<'a> {

    /// Create a new virtual machine instance, and load an eBPF program into that instance.
    /// When attempting to load the program, it passes through a simple verifier.
    ///
    /// # Panics
    ///
    /// The simple verifier may panic if it finds errors in the eBPF program at load time.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    ///     0x79, 0x12, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem from r1[0x40] to r2
    ///     0x07, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // add r2, 5
    ///     0x79, 0x11, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem_end from r1[0x50] to r1
    ///     0x2d, 0x12, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, // if r2 > r1 skip 3 instructions
    ///     0x71, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // load r2 (= *(mem + 5)) into r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// // Instantiate a VM. Note that we provide the start and end offsets for mem pointers.
    /// let mut vm = rbpf::EbpfVmFixedMbuff::new(&prog, 0x40, 0x50);
    /// ```
    pub fn new(prog: &'a std::vec::Vec<u8>, data_offset: usize, data_end_offset: usize) -> EbpfVmFixedMbuff<'a> {
        let parent = EbpfVmMbuff::new(prog);
        let get_buff_len = | x: usize, y: usize | if x >= y { x + 8 } else { y + 8 };
        let buffer = vec![0u8; get_buff_len(data_offset, data_end_offset)];
        let mbuff = MetaBuff {
            data_offset:     data_offset,
            data_end_offset: data_end_offset,
            buffer:          buffer,
        };
        EbpfVmFixedMbuff {
            parent: parent,
            mbuff:  mbuff,
        }
    }

    /// Load a new eBPF program into the virtual machine instance.
    ///
    /// At the same time, load new offsets for storing pointers to start and end of packet data in
    /// the internal metadata buffer.
    ///
    /// # Panics
    ///
    /// The simple verifier may panic if it finds errors in the eBPF program at load time.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog1 = vec![
    ///     0xb7, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    /// let prog2 = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    ///     0x79, 0x12, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem from r1[0x40] to r2
    ///     0x07, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // add r2, 5
    ///     0x79, 0x11, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem_end from r1[0x50] to r1
    ///     0x2d, 0x12, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, // if r2 > r1 skip 3 instructions
    ///     0x71, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // load r2 (= *(mem + 5)) into r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut mem = vec![
    ///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0x27,
    /// ];
    ///
    /// let mut vm = rbpf::EbpfVmFixedMbuff::new(&prog1, 0, 0);
    /// vm.set_prog(&prog2, 0x40, 0x50);
    ///
    /// let res = vm.prog_exec(&mut mem);
    /// assert_eq!(res, 0x27);
    /// ```
    pub fn set_prog(&mut self, prog: &'a std::vec::Vec<u8>, data_offset: usize, data_end_offset: usize) {
        let get_buff_len = | x: usize, y: usize | if x >= y { x + 8 } else { y + 8 };
        let buffer = vec![0u8; get_buff_len(data_offset, data_end_offset)];
        self.mbuff.buffer = buffer;
        self.mbuff.data_offset = data_offset;
        self.mbuff.data_end_offset = data_end_offset;
        self.parent.set_prog(prog)
    }

    /// Register a built-in or user-defined helper function in order to use it later from within
    /// the eBPF program. The helper is registered into a hashmap, so the `key` can be any `u32`.
    ///
    /// If using JIT-compiled eBPF programs, be sure to register all helpers before compiling the
    /// program. You should be able to change registered helpers after compiling, but not to add
    /// new ones (i.e. with new keys).
    ///
    /// # Examples
    ///
    /// ```
    /// use rbpf::helpers;
    ///
    /// // This program was compiled with clang, from a C program containing the following single
    /// // instruction: `return bpf_trace_printk("foo %c %c %c\n", 10, 1, 2, 3);`
    /// let prog = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    ///     0x79, 0x12, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem from r1[0x40] to r2
    ///     0x07, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // add r2, 5
    ///     0x79, 0x11, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem_end from r1[0x50] to r1
    ///     0x2d, 0x12, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, // if r2 > r1 skip 6 instructions
    ///     0x71, 0x21, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // load r2 (= *(mem + 5)) into r1
    ///     0xb7, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r2, 0
    ///     0xb7, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r3, 0
    ///     0xb7, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r4, 0
    ///     0xb7, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r5, 0
    ///     0x85, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // call helper with key 1
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut mem = vec![
    ///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0x09,
    /// ];
    ///
    /// // Instantiate a VM.
    /// let mut vm = rbpf::EbpfVmFixedMbuff::new(&prog, 0x40, 0x50);
    ///
    /// // Register a helper. This helper will store the result of the square root of r1 into r0.
    /// vm.register_helper(1, helpers::sqrti);
    ///
    /// let res = vm.prog_exec(&mut mem);
    /// assert_eq!(res, 3);
    /// ```
    pub fn register_helper(&mut self, key: u32, function: fn (u64, u64, u64, u64, u64) -> u64) {
        self.parent.register_helper(key, function);
    }

    /// Execute the program loaded, with the given packet data.
    ///
    /// If the program is made to be compatible with Linux kernel, it is expected to load the
    /// address of the beginning and of the end of the memory area used for packet data from some
    /// metadata buffer, which in the case of this VM is handled internally. The offsets at which
    /// the addresses should be placed should have be set at the creation of the VM.
    ///
    /// # Panics
    ///
    /// This function is currently expected to panic if it encounters any error during the program
    /// execution, such as out of bounds accesses or division by zero attempts. This may be changed
    /// in the future (we could raise errors instead).
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    ///     0x79, 0x12, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem from r1[0x40] to r2
    ///     0x07, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // add r2, 5
    ///     0x79, 0x11, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem_end from r1[0x50] to r1
    ///     0x2d, 0x12, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, // if r2 > r1 skip 3 instructions
    ///     0x71, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // load r2 (= *(mem + 5)) into r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    /// let mut mem = vec![
    ///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0xdd
    /// ];
    ///
    /// // Instantiate a VM. Note that we provide the start and end offsets for mem pointers.
    /// let mut vm = rbpf::EbpfVmFixedMbuff::new(&prog, 0x40, 0x50);
    ///
    /// // Provide only a reference to the packet data. We do not manage the metadata buffer.
    /// let res = vm.prog_exec(&mut mem);
    /// assert_eq!(res, 0xdd);
    /// ```
    pub fn prog_exec(&mut self, mem: &'a mut std::vec::Vec<u8>) -> u64 {
        let l = self.mbuff.buffer.len();
        // Can this ever happen? Probably not, should be ensured at mbuff creation.
        if self.mbuff.data_offset + 8 > l || self.mbuff.data_end_offset + 8 > l {
            panic!("Error: buffer too small ({:?}), cannot use data_offset {:?} and data_end_offset {:?}",
            l, self.mbuff.data_offset, self.mbuff.data_end_offset);
        }
        unsafe {
            let mut data     = self.mbuff.buffer.as_ptr().offset(self.mbuff.data_offset as isize)     as *mut u64;
            let mut data_end = self.mbuff.buffer.as_ptr().offset(self.mbuff.data_end_offset as isize) as *mut u64;
            *data     = mem.as_ptr() as u64;
            *data_end = mem.as_ptr() as u64 + mem.len() as u64;
        }
        self.parent.prog_exec(mem, &mut self.mbuff.buffer)
    }

    /// JIT-compile the loaded program. No argument required for this.
    ///
    /// If using helper functions, be sure to register them into the VM before calling this
    /// function.
    ///
    /// # Panics
    ///
    /// This function panics if an error occurs during JIT-compiling, such as the occurrence of an
    /// unknown eBPF operation code.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    ///     0x79, 0x12, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem from r1[0x40] to r2
    ///     0x07, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // add r2, 5
    ///     0x79, 0x11, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem_end from r1[0x50] to r1
    ///     0x2d, 0x12, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, // if r2 > r1 skip 3 instructions
    ///     0x71, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // load r2 (= *(mem + 5)) into r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// // Instantiate a VM. Note that we provide the start and end offsets for mem pointers.
    /// let mut vm = rbpf::EbpfVmFixedMbuff::new(&prog, 0x40, 0x50);
    ///
    /// vm.jit_compile();
    /// ```
    pub fn jit_compile(&mut self) {
        self.parent.jit = jit::compile(&self.parent.prog, &self.parent.helpers, true, true);
    }

    /// Execute the previously JIT-compiled program, with the given packet data, in a manner very
    /// similar to `prog_exec()`.
    ///
    /// If the program is made to be compatible with Linux kernel, it is expected to load the
    /// address of the beginning and of the end of the memory area used for packet data from some
    /// metadata buffer, which in the case of this VM is handled internally. The offsets at which
    /// the addresses should be placed should have be set at the creation of the VM.
    ///
    /// # Panics
    ///
    /// This function panics if an error occurs during the execution of the program.
    ///
    /// **WARNING:** JIT-compiled assembly code is not safe, in particular there is no runtime
    /// check for memory access; so if the eBPF program attempts erroneous accesses, this may end
    /// very bad (program may segfault). It may be wise to check that the program works with the
    /// interpreter before running the JIT-compiled version of it.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    ///     0x79, 0x12, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem from r1[0x40] to r2
    ///     0x07, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // add r2, 5
    ///     0x79, 0x11, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, // load mem_end from r1[0x50] to r1
    ///     0x2d, 0x12, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, // if r2 > r1 skip 3 instructions
    ///     0x71, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // load r2 (= *(mem + 5)) into r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    /// let mut mem = vec![
    ///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0xdd
    /// ];
    ///
    /// // Instantiate a VM. Note that we provide the start and end offsets for mem pointers.
    /// let mut vm = rbpf::EbpfVmFixedMbuff::new(&prog, 0x40, 0x50);
    ///
    /// vm.jit_compile();
    ///
    /// // Provide only a reference to the packet data. We do not manage the metadata buffer.
    /// let res = vm.prog_exec_jit(&mut mem);
    /// assert_eq!(res, 0xdd);
    /// ```
    // This struct redefines the `prog_exec_jit()` function, in order to pass the offsets
    // associated with the fixed mbuff.
    pub fn prog_exec_jit(&mut self, mem: &'a mut std::vec::Vec<u8>) -> u64 {
        // If packet data is empty, do not send the address of an empty vector; send a null
        // pointer (zero value) as first argument instead, as this is uBPF's behavior (empty
        // packet should not happen in the kernel; anyway the verifier would prevent the use of
        // uninitialized registers). See `mul_loop` test.
        let mem_ptr = match mem.len() {
            0 => 0 as *mut u8,
            _ => mem.as_ptr() as *mut u8
        };
        (self.parent.jit)(self.mbuff.buffer.as_ptr() as *mut u8, self.mbuff.buffer.len(),
                          mem_ptr, mem.len(), self.mbuff.data_offset, self.mbuff.data_end_offset)
    }
}

/// A virtual machine to run eBPF program. This kind of VM is used for programs expecting to work
/// directly on the memory area representing packet data.
///
/// # Examples
///
/// ```
/// let prog = vec![
///     0x71, 0x11, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, // ldxb r1[0x04], r1
///     0x07, 0x01, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, // add r1, 0x22
///     0xbf, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, r1
///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
/// ];
/// let mut mem = vec![
///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0xdd
/// ];
///
/// // Instantiate a VM.
/// let vm = rbpf::EbpfVmRaw::new(&prog);
///
/// // Provide only a reference to the packet data.
/// let res = vm.prog_exec(&mut mem);
/// assert_eq!(res, 0x22cc);
/// ```
pub struct EbpfVmRaw<'a> {
    parent: EbpfVmMbuff<'a>,
}

impl<'a> EbpfVmRaw<'a> {

    /// Create a new virtual machine instance, and load an eBPF program into that instance.
    /// When attempting to load the program, it passes through a simple verifier.
    ///
    /// # Panics
    ///
    /// The simple verifier may panic if it finds errors in the eBPF program at load time.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0x71, 0x11, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, // ldxb r1[0x04], r1
    ///     0x07, 0x01, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, // add r1, 0x22
    ///     0xbf, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, r1
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// // Instantiate a VM.
    /// let vm = rbpf::EbpfVmRaw::new(&prog);
    /// ```
    pub fn new(prog: &'a std::vec::Vec<u8>) -> EbpfVmRaw<'a> {
        let parent = EbpfVmMbuff::new(prog);
        EbpfVmRaw {
            parent: parent,
        }
    }

    /// Load a new eBPF program into the virtual machine instance.
    ///
    /// # Panics
    ///
    /// The simple verifier may panic if it finds errors in the eBPF program at load time.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog1 = vec![
    ///     0xb7, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    /// let prog2 = vec![
    ///     0x71, 0x11, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, // ldxb r1[0x04], r1
    ///     0x07, 0x01, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, // add r1, 0x22
    ///     0xbf, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, r1
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut mem = vec![
    ///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0x27,
    /// ];
    ///
    /// let mut vm = rbpf::EbpfVmRaw::new(&prog1);
    /// vm.set_prog(&prog2);
    ///
    /// let res = vm.prog_exec(&mut mem);
    /// assert_eq!(res, 0x22cc);
    /// ```
    pub fn set_prog(&mut self, prog: &'a std::vec::Vec<u8>) {
        self.parent.set_prog(prog)
    }

    /// Register a built-in or user-defined helper function in order to use it later from within
    /// the eBPF program. The helper is registered into a hashmap, so the `key` can be any `u32`.
    ///
    /// If using JIT-compiled eBPF programs, be sure to register all helpers before compiling the
    /// program. You should be able to change registered helpers after compiling, but not to add
    /// new ones (i.e. with new keys).
    ///
    /// # Examples
    ///
    /// ```
    /// use rbpf::helpers;
    ///
    /// let prog = vec![
    ///     0x79, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // ldxdw r1, r1[0x00]
    ///     0xb7, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r2, 0
    ///     0xb7, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r3, 0
    ///     0xb7, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r4, 0
    ///     0xb7, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r5, 0
    ///     0x85, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // call helper with key 1
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut mem = vec![
    ///     0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01
    /// ];
    ///
    /// // Instantiate a VM.
    /// let mut vm = rbpf::EbpfVmRaw::new(&prog);
    ///
    /// // Register a helper. This helper will store the result of the square root of r1 into r0.
    /// vm.register_helper(1, helpers::sqrti);
    ///
    /// let res = vm.prog_exec(&mut mem);
    /// assert_eq!(res, 0x10000000);
    /// ```
    pub fn register_helper(&mut self, key: u32, function: fn (u64, u64, u64, u64, u64) -> u64) {
        self.parent.register_helper(key, function);
    }

    /// Execute the program loaded, with the given packet data.
    ///
    /// # Panics
    ///
    /// This function is currently expected to panic if it encounters any error during the program
    /// execution, such as out of bounds accesses or division by zero attempts. This may be changed
    /// in the future (we could raise errors instead).
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0x71, 0x11, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, // ldxb r1[0x04], r1
    ///     0x07, 0x01, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, // add r1, 0x22
    ///     0xbf, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, r1
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut mem = vec![
    ///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0x27
    /// ];
    ///
    /// let mut vm = rbpf::EbpfVmRaw::new(&prog);
    ///
    /// let res = vm.prog_exec(&mut mem);
    /// assert_eq!(res, 0x22cc);
    /// ```
    pub fn prog_exec(&self, mem: &'a mut std::vec::Vec<u8>) -> u64 {
        let mut mbuff = vec![];
        self.parent.prog_exec(mem, &mut mbuff)
    }

    /// JIT-compile the loaded program. No argument required for this.
    ///
    /// If using helper functions, be sure to register them into the VM before calling this
    /// function.
    ///
    /// # Panics
    ///
    /// This function panics if an error occurs during JIT-compiling, such as the occurrence of an
    /// unknown eBPF operation code.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0x71, 0x11, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, // ldxb r1[0x04], r1
    ///     0x07, 0x01, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, // add r1, 0x22
    ///     0xbf, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, r1
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut vm = rbpf::EbpfVmRaw::new(&prog);
    ///
    /// vm.jit_compile();
    /// ```
    pub fn jit_compile(&mut self) {
        self.parent.jit = jit::compile(&self.parent.prog, &self.parent.helpers, false, false);
    }

    /// Execute the previously JIT-compiled program, with the given packet data, in a manner very
    /// similar to `prog_exec()`.
    ///
    /// # Panics
    ///
    /// This function panics if an error occurs during the execution of the program.
    ///
    /// **WARNING:** JIT-compiled assembly code is not safe, in particular there is no runtime
    /// check for memory access; so if the eBPF program attempts erroneous accesses, this may end
    /// very bad (program may segfault). It may be wise to check that the program works with the
    /// interpreter before running the JIT-compiled version of it.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0x71, 0x11, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, // ldxb r1[0x04], r1
    ///     0x07, 0x01, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, // add r1, 0x22
    ///     0xbf, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, r1
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut mem = vec![
    ///     0xaa, 0xbb, 0x11, 0x22, 0xcc, 0x27
    /// ];
    ///
    /// let mut vm = rbpf::EbpfVmRaw::new(&prog);
    ///
    /// vm.jit_compile();
    ///
    /// let res = vm.prog_exec_jit(&mut mem);
    /// assert_eq!(res, 0x22cc);
    /// ```
    pub fn prog_exec_jit(&self, mem: &'a mut std::vec::Vec<u8>) -> u64 {
        let mut mbuff = vec![];
        self.parent.prog_exec_jit(mem, &mut mbuff)
    }
}

/// A virtual machine to run eBPF program. This kind of VM is used for programs that do not work
/// with any memory area—no metadata buffer, no packet data either.
///
/// # Examples
///
/// ```
/// let prog = vec![
///     0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
///     0xb7, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // mov r1, 1
///     0xb7, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, // mov r2, 2
///     0xb7, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, // mov r3, 3
///     0xb7, 0x04, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, // mov r4, 4
///     0xb7, 0x05, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // mov r5, 5
///     0xb7, 0x06, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, // mov r6, 6
///     0xb7, 0x07, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, // mov r7, 7
///     0xb7, 0x08, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, // mov r8, 8
///     0x4f, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // or r0, r5
///     0x47, 0x00, 0x00, 0x00, 0xa0, 0x00, 0x00, 0x00, // or r0, 0xa0
///     0x57, 0x00, 0x00, 0x00, 0xa3, 0x00, 0x00, 0x00, // and r0, 0xa3
///     0xb7, 0x09, 0x00, 0x00, 0x91, 0x00, 0x00, 0x00, // mov r9, 0x91
///     0x5f, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // and r0, r9
///     0x67, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, // lsh r0, 32
///     0x67, 0x00, 0x00, 0x00, 0x16, 0x00, 0x00, 0x00, // lsh r0, 22
///     0x6f, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // lsh r0, r8
///     0x77, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, // rsh r0, 32
///     0x77, 0x00, 0x00, 0x00, 0x13, 0x00, 0x00, 0x00, // rsh r0, 19
///     0x7f, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // rsh r0, r7
///     0xa7, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, // xor r0, 0x03
///     0xaf, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // xor r0, r2
///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
/// ];
///
/// // Instantiate a VM.
/// let vm = rbpf::EbpfVmNoData::new(&prog);
///
/// // Provide only a reference to the packet data.
/// let res = vm.prog_exec();
/// assert_eq!(res, 0x11);
/// ```
pub struct EbpfVmNoData<'a> {
    parent: EbpfVmRaw<'a>,
}

impl<'a> EbpfVmNoData<'a> {

    /// Create a new virtual machine instance, and load an eBPF program into that instance.
    /// When attempting to load the program, it passes through a simple verifier.
    ///
    /// # Panics
    ///
    /// The simple verifier may panic if it finds errors in the eBPF program at load time.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x11, 0x22, 0x00, 0x00, // mov r0, 0x2211
    ///     0xdc, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, // be16 r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// // Instantiate a VM.
    /// let vm = rbpf::EbpfVmNoData::new(&prog);
    /// ```
    pub fn new(prog: &'a std::vec::Vec<u8>) -> EbpfVmNoData<'a> {
        let parent = EbpfVmRaw::new(prog);
        EbpfVmNoData {
            parent: parent,
        }
    }

    /// Load a new eBPF program into the virtual machine instance.
    ///
    /// # Panics
    ///
    /// The simple verifier may panic if it finds errors in the eBPF program at load time.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog1 = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x11, 0x22, 0x00, 0x00, // mov r0, 0x2211
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    /// let prog2 = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x11, 0x22, 0x00, 0x00, // mov r0, 0x2211
    ///     0xdc, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, // be16 r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut vm = rbpf::EbpfVmNoData::new(&prog1);
    ///
    /// let res = vm.prog_exec();
    /// assert_eq!(res, 0x2211);
    ///
    /// vm.set_prog(&prog2);
    ///
    /// let res = vm.prog_exec();
    /// assert_eq!(res, 0x1122);
    /// ```
    pub fn set_prog(&mut self, prog: &'a std::vec::Vec<u8>) {
        self.parent.set_prog(prog)
    }

    /// Register a built-in or user-defined helper function in order to use it later from within
    /// the eBPF program. The helper is registered into a hashmap, so the `key` can be any `u32`.
    ///
    /// If using JIT-compiled eBPF programs, be sure to register all helpers before compiling the
    /// program. You should be able to change registered helpers after compiling, but not to add
    /// new ones (i.e. with new keys).
    ///
    /// # Examples
    ///
    /// ```
    /// use rbpf::helpers;
    ///
    /// let prog = vec![
    ///     0xb7, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // mov r1, 0x010000000
    ///     0xb7, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r2, 0
    ///     0xb7, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r3, 0
    ///     0xb7, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r4, 0
    ///     0xb7, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r5, 0
    ///     0x85, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // call helper with key 1
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut vm = rbpf::EbpfVmNoData::new(&prog);
    ///
    /// // Register a helper. This helper will store the result of the square root of r1 into r0.
    /// vm.register_helper(1, helpers::sqrti);
    ///
    /// let res = vm.prog_exec();
    /// assert_eq!(res, 0x1000);
    /// ```
    pub fn register_helper(&mut self, key: u32, function: fn (u64, u64, u64, u64, u64) -> u64) {
        self.parent.register_helper(key, function);
    }

    /// JIT-compile the loaded program. No argument required for this.
    ///
    /// If using helper functions, be sure to register them into the VM before calling this
    /// function.
    ///
    /// # Panics
    ///
    /// This function panics if an error occurs during JIT-compiling, such as the occurrence of an
    /// unknown eBPF operation code.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x11, 0x22, 0x00, 0x00, // mov r0, 0x2211
    ///     0xdc, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, // be16 r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut vm = rbpf::EbpfVmNoData::new(&prog);
    ///
    ///
    /// vm.jit_compile();
    /// ```
    pub fn jit_compile(&mut self) {
        self.parent.jit_compile();
    }

    /// Execute the program loaded, without providing pointers to any memory area whatsoever.
    ///
    /// # Panics
    ///
    /// This function is currently expected to panic if it encounters any error during the program
    /// execution, such as memory accesses or division by zero attempts. This may be changed in the
    /// future (we could raise errors instead).
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x11, 0x22, 0x00, 0x00, // mov r0, 0x2211
    ///     0xdc, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, // be16 r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let vm = rbpf::EbpfVmNoData::new(&prog);
    ///
    /// // For this kind of VM, the `prog_exec()` function needs no argument.
    /// let res = vm.prog_exec();
    /// assert_eq!(res, 0x1122);
    /// ```
    pub fn prog_exec(&self) -> u64 {
        self.parent.prog_exec(&mut vec![])
    }

    /// Execute the previously JIT-compiled program, without providing pointers to any memory area
    /// whatsoever, in a manner very similar to `prog_exec()`.
    ///
    /// # Panics
    ///
    /// This function panics if an error occurs during the execution of the program.
    ///
    /// **WARNING:** JIT-compiled assembly code is not safe, in particular there is no runtime
    /// check for memory access; so if the eBPF program attempts erroneous accesses, this may end
    /// very bad (program may segfault). It may be wise to check that the program works with the
    /// interpreter before running the JIT-compiled version of it.
    ///
    /// # Examples
    ///
    /// ```
    /// let prog = vec![
    ///     0xb7, 0x00, 0x00, 0x00, 0x11, 0x22, 0x00, 0x00, // mov r0, 0x2211
    ///     0xdc, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, // be16 r0
    ///     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00  // exit
    /// ];
    ///
    /// let mut vm = rbpf::EbpfVmNoData::new(&prog);
    ///
    /// vm.jit_compile();
    ///
    /// let res = vm.prog_exec_jit();
    /// assert_eq!(res, 0x1122);
    /// ```
    pub fn prog_exec_jit(&self) -> u64 {
        self.parent.prog_exec_jit(&mut vec![])
    }
}
