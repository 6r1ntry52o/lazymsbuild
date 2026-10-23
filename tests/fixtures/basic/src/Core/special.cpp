#include "core.h"

// Has its own PreprocessorDefinitions, so MSBuild compiles it in a separate cl.exe call.
#if SPECIAL_BUILD != 1
#error "SPECIAL_BUILD is set per file in MyApp.Core.vcxproj"
#endif

namespace core {

int special()
{
    return 3;
}

} // namespace core
