function cb() {}
class Job {
    private f: () => void;
    constructor(f: () => void) { this.f = f; }
    run() { this.f(); }
}
new Job(cb).run();
