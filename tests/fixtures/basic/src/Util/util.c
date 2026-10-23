#include <vendor.h>
#include <external.h>

#ifndef UTIL_LEGACY
#error "UTIL_LEGACY comes from Util (Legacy).vcxproj"
#endif

int util_extra(void);

int util_answer(void)
{
    return VENDOR_SDK_VERSION + EXTERNAL_SDK_VERSION + 30 + util_extra();
}
