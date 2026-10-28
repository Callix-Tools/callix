#define MY_EXPORT
#define GUARDED_BY(x)

class MY_EXPORT A {
 public:
  void m();
  int x GUARDED_BY(mu);
  int mu;
};

void A::m() {}

int main() { A a; a.m(); }
