mod net;

fn main() {
    let ok = net::connect("localhost");
    println!("{ok}");
}
