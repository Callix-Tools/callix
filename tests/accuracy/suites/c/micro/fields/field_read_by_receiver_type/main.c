struct cmdArg { int type; };
struct moduleValue { void *type; };
struct srv { long mstime; };
long mstime(void) { return 0; }
void *get(struct moduleValue *mv) { return mv->type; }
long now(void) { return mstime(); }

int main(void) {
    struct moduleValue v = {0};
    get(&v);
    now();
    return 0;
}
