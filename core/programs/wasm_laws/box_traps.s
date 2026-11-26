# The contained laws' counted functions, in LLVM's WebAssembly assembly, so
# no optimizer folds a loop or chooses a trapping operator
# (docs/spec/foreign.md, "Contained laws").
	.functype	bump () -> ()
	.functype	guest_exit (i32) -> ()
	.functype	recurse (i32) -> (i32)
	.tabletype	__indirect_function_table, funcref
	.text

# The one table entry: a [] -> [] function that trap(5) calls as [] -> [i32].
	.type	slot_target,@function
slot_target:
	.functype	slot_target () -> ()
	end_function

	.globl	hwl_spin
	.type	hwl_spin,@function
hwl_spin:
	.functype	hwl_spin (i64) -> (i64)
	.local	i64
	call	bump
	block
	loop
	local.get	1
	local.get	0
	i64.ge_u
	br_if	1
	local.get	1
	i64.const	1
	i64.add
	local.set	1
	br	0
	end_loop
	end_block
	local.get	1
	end_function

	.globl	hwl_straight
	.type	hwl_straight,@function
hwl_straight:
	.functype	hwl_straight () -> (i32)
	call	bump
	i32.const	0
	.rept	2000
	i32.const	1
	i32.add
	.endr
	end_function

	.globl	hwl_straight_trap
	.type	hwl_straight_trap,@function
hwl_straight_trap:
	.functype	hwl_straight_trap () -> (i32)
	call	bump
	i32.const	0
	.rept	2000
	i32.const	1
	i32.add
	.endr
	unreachable
	end_function

	.globl	hwl_loop_div
	.type	hwl_loop_div,@function
hwl_loop_div:
	.functype	hwl_loop_div (i64) -> (i32)
	call	bump
	block
	loop
	local.get	0
	i64.eqz
	br_if	1
	local.get	0
	i64.const	1
	i64.sub
	local.set	0
	br	0
	end_loop
	end_block
	i32.const	1
	i32.const	0
	i32.div_u
	end_function

	.globl	hwl_fill_mem
	.type	hwl_fill_mem,@function
hwl_fill_mem:
	.functype	hwl_fill_mem (i32) -> (i32)
	call	bump
	i32.const	65536
	i32.const	171
	local.get	0
	memory.fill	0
	i32.const	65536
	i32.load8_u	0
	end_function

	.globl	hwl_tgrow
	.type	hwl_tgrow,@function
hwl_tgrow:
	.functype	hwl_tgrow (i64) -> (i32)
	.local	i32
	call	bump
	block
	loop
	local.get	0
	i64.eqz
	br_if	1
	ref.null_func
	i32.const	-1
	table.grow	__indirect_function_table
	local.set	1
	local.get	0
	i64.const	1
	i64.sub
	local.set	0
	br	0
	end_loop
	end_block
	local.get	1
	end_function

	.globl	hwl_nan32
	.type	hwl_nan32,@function
hwl_nan32:
	.functype	hwl_nan32 () -> (f32)
	call	bump
	i32.const	0x7fa00001
	f32.reinterpret_i32
	end_function

# The quotient goes through linear memory at 69632, in the second page.
	.globl	hwl_div_bits
	.type	hwl_div_bits,@function
hwl_div_bits:
	.functype	hwl_div_bits (f64, f64) -> (i64)
	call	bump
	i32.const	69632
	local.get	0
	local.get	1
	f64.div
	f64.store	0
	i32.const	69632
	i64.load	0
	end_function

# Rows 1 to 10 of FFI-14's trap table, in its order; any other k returns 0.
	.globl	hwl_trap
	.type	hwl_trap,@function
hwl_trap:
	.functype	hwl_trap (i32) -> (i32)
	call	bump
	local.get	0
	i32.const	1
	i32.eq
	if
	unreachable
	end_if
	local.get	0
	i32.const	2
	i32.eq
	if
	i32.const	-16
	i32.load	0
	drop
	end_if
	local.get	0
	i32.const	3
	i32.eq
	if
	i32.const	4
	call_indirect	__indirect_function_table, () -> ()
	end_if
	local.get	0
	i32.const	4
	i32.eq
	if
	i32.const	0
	call_indirect	__indirect_function_table, () -> ()
	end_if
	local.get	0
	i32.const	5
	i32.eq
	if
	i32.const	slot_target
	call_indirect	__indirect_function_table, () -> (i32)
	drop
	end_if
	local.get	0
	i32.const	6
	i32.eq
	if
	i32.const	-2147483648
	i32.const	-1
	i32.div_s
	drop
	end_if
	local.get	0
	i32.const	7
	i32.eq
	if
	i32.const	1
	i32.const	0
	i32.div_u
	drop
	end_if
	local.get	0
	i32.const	8
	i32.eq
	if
	i32.const	0x7fc00000
	f32.reinterpret_i32
	i32.trunc_f32_s
	drop
	end_if
	local.get	0
	i32.const	9
	i32.eq
	if
	i32.const	-1
	call	recurse
	drop
	end_if
	local.get	0
	i32.const	10
	i32.eq
	if
	i32.const	7
	call	guest_exit
	end_if
	i32.const	0
	end_function

	.globl	hwl_recurse
	.type	hwl_recurse,@function
hwl_recurse:
	.functype	hwl_recurse (i32) -> (i32)
	call	bump
	local.get	0
	call	recurse
	end_function

# Depth d in Wasm locals only: recurse(d) = recurse(d - 1) + 1, not a tail call.
	.type	recurse,@function
recurse:
	.functype	recurse (i32) -> (i32)
	local.get	0
	i32.eqz
	if	i32
	i32.const	0
	else
	local.get	0
	i32.const	1
	i32.sub
	call	recurse
	i32.const	1
	i32.add
	end_if
	end_function
