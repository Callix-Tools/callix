// A class defined in a function body, and a nested class defined out of line.
namespace fixture {

int local_total() {
  struct Tally {
    int count;
    Tally() : count(0) {}
    void bump() { count += 1; }
  };
  Tally tally;
  tally.bump();
  return tally.count;
}

class Outer {
 public:
  class Inner;
};

class Outer::Inner {
 public:
  int v;
};

}  // namespace fixture
