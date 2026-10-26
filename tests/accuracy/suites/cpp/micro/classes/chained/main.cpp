struct A {
    A& a() { return *this; }
    void b() {}
};

int main() { A x; x.a().b(); }
