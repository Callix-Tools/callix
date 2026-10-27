int act(int x) { return x; }

typedef struct { int (*run)(int); } ops_t;

int drive(ops_t *o) { return o->run(1); }

int main(void) {
    ops_t o = { act };
    return drive(&o);
}
