pub fn run() -> bool {
    std::fs::metadata("/").is_ok()
}
