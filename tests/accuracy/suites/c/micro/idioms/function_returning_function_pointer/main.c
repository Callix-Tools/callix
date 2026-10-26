int one(int x) { return x; }
typedef int (*handler_t)(int);
handler_t pick(void) { return one; }

int main(void) { return pick()(1); }
