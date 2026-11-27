/* The Darwin C toolchain's only Apple header (BUCK
 * cxx_tools_darwin): ring 0.17.14 includes it from include/ring-core/base.h,
 * and no runtime C source needs another. Hirsel writes it; it is not Apple's.
 * The values follow clang's selected target environment. */
#pragma once
#define TARGET_OS_MAC 1
#define TARGET_OS_OSX __is_target_os(macos)
#define TARGET_OS_IPHONE __is_target_os(ios)
#define TARGET_OS_SIMULATOR __is_target_environment(simulator)
#define TARGET_OS_EMBEDDED (TARGET_OS_IPHONE && !TARGET_OS_SIMULATOR)
