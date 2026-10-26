struct A {
    void m() {}
    void run() { auto l = [this] { m(); }; l(); }
};

int main() { A a; a.run(); }
