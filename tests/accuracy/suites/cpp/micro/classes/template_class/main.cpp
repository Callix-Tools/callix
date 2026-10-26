template <typename T> struct Box { T get() { return T(); } };

int main() { Box<int> b; b.get(); }
