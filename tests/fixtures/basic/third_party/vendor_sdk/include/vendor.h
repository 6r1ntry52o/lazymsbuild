#pragma once

/* Stands in for a vendor SDK that a project adds through VC++ Directories (IncludePath),
   which MSBuild hands to cl.exe in the INCLUDE environment variable, not on the command line. */
#define VENDOR_SDK_VERSION 3
