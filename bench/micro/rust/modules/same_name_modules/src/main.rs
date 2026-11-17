mod a;
mod b;

use a::run as run_a;

fn main() {
    run_a();
    b::run();
}
