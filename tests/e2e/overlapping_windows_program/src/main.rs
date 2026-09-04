/// Prints the program IDL JSON to stdout, matching the fixture-program
/// convention. The e2e test only builds this crate; reaching main at
/// all means the overlap went undetected.
fn main() {
    println!("{}", overlapping_windows_program::PROGRAM_IDL_JSON);
}
