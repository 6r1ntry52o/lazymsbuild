#include "core.h"

#ifndef CORE_COMMON
#error "CORE_COMMON comes from props/common.props"
#endif

// A clang tool that guesses the wrong target (x64 for a Win32 build) fails here.
static_assert(sizeof(void*) == FIXTURE_POINTER_SIZE, "compile command targets the wrong architecture");

namespace core {

int answer()
{
    return 40 + special() - 1;
}

} // namespace core
