#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tokenledger_omp_lib::run();
}
