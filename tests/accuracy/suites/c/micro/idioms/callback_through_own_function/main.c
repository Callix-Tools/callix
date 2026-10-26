void cb(void) {}
void mid(void (*f)(void)) { f(); }
void top(void (*g)(void)) { mid(g); }

int main(void) { top(cb); return 0; }
