pub fn terminaler_version() -> &'static str {
    // See build.rs
    env!("TERMINALER_CI_TAG")
}

pub fn terminaler_target_triple() -> &'static str {
    // See build.rs
    env!("TERMINALER_TARGET_TRIPLE")
}
