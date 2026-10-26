int act(int x) { return x; }
struct ops { int (*run)(int); };

int drive(struct ops *o) { return o->run(1); }

int main(void) {
    struct ops o = { act };
    return drive(&o);
}
