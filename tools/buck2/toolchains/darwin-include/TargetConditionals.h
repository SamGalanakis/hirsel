/* The Darwin C toolchain's only Apple header (BUCK
 * cxx_tools_darwin): ring 0.17.14 includes it from include/ring-core/base.h,
 * and no runtime C source needs another. Hirsel writes it; it is not Apple's.
 * The values are those of a macOS (not iOS, not simulator) target. */
#pragma once
#define TARGET_OS_MAC 1
#define TARGET_OS_OSX 1
#define TARGET_OS_IPHONE 0
#define TARGET_OS_SIMULATOR 0
#define TARGET_OS_EMBEDDED 0
