void g(void) {}
struct ops { void (*fn)(void); };
static struct ops o = { .fn = g };

int main(void) { o.fn(); return 0; }
