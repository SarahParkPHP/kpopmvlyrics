fn main() {
    let args: Vec<String> = std::env::args().collect();
    kpopmvlyrics_lib::init_logging(&args);
    kpopmvlyrics_lib::run_with_args(kpopmvlyrics_lib::filter_app_args(args));
}
