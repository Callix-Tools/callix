template <typename T> T id(T x) { return x; }

int main() { id(1); id<double>(1.0); }
