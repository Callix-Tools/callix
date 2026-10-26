void g(void) {}
struct ops { void (*fn)(void); };

int main(void) { struct ops o = { g }; o.fn(); return 0; }
