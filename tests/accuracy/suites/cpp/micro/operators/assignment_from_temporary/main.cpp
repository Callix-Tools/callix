struct T {
  T& operator=(const T& o) { return *this; }
  T& operator=(T&& o) { return *this; }
  static T make() { return T(); }
};
void g() {
  T s;
  T t;
  s = T::make();
  s = t;
}
