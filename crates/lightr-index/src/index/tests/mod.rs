//! Test submodules for lightr-index. Split from the original lib.rs
//! `mod tests` and `mod r1_tests` blocks.
//! All files are #[cfg(test)]-gated; TEST_ENV_LOCK lives at crate root.

#[cfg(test)]
mod mod_tests_codec;
#[cfg(test)]
mod mod_tests_ops;
#[cfg(test)]
mod r1_tests_diff;
#[cfg(test)]
mod r1_tests_gc;
#[cfg(test)]
mod wp_img_09_tests;
