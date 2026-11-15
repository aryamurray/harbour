#include <lua.h>
#include <lauxlib.h>
#include <lualib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Hot loop long enough for the tracing JIT to compile it, then ask the VM
   whether it did. Correct arithmetic alone proves only the interpreter; the
   generated assembly VM and the JIT backend are what this is here to hit. */
static const char *script =
  "local s = 0\n"
  "for i = 1, 1e6 do s = s + i % 7 end\n"
  "local ffi = require('ffi')\n"
  "ffi.cdef[[ int abs(int); ]]\n"
  "local st = {jit.status()}\n"
  "return s, ffi.C.abs(-42), st[1], jit.arch, _VERSION\n";

int main(void) {
    lua_State *L = luaL_newstate();
    if (!L) { puts("FAIL: no state"); return 1; }
    luaL_openlibs(L);
    if (luaL_dostring(L, script)) { printf("FAIL: %s\n", lua_tostring(L, -1)); return 1; }
    double s = lua_tonumber(L, -5);
    int a = (int)lua_tonumber(L, -4);
    int jit_on = lua_toboolean(L, -3);
    const char *arch = lua_tostring(L, -2);
    if (s != 2999998) { printf("FAIL: loop sum %.0f\n", s); return 1; }
    if (a != 42) { printf("FAIL: ffi abs %d\n", a); return 1; }
    if (!jit_on) { puts("FAIL: JIT not enabled"); return 1; }
    const char *want = getenv("LJ_EXPECT_ARCH");
    if (want && strcmp(want, arch) != 0) {
        printf("FAIL: jit.arch is %s, expected %s\n", arch, want);
        return 1;
    }
    printf("OK LuaJIT loop=%.0f ffi=%d jit=on arch=%s\n", s, a, arch);
    lua_close(L);
    return 0;
}
