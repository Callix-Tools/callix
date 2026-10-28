struct V {
  V& operator=(const V& o) { return *this; }
};
struct Holder {
  V v_;
  V* p_;
  void set(const V& x, V* q) {
    v_ = x;
    p_ = q;
  }
};
void copy(V* out, const V& in) { *out = in; }
bool same(V* p, V* q) { return p == q; }
struct W {};
W& operator+=(W& a, const W& b) { return a; }
void count() {
  int i = 0;
  i += 1;
}
