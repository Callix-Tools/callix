#define MOD_API
#define LUA_API extern

MOD_API int (*hook)(int);
LUA_API int (lua_next)(int);
