fn main() {
    let who = std::env::args().nth(1).unwrap_or_default();
    println!("{}", app_core::greeting(&who));
}
