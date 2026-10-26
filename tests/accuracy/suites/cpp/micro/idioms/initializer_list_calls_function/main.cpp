int seed() { return 1; }

struct A {
    int x;
    A() : x(seed()) {}
};

int main() { A a; return 0; }
