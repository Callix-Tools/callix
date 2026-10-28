struct S {
  S& operator=(const S& o) { return *this; }
  bool operator==(const S& o) const { return true; }
  int operator[](int i) const { return i; }
};
bool operator!=(const S& a, const S& b) { return false; }
S make() { return S(); }
int f() {
  S a;
  S b = make();
  a = b;
  bool e = (a == b);
  bool n = (a != b);
  return a[0] + e + n;
}
