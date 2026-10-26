int f_one(void) { return 1; }
#define CALL(n) f_##n()

int main(void) { return CALL(one); }
