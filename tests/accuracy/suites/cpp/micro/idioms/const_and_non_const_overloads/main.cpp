struct A {
    void f() {}
    void f() const {}
};

int main() { A a; a.f(); }
