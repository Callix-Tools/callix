int on_event(int x) { return x; }
#define HANDLER on_event

int main(void) { return HANDLER(1); }
