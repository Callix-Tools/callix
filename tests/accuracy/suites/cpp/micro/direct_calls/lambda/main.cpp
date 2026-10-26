void g() {}

int main() {
    auto l = [] { g(); };
    l();
}
