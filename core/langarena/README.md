# LangArena port interface

The reference is kostya/LangArena commit 3c9f84d. A port uses the unchanged
upstream JSON task row, including its checksum, parameters, iterations and optional
warmup_iterations. The default warmup count is max(iterations / 5, 1).

Put a task group in core/langarena/<module>.hirsel. Export a dispatcher with:

    pub fn dispatch(config:String,name:String,warmup:u64,iterations:u64)
        -> Result[u32,helper.Error]

Use direct if/match dispatch, not a stored table of callable constants. Prepare
the benchmark once, run warmups and measured iterations on the same benchmark
state, and return its upstream checksum. The upstream driver resets the random
generator before prepare and again after warmup; use an explicit helper.Lcg
created/reset at those same points. Do not reset accumulated results unless
the upstream task does.

The port's entry is:

    pub fn main(out:Stdout,d:Dir,argv:Args)->Result[(),Exit] waits {
        harness.main(out,d,argv,fn(c:String,n:String,w:u64,i:u64)
            ->Result[u32,helper.Error] {dispatch(c,n,w,i)})
    }

The command-line arguments are a config filename relative to the granted Dir,
then the exact task name. The harness reads the config, obtains counts and expected
checksum, calls the dispatcher, and prints:

    Base64::Encode checksum=3894061047 expected=3894061047 status=OK

A mismatch prints status=ERR with both values and returns exit status 1. Parse,
input and unknown-task errors return nonzero. No missing task is a passing run.

Build the compiler and runtime with the repository Buck targets
hirsel-core-driver-bin-hirsel-core and hirsel-core-rt[staticlib], using
--show-json-output. Then run the bootstrap on Cranelift dev and LLVM release:

    python3 core/tools/langarena.py --compiler <compiler> --runtime <archive>         --config core/langarena/tests/base64-test.json         --module tests/bootstrap --task Base64::Encode --backend both         --report /tmp/base64-native.json

For a new port change --module and --task; --config may be the complete unchanged
upstream test.js or run.js, array or object form. The Python tool builds through
product.build, uses a fresh confined host directory and checks the exact printed
checksum and process status. --helper-laws builds the bootstrap's independent
LCG/checksum controls. This is a correctness entry; slice F owns the performance
runner core/langarena/run.py and the vendored Rust twin.

tests/bootstrap.hirsel is the initial Base64::Encode witness, not the owner of the
full base64 port. Size30 encodes to40 bytes; 8192 iterations and1638 warmups add
40*9830=393200 to its u32 result. DJB2 of
"encode aaaa... to YWFh...: 393200" is3894061047, the upstream test checksum.

Use `helper.checksum_f64(x)` for upstream `checksum_f64`: it formats exactly
seven decimal places through `float.push_fixed64`, then applies `checksum_str`.
It returns `Result[u32,helper.Error]` and propagates formatting Capacity. The
bootstrap helper laws check independently derived hashes for zero, positive,
negative and fractional values, plus Capacity under a zero-byte bound.
