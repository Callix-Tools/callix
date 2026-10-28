#include "api.h"

LUA_API int (lua_next)(int idx) { return idx; }

int main(void) { return lua_next(1); }
