pub fn runtime_wait() {
    // Production behavior is not part of the unit-test policy scan.
    std::thread::sleep(std::time::Duration::from_millis(1));
}

#[cfg(test)]
mod tests {
    #[test]
    fn comments_and_literals_are_not_code() {
        let example = "std::process::Command::new(\"false\")";
        // std::env::set_var("POLICY_FALSE_POSITIVE", "1");
        assert!(example.contains("Command"));
    }
}
