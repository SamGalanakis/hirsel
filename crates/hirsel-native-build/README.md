# hirsel-native-build

A source build helper for native configuration comparisons. `hirsel-native-build SOURCE OUTPUT CONFIGURATION|all` compiles a supplied source through the ordinary native pipeline and writes standalone executables. It can select region and allocator configurations without timing compilation as program execution. Sources and result collection belong to the caller.
