/* The header Redis's `src/mkreleasehdr.sh` writes at build time, as constants.
 *
 * Upstream's script fills in the git commit, a dirty flag and a build id made of
 * the host name and the clock. A release tarball has no git history, so the
 * commit is always 00000000 and the dirty flag 0, and the build id is the one
 * part that varies. It is fixed here: a build that differs from the last one
 * only in the time it was made would otherwise never be a cache hit, and
 * `redis-server --version` would differ between two builds of the same bytes.
 * `redis-server --version` and `INFO server` report these. */
#define REDIS_GIT_SHA1 "00000000"
#define REDIS_GIT_DIRTY "0"
#define REDIS_BUILD_ID "harbour"
#include "version.h"
#define REDIS_BUILD_ID_RAW REDIS_VERSION REDIS_BUILD_ID REDIS_GIT_DIRTY REDIS_GIT_SHA1
