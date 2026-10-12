//! Standalone algorithm verification; does not execute translated guest code.
pub type Result<T> = std::result::Result<T, String>;
#[path = "../src/guest_compare.rs"]
pub mod guest_compare;
#[path = "../src/guest_search.rs"]
pub mod guest_search;
#[path = "../src/guest_sort.rs"]
pub mod guest_sort;
