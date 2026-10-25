package app;

class Solver {
    int helper() {
        return 1;
    }

    int solve() {
        class Local {
            int go() {
                return helper();
            }
        }
        return new Local().go();
    }
}
