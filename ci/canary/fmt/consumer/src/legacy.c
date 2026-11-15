/* A C translation unit in an otherwise C++ executable, so the program has both
 * languages in one link and the driver has to be the C++ one. */
#include "legacy.h"

int legacy_triple(int x) { return 3 * x; }
