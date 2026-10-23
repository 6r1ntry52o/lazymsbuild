#include "core.h"

// Excluded from the Release configurations in MyApp.Core.vcxproj.
namespace core {

int debug_only()
{
    return 1;
}

} // namespace core
