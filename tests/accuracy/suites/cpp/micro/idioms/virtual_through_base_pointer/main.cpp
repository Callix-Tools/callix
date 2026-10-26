struct Base {
    virtual void run() {}
};
struct Derived : Base {
    void run() override {}
};

void go(Base* b) { b->run(); }

int main() {
    Derived d;
    go(&d);
    return 0;
}
