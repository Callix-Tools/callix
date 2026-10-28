struct State {
    State() {}
};

void helper() {}

int run() {
    struct State {
        int stats;
        State() : stats(0) { helper(); }
    };
    State state;
    return state.stats;
}

void other() { State s; }

int main() {
    run();
    other();
}
