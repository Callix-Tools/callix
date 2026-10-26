struct A {
    void f() { this->g(); g(); }
    void g() {}
};

int main() { A a; a.f(); }
